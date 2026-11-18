#![forbid(unsafe_code)]
//! Command dispatch for the hexa2rust CLI.

use std::fs;
use std::path::Path;

use transpiler_core::ir::{Decl, Lang, Module};
use transpiler_core::Diag;

use crate::stub;

pub fn detect_lang(path: &Path) -> Option<Lang> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => Some(Lang::JavaScript),
        Some("ts") | Some("tsx") => Some(Lang::TypeScript),
        Some("py") => Some(Lang::Python),
        Some("go") => Some(Lang::Go),
        Some("java") => Some(Lang::Java),
        Some("kt") | Some("kts") => Some(Lang::Kotlin),
        _ => None,
    }
}

pub fn parse_file(src: &str, lang: Lang, file: &str) -> Result<Module, Diag> {
    match lang {
        Lang::JavaScript => frontend_javascript::parse_js(src, Lang::JavaScript, file),
        Lang::TypeScript => frontend_javascript::parse_js(src, Lang::TypeScript, file),
        Lang::Python => frontend_python::parse_python(src, file),
        Lang::Go => frontend_go::parse_go(src, file),
        Lang::Java => frontend_java::parse_java(src, file),
        Lang::Kotlin => frontend_kotlin::parse_kotlin(src, file),
    }
}

fn count_nodes(m: &Module) -> usize {
    use transpiler_core::ir::{expr_size, stmts_size};
    let mut n = 0;
    for d in &m.decls {
        match d {
            Decl::Function(f) => n += stmts_size(&f.body),
            Decl::Class(c) => {
                n += c.methods.iter().map(|m| stmts_size(&m.body)).sum::<usize>();
                if let Some(ctor) = &c.ctor {
                    n += stmts_size(&ctor.body);
                }
            }
            Decl::Global(g) => {
                n += g.init.as_ref().map(expr_size).unwrap_or(0);
            }
            Decl::TypeAlias { .. } => {}
        }
    }
    n
}

pub fn run(args: &[String]) -> i32 {
    if args.is_empty() {
        print_usage();
        return 1;
    }
    match args[0].as_str() {
        "transpile" => cmd_transpile(&args[1..]),
        "verify" => cmd_verify(&args[1..]),
        "corpus" => cmd_corpus(&args[1..]),
        "version" => {
            println!("hexa2rust 0.1.0");
            0
        }
        "help" | "--help" | "-h" => {
            print_usage();
            0
        }
        other => {
            eprintln!("unknown command: {}", other);
            print_usage();
            1
        }
    }
}

fn print_usage() {
    println!(
        "hexa2rust — Java/JavaScript/Kotlin/Python/Go/TypeScript -> SAFE Rust\n\n\
         Usage:\n  \
         hexa2rust transpile <input> [-o <output.rs>] [--stdout] [--stats]\n  \
         hexa2rust verify <input.py|.js|...> [--z3 <path>]\n  \
         hexa2rust corpus <dir> [--report <file.json>]\n\n\
         Output always starts with #![forbid(unsafe_code)]."
    );
}

fn cmd_transpile(args: &[String]) -> i32 {
    let mut input = None;
    let mut output = None;
    let mut stdout = false;
    let mut stats = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                if i + 1 < args.len() {
                    output = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--stdout" => stdout = true,
            "--stats" => stats = true,
            a if !a.starts_with('-') && input.is_none() => input = Some(a.to_string()),
            _ => {}
        }
        i += 1;
    }
    let input = match input {
        Some(p) => p,
        None => {
            eprintln!("transpile: missing input file");
            return 1;
        }
    };
    let path = Path::new(&input);
    let lang = match detect_lang(path) {
        Some(l) => l,
        None => {
            eprintln!("transpile: unrecognized file extension: {}", input);
            return 1;
        }
    };
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("transpile: cannot read {}: {}", input, e);
            return 1;
        }
    };
    let module = match parse_file(&src, lang, &input) {
        Ok(m) => m,
        Err(d) => {
            eprintln!("parse error: {}", d);
            return 2;
        }
    };
    let code = backend_rust::generate(&module);
    if stats {
        let lines = src.lines().count();
        let decls = module.decls.len();
        let nodes = count_nodes(&module);
        let imports = module.imports.len();
        println!(
            "{}: {} lines, {} decls, {} imports, {} IR nodes -> {} lines of safe Rust",
            input,
            lines,
            decls,
            imports,
            nodes,
            code.lines().count()
        );
    }
    match output {
        Some(o) => {
            if let Some(dir) = Path::new(&o).parent() {
                let _ = fs::create_dir_all(dir);
            }
            if let Err(e) = fs::write(&o, &code) {
                eprintln!("transpile: cannot write {}: {}", o, e);
                return 1;
            }
            if !stdout {
                println!("wrote {}", o);
            }
        }
        None => {
            println!("{}", code);
        }
    }
    0
}

fn cmd_verify(args: &[String]) -> i32 {
    let mut input = None;
    let mut z3 = std::env::var("Z3_BIN").unwrap_or_else(|_| "z3".to_string());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--z3" => {
                if i + 1 < args.len() {
                    z3 = args[i + 1].clone();
                    i += 1;
                }
            }
            a if !a.starts_with('-') && input.is_none() => input = Some(a.to_string()),
            _ => {}
        }
        i += 1;
    }
    let input = match input {
        Some(p) => p,
        None => {
            eprintln!("verify: missing input file");
            return 1;
        }
    };
    let path = Path::new(&input);
    let lang = match detect_lang(path) {
        Some(l) => l,
        None => {
            eprintln!("verify: unrecognized file extension");
            return 1;
        }
    };
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("verify: cannot read {}: {}", input, e);
            return 1;
        }
    };
    let module = match parse_file(&src, lang, &input) {
        Ok(m) => m,
        Err(d) => {
            eprintln!("parse error: {}", d);
            return 2;
        }
    };
    match verifier_z3::verify_module(&module, &z3) {
        Ok(report) => {
            println!("{}", report.summary());
            for c in &report.checks {
                println!("  {}", c);
            }
            0
        }
        Err(e) => {
            eprintln!("verify: {}", e);
            3
        }
    }
}

fn cmd_corpus(args: &[String]) -> i32 {
    let mut dir = None;
    let mut report_path = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--report" => {
                if i + 1 < args.len() {
                    report_path = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            a if !a.starts_with('-') && dir.is_none() => dir = Some(a.to_string()),
            _ => {}
        }
        i += 1;
    }
    let dir = match dir {
        Some(d) => d,
        None => {
            eprintln!("corpus: missing directory");
            return 1;
        }
    };
    let mut entries: Vec<_> = match fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect(),
        Err(e) => {
            eprintln!("corpus: cannot read {}: {}", dir, e);
            return 1;
        }
    };
    entries.sort();
    let mut pass = 0;
    let mut fail = 0;
    let mut lines_total = 0;
    let mut results: Vec<(String, String, usize, bool, String)> = Vec::new();
    for path in entries {
        let lang = match detect_lang(&path) {
            Some(l) => l,
            None => continue,
        };
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let src = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let lines = src.lines().count();
        lines_total += lines;
        match parse_file(&src, lang, &name) {
            Ok(module) => {
                let code = backend_rust::generate(&module);
                let ir_nodes = count_nodes(&module);
                let msg = format!(
                    "OK  lang={} lines={} decls={} ir_nodes={} rust_lines={}",
                    lang.name(),
                    lines,
                    module.decls.len(),
                    ir_nodes,
                    code.lines().count()
                );
                println!("[PASS] {} {}", name, msg);
                pass += 1;
                results.push((name, lang.name().to_string(), lines, true, msg));
            }
            Err(d) => {
                println!("[FAIL] {} {}:{}: {}", name, d.line, d.col, d.message);
                fail += 1;
                results.push((
                    name,
                    lang.name().to_string(),
                    lines,
                    false,
                    format!("{}:{}: {}", d.line, d.col, d.message),
                ));
            }
        }
    }
    println!(
        "\ncorpus: {}/{} files transpiled ({} source lines)",
        pass,
        pass + fail,
        lines_total
    );
    if let Some(rp) = report_path {
        let mut json = String::from("{\n  \"files\": [\n");
        for (i, (name, lang, lines, ok, msg)) in results.iter().enumerate() {
            json.push_str(&format!(
                "    {{\"file\": \"{}\", \"lang\": \"{}\", \"lines\": {}, \"ok\": {}, \"note\": \"{}\"}}{}",
                name,
                lang,
                lines,
                ok,
                msg.replace('"', "'").replace('\\', "/"),
                if i + 1 < results.len() { "," } else { "" }
            ));
            json.push('\n');
        }
        json.push_str(&format!(
            "  ],\n  \"pass\": {},\n  \"fail\": {},\n  \"lines\": {}\n}}",
            pass, fail, lines_total
        ));
        if let Some(dirp) = Path::new(&rp).parent() {
            let _ = fs::create_dir_all(dirp);
        }
        let _ = fs::write(&rp, json);
        println!("report: {}", rp);
    }
    let _ = stub;
    if fail == 0 {
        0
    } else {
        0 // corpus command reports, does not fail
    }
}

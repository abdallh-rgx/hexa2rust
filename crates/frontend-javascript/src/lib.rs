#![forbid(unsafe_code)]
//! Hand-written JavaScript (ES2015+) recursive-descent parser lowering to the
//! unified IR. Also used by `frontend-typescript` with `ts = true` to accept
//! type annotations, interfaces, enums and generics.

use transpiler_core::diag::{Diag, PResult};
use transpiler_core::ir::*;
use transpiler_core::lexer::*;

const JS_KEYWORDS: &[&str] = &[
    "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete",
    "do", "else", "export", "extends", "finally", "for", "function", "if", "import", "in",
    "instanceof", "new", "return", "super", "switch", "this", "throw", "try", "typeof",
    "var", "void", "while", "with", "yield", "let", "static", "enum", "await", "async", "of",
];

const TS_EXTRA_KEYWORDS: &[&str] = &[
    "interface", "type", "namespace", "declare", "abstract", "implements", "private",
    "public", "protected", "readonly", "keyof", "as", "satisfies", "is", "infer",
    "asserts", "any", "unknown", "never", "undefined",
];

const JS_PUNCTS: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "**",
    "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "??", "?.", "=>", "++", "--", "+=",
    "-=", "*=", "/=", "%=", "&=", "|=", "^=", "+", "-", "*", "/", "%", "=", "<", ">",
    "!", "~", "?", ":", ";", ",", ".", "(", ")", "[", "]", "{", "}", "&", "|", "^", "@",
];

pub fn js_lexer_config(ts: bool) -> LexerConfig {
    let mut keywords = JS_KEYWORDS.to_vec();
    if ts {
        keywords.extend_from_slice(TS_EXTRA_KEYWORDS);
    }
    LexerConfig {
        keywords,
        puncts: JS_PUNCTS.to_vec(),
        backtick: Backtick::Template,
        dollar_strings: false,
        char_literals: false,
    }
}

pub struct JsParser {
    toks: Vec<Token>,
    pos: usize,
    ts: bool,
    lang: Lang,
    file: String,
    imports: Vec<String>,
    hoisted: Vec<Decl>,
}

/// Parse a JavaScript (or TypeScript when `lang == TypeScript`) source file.
pub fn parse_js(src: &str, lang: Lang, file: &str) -> PResult<Module> {
    let ts = lang == Lang::TypeScript;
    let cfg = js_lexer_config(ts);
    let toks = Lexer::new(src, &cfg).tokenize()?;
    let mut p = JsParser {
        toks,
        pos: 0,
        ts,
        lang,
        file: file.to_string(),
        imports: vec![],
        hoisted: vec![],
    };
    p.parse_program()
}

impl JsParser {
    // ---- token helpers -------------------------------------------------
    fn cur(&self) -> &Token {
        &self.toks[self.pos.min(self.toks.len() - 1)]
    }
    fn nxt(&self, n: usize) -> &Token {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)]
    }
    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos.min(self.toks.len() - 1)].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn at_punct(&self, p: &str) -> bool {
        self.cur().is_punct(p)
    }
    fn at_kw(&self, k: &str) -> bool {
        self.cur().is_kw(k)
    }
    fn eat_punct(&mut self, p: &str) -> bool {
        if self.at_punct(p) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn eat_kw(&mut self, k: &str) -> bool {
        if self.at_kw(k) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect_punct(&mut self, p: &str) -> PResult<()> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            Err(self.err(format!("expected '{}', found '{}'", p, self.cur().text)))
        }
    }
    fn expect_ident(&mut self) -> PResult<String> {
        if self.cur().kind == Tok::Ident
            || (self.ts && self.is_contextual_ts_word(&self.cur().text))
        {
            Ok(self.bump().text)
        } else {
            Err(self.err(format!("expected identifier, found '{}'", self.cur().text)))
        }
    }
    fn is_contextual_ts_word(&self, s: &str) -> bool {
        matches!(
            s,
            "type" | "of" | "from" | "as" | "is" | "declare" | "namespace" | "abstract"
                | "satisfies" | "infer" | "asserts" | "any" | "unknown" | "never" | "undefined"
        )
    }
    fn err(&self, msg: impl Into<String>) -> Diag {
        Diag::new(self.cur().line, self.cur().col, msg.into()).at(&self.file)
    }
    fn unsupported(&self, what: &str) -> Diag {
        self.err(format!("unsupported JavaScript/TypeScript syntax: {}", what))
    }

    /// ASI-aware semicolon: `;` or a newline or `}` or EOF.
    fn expect_semi(&mut self) -> PResult<()> {
        if self.eat_punct(";") {
            return Ok(());
        }
        let c = self.cur();
        if c.kind == Tok::Eof || c.is_punct("}") {
            return Ok(());
        }
        let prev = &self.toks[self.pos.saturating_sub(1)];
        if c.line > prev.line {
            return Ok(());
        }
        Err(self.err(format!("expected ';', found '{}'", c.text)))
    }

    fn same_line_as_prev(&self) -> bool {
        let prev = &self.toks[self.pos.saturating_sub(1)];
        self.cur().line == prev.line
    }

    // ---- program -------------------------------------------------------
    fn parse_program(&mut self) -> PResult<Module> {
        let mut decls: Vec<Decl> = Vec::new();
        let mut main_body: Block = Vec::new();
        while self.cur().kind != Tok::Eof {
            if self.at_kw("import") {
                self.parse_import()?;
                continue;
            }
            if self.at_kw("export") {
                self.bump();
                if self.at_kw("default") {
                    self.bump();
                }
                if self.at_punct("{") {
                    self.skip_balanced()?;
                    self.expect_semi()?;
                    continue;
                }
                if self.at_punct("*") {
                    self.skip_balanced()?;
                    self.expect_semi()?;
                    continue;
                }
            }
            // declarations only valid at top level
            if self.at_kw("function") {
                let f = self.parse_function(None, false)?;
                decls.push(Decl::Function(f));
                continue;
            }
            if self.at_kw("async") && self.nxt(1).is_kw("function") {
                self.bump();
                let f = self.parse_function(None, true)?;
                decls.push(Decl::Function(f));
                continue;
            }
            if self.at_kw("class") || (self.ts && self.at_kw("abstract") && self.nxt(1).is_kw("class")) {
                if self.at_kw("abstract") {
                    self.bump();
                }
                let c = self.parse_class()?;
                decls.push(Decl::Class(c));
                continue;
            }
            if self.ts && self.at_kw("interface") {
                let c = self.parse_ts_interface()?;
                decls.push(Decl::Class(c));
                continue;
            }
            if self.ts && self.at_kw("enum") {
                let c = self.parse_ts_enum()?;
                decls.push(Decl::Class(c));
                continue;
            }
            if self.ts && self.at_kw("const") && self.nxt(1).is_kw("enum") {
                self.bump();
                let c = self.parse_ts_enum()?;
                decls.push(Decl::Class(c));
                continue;
            }
            if self.ts && self.at_kw("declare") {
                self.bump();
                self.skip_to_semi()?;
                continue;
            }
            if self.ts && (self.at_kw("namespace") || self.at_kw("module")) {
                return Err(self.unsupported("namespace/module"));
            }
            if self.ts && self.at_kw("type")
                && (self.nxt(1).kind == Tok::Ident || self.nxt(1).kind == Tok::Keyword)
                && self.nxt(2).is_punct("=")
            {
                self.bump();
                let name = self.expect_ident()?;
                self.bump(); // =
                self.skip_to_semi()?;
                decls.push(Decl::TypeAlias { name, ty: Ty::Any });
                continue;
            }
            if self.at_kw("var") || self.at_kw("let") || self.at_kw("const") {
                let mutable = self.cur().text != "const";
                self.bump();
                let stmts = self.parse_var_decl(mutable, false)?;
                for s in stmts {
                    match s {
                        Stmt::Local { name, ty, init, .. } => {
                            decls.push(Decl::Global(GlobalDecl {
                                name,
                                ty,
                                init,
                                mutable,
                                visibility: Visibility::Default,
                            }));
                        }
                        other => main_body.push(other), // destructuring temp stmts
                    }
                }
                continue;
            }
            if let Some(s) = self.parse_stmt(false)? {
                main_body.push(s);
            }
        }
        if !main_body.is_empty() {
            decls.push(Decl::Function(FunctionDecl::new("__main__", vec![], Ty::Any, main_body)));
        }
        let mut hoisted = std::mem::take(&mut self.hoisted);
        decls.append(&mut hoisted);
        Ok(Module {
            lang: self.lang,
            name: "module".into(),
            imports: std::mem::take(&mut self.imports),
            decls,
        })
    }

    fn skip_balanced(&mut self) -> PResult<()> {
        let mut depth = 0i32;
        loop {
            let t = self.cur().clone();
            if t.kind == Tok::Eof {
                return Err(self.err("unexpected end of file"));
            }
            if t.is_punct("{") || t.is_punct("(") || t.is_punct("[") {
                depth += 1;
            } else if t.is_punct("}") || t.is_punct(")") || t.is_punct("]") {
                depth -= 1;
                if depth == 0 {
                    self.bump();
                    return Ok(());
                }
            } else if depth == 0 {
                self.bump();
                return Ok(());
            }
            self.bump();
        }
    }

    fn parse_import(&mut self) -> PResult<()> {
        self.bump(); // import
        let mut path = String::new();
        while self.cur().kind != Tok::Eof && !self.at_punct(";") {
            if self.cur().kind == Tok::Str {
                path = self.cur().text.clone();
            }
            self.bump();
        }
        self.eat_punct(";");
        if !path.is_empty() {
            self.imports.push(path);
        }
        Ok(())
    }

    // ---- statements ----------------------------------------------------
    fn parse_stmt(&mut self, _top: bool) -> PResult<Option<Stmt>> {
        let t = self.cur().clone();
        if t.is_punct(";") {
            self.bump();
            return Ok(Some(Stmt::Pass));
        }
        if t.is_punct("{") {
            return Ok(Some(Stmt::Block(self.parse_block()?)));
        }
        if t.kind == Tok::Keyword {
            match t.text.as_str() {
                "var" | "let" | "const" => {
                    let mutable = t.text == "var" || t.text == "let";
                    self.bump();
                    let stmts = self.parse_var_decl(mutable, false)?;
                    if stmts.len() == 1 {
                        return Ok(Some(stmts.into_iter().next().unwrap()));
                    }
                    return Ok(Some(Stmt::Block(stmts)));
                }
                "function" => {
                    // nested function declaration -> local closure binding
                    let f = self.parse_function(None, false)?;
                    return Ok(Some(Stmt::Local {
                        name: f.name.clone(),
                        ty: Ty::Any,
                        init: Some(Expr::Closure {
                            params: f.params,
                            body: f.body,
                            value: None,
                        }),
                        mutable: false,
                    }));
                }
                "async" if self.nxt(1).is_kw("function") => {
                    self.bump();
                    let f = self.parse_function(None, true)?;
                    return Ok(Some(Stmt::Local {
                        name: f.name.clone(),
                        ty: Ty::Any,
                        init: Some(Expr::Closure {
                            params: f.params,
                            body: f.body,
                            value: None,
                        }),
                        mutable: false,
                    }));
                }
                "class" => {
                    // nested class: hoist ctor decl, bind local to class object
                    let c = self.parse_class()?;
                    let bind = format!("__class_{}", c.name);
                    self.hoisted.push(Decl::Class(c));
                    return Ok(Some(Stmt::Local {
                        name: bind
                            .trim_start_matches("__class_")
                            .to_string(),
                        ty: Ty::Any,
                        init: Some(Expr::call(Expr::ident(&bind), vec![])),
                        mutable: false,
                    }));
                }
                "if" => return self.parse_if(),
                "while" => {
                    self.bump();
                    self.expect_punct("(")?;
                    let cond = self.parse_expr()?;
                    self.expect_punct(")")?;
                    let body = self.parse_block()?;
                    return Ok(Some(Stmt::While { cond, body }));
                }
                "do" => {
                    self.bump();
                    let body = self.parse_block()?;
                    if !self.eat_kw("while") {
                        return Err(self.err("expected 'while' after do-block"));
                    }
                    self.expect_punct("(")?;
                    let cond = self.parse_expr()?;
                    self.expect_punct(")")?;
                    self.expect_semi()?;
                    return Ok(Some(Stmt::While {
                        cond: Expr::Bool(true),
                        body: vec![
                            Stmt::Block(body),
                            Stmt::If {
                                cond: Expr::Unary { op: UnOp::Not, e: Box::new(cond) },
                                then: vec![Stmt::Break],
                                els: vec![],
                            },
                        ],
                    }));
                }
                "for" => return self.parse_for(),
                "switch" => return self.parse_switch(),
                "try" => return self.parse_try(),
                "throw" => {
                    self.bump();
                    let e = self.parse_expr()?;
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Throw(e)));
                }
                "return" => {
                    self.bump();
                    let has_expr = !(self.cur().kind == Tok::Eof
                        || self.at_punct(";")
                        || self.at_punct("}")
                        || !self.same_line_as_prev());
                    let v = if has_expr { Some(self.parse_expr()?) } else { None };
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Return(v)));
                }
                "break" => {
                    self.bump();
                    if self.cur().kind == Tok::Ident && self.same_line_as_prev() {
                        self.bump(); // label
                    }
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Break));
                }
                "continue" => {
                    self.bump();
                    if self.cur().kind == Tok::Ident && self.same_line_as_prev() {
                        self.bump();
                    }
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Continue));
                }
                "debugger" => {
                    self.bump();
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Pass));
                }
                _ => {}
            }
        }
        let e = self.parse_expr()?;
        self.expect_semi()?;
        Ok(Some(Stmt::Expr(e)))
    }

    fn skip_to_semi(&mut self) -> PResult<()> {
        let mut depth = 0i32;
        while self.cur().kind != Tok::Eof {
            let t = self.cur().clone();
            if depth == 0 && t.is_punct(";") {
                self.bump();
                return Ok(());
            }
            if t.is_punct("{") {
                depth += 1;
            }
            if t.is_punct("}") {
                if depth == 0 {
                    return Ok(());
                }
                depth -= 1;
            }
            self.bump();
        }
        Ok(())
    }

    fn parse_block(&mut self) -> PResult<Block> {
        self.expect_punct("{")?;
        let mut b = Vec::new();
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of file, expected '}'"));
            }
            if self.eat_punct(";") {
                continue;
            }
            if let Some(s) = self.parse_stmt(false)? {
                b.push(s);
            }
        }
        self.expect_punct("}")?;
        Ok(b)
    }

    fn parse_var_decl(&mut self, mutable: bool, _no_in: bool) -> PResult<Vec<Stmt>> {
        let mut out = Vec::new();
        loop {
            let target = self.parse_binding_target()?;
            let ty = if self.ts && self.eat_punct(":") {
                self.parse_ts_type()?
            } else {
                Ty::Any
            };
            let init = if self.eat_punct("=") { Some(self.parse_assign()?) } else { None };
            match target {
                Expr::Ident(name) => {
                    out.push(Stmt::Local { name, ty, init, mutable });
                }
                pat => {
                    let tmp = format!("$destr{}", self.pos);
                    out.push(Stmt::Local {
                        name: tmp.clone(),
                        ty: Ty::Any,
                        init,
                        mutable: false,
                    });
                    out.extend(self.desugar_pattern(&pat, &Expr::Ident(tmp))?);
                }
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        Ok(out)
    }

    fn parse_binding_target(&mut self) -> PResult<Expr> {
        if self.at_punct("{") {
            return self.parse_object_pattern();
        }
        if self.at_punct("[") {
            return self.parse_array_pattern();
        }
        Ok(Expr::Ident(self.expect_ident()?))
    }

    fn parse_object_pattern(&mut self) -> PResult<Expr> {
        let mut fields = Vec::new();
        self.expect_punct("{")?;
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of object pattern"));
            }
            if self.eat_punct("...") {
                return Err(self.unsupported("object rest element"));
            }
            let key = if self.cur().kind == Tok::Ident
                || self.cur().kind == Tok::Keyword
                || self.ts && self.is_contextual_ts_word(&self.cur().text)
            {
                self.bump().text
            } else if self.cur().kind == Tok::Str {
                self.bump().text
            } else {
                return Err(self.unsupported("computed object pattern key"));
            };
            if self.eat_punct(":") {
                let sub = self.parse_binding_target()?;
                fields.push((key, sub));
            } else {
                fields.push((key.clone(), Expr::ident(&key)));
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct("}")?;
        Ok(Expr::Object(fields))
    }

    fn parse_array_pattern(&mut self) -> PResult<Expr> {
        let mut items = Vec::new();
        self.expect_punct("[")?;
        while !self.at_punct("]") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of array pattern"));
            }
            if self.eat_punct(",") {
                items.push(Expr::Undefined);
                continue;
            }
            let t = self.parse_binding_target()?;
            items.push(t);
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct("]")?;
        Ok(Expr::Array(items))
    }

    fn desugar_pattern(&mut self, pat: &Expr, value: &Expr) -> PResult<Vec<Stmt>> {
        let mut out = Vec::new();
        match pat {
            Expr::Ident(name) => {
                out.push(Stmt::Assign {
                    target: Expr::Ident(name.clone()),
                    op: None,
                    value: value.clone(),
                });
            }
            Expr::Object(fields) => {
                for (k, sub) in fields {
                    let access = Expr::field(value.clone(), k);
                    out.extend(self.desugar_pattern(sub, &access)?);
                }
            }
            Expr::Array(items) => {
                for (i, sub) in items.iter().enumerate() {
                    if matches!(sub, Expr::Undefined) {
                        continue;
                    }
                    let access = Expr::index(value.clone(), Expr::Int(i as i64));
                    out.extend(self.desugar_pattern(sub, &access)?);
                }
            }
            _ => return Err(self.unsupported("complex destructuring pattern")),
        }
        Ok(out)
    }

    fn parse_if(&mut self) -> PResult<Option<Stmt>> {
        self.bump(); // if
        self.expect_punct("(")?;
        let cond = self.parse_expr()?;
        self.expect_punct(")")?;
        let then = self.parse_stmt(false)?.unwrap_or(Stmt::Pass);
        let then = if let Stmt::Block(b) = then { b } else { vec![then] };
        let els = if self.eat_kw("else") {
            let s = self.parse_stmt(false)?.unwrap_or(Stmt::Pass);
            if let Stmt::Block(b) = s { b } else { vec![s] }
        } else {
            vec![]
        };
        Ok(Some(Stmt::If { cond, then, els }))
    }

    fn parse_for(&mut self) -> PResult<Option<Stmt>> {
        self.bump(); // for
        if self.at_kw("await") {
            self.bump();
        }
        self.expect_punct("(")?;
        let init_stmts: Vec<Stmt>;
        if self.at_punct(";") {
            init_stmts = vec![];
        } else if self.at_kw("var") || self.at_kw("let") || self.at_kw("const") {
            let mutable = self.cur().text != "const";
            self.bump();
            let decls = self.parse_var_decl(mutable, true)?;
            if self.at_kw("of") {
                self.bump();
                let iter = self.parse_assign()?;
                self.expect_punct(")")?;
                let var = first_decl_name(&decls)
                    .ok_or_else(|| self.unsupported("destructuring in for-of"))?;
                let body = self.parse_stmt(false)?.unwrap_or(Stmt::Pass);
                return Ok(Some(Stmt::ForEach {
                    var,
                    var2: None,
                    var_ty: Ty::Any,
                    iter,
                    body: blockify(body),
                }));
            }
            if self.at_kw("in") {
                self.bump();
                let iter = self.parse_assign()?;
                self.expect_punct(")")?;
                let var = first_decl_name(&decls)
                    .ok_or_else(|| self.unsupported("destructuring in for-in"))?;
                let body = self.parse_stmt(false)?.unwrap_or(Stmt::Pass);
                return Ok(Some(Stmt::ForEach {
                    var,
                    var2: None,
                    var_ty: Ty::Any,
                    iter: Expr::method(iter, "__keys__", vec![]),
                    body: blockify(body),
                }));
            }
            init_stmts = decls;
        } else {
            let e = self.parse_expr()?;
            if self.at_kw("of") || self.at_kw("in") {
                let is_of = self.at_kw("of");
                self.bump();
                let iter = self.parse_assign()?;
                self.expect_punct(")")?;
                let var = match &e {
                    Expr::Ident(n) => n.clone(),
                    _ => return Err(self.unsupported("non-declaration for-of target")),
                };
                let body = self.parse_stmt(false)?.unwrap_or(Stmt::Pass);
                let iter = if is_of {
                    iter
                } else {
                    Expr::method(iter, "__keys__", vec![])
                };
                return Ok(Some(Stmt::ForEach {
                    var,
                    var2: None,
                    var_ty: Ty::Any,
                    iter,
                    body: blockify(body),
                }));
            }
            init_stmts = vec![Stmt::Expr(e)];
        }
        self.expect_punct(";")?;
        let cond = if self.at_punct(";") { None } else { Some(self.parse_expr()?) };
        self.expect_punct(";")?;
        let mut step = Vec::new();
        if !self.at_punct(")") {
            let e = self.parse_expr()?;
            step.push(Stmt::Expr(e));
        }
        self.expect_punct(")")?;
        let body = self.parse_stmt(false)?.unwrap_or(Stmt::Pass);
        Ok(Some(Stmt::For {
            init: init_stmts,
            cond,
            step,
            body: blockify(body),
        }))
    }

    fn parse_switch(&mut self) -> PResult<Option<Stmt>> {
        self.bump();
        self.expect_punct("(")?;
        let subject = self.parse_expr()?;
        self.expect_punct(")")?;
        self.expect_punct("{")?;
        let mut arms: Vec<MatchArm> = Vec::new();
        let mut default: Option<Block> = None;
        let mut pending: Vec<Expr> = Vec::new();
        let mut pending_default = false;
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of switch"));
            }
            if self.eat_kw("case") {
                let pat = self.parse_expr()?;
                self.expect_punct(":")?;
                pending.push(pat);
                continue;
            }
            if self.eat_kw("default") {
                self.expect_punct(":")?;
                pending_default = true;
                continue;
            }
            let mut body = Vec::new();
            while !self.at_kw("case") && !self.at_kw("default") && !self.at_punct("}") {
                if self.cur().kind == Tok::Eof {
                    return Err(self.err("unexpected end of switch body"));
                }
                if let Some(s) = self.parse_stmt(false)? {
                    body.push(s);
                }
            }
            if matches!(body.last(), Some(Stmt::Break)) {
                body.pop();
            }
            let has_exit = matches!(
                body.last(),
                Some(Stmt::Return(_)) | Some(Stmt::Throw(_)) | Some(Stmt::Continue)
            );
            if !has_exit && !pending.is_empty() && !pending_default && !body.is_empty() {
                return Err(self.unsupported(
                    "switch fallthrough (case without break/return)",
                ));
            }
            if pending_default {
                default = Some(body);
                pending_default = false;
            } else if !pending.is_empty() {
                let pats = std::mem::take(&mut pending);
                arms.push(MatchArm { pattern: pats, guard: None, body });
            } else {
                if body.is_empty() {
                    continue; // dangling separators
                }
                return Err(self.err("switch statement before any case"));
            }
        }
        self.expect_punct("}")?;
        Ok(Some(Stmt::Match { subject, arms, default }))
    }

    fn parse_try(&mut self) -> PResult<Option<Stmt>> {
        self.bump();
        let body = self.parse_block()?;
        let mut catches = Vec::new();
        let mut finally = None;
        while self.at_kw("catch") {
            self.bump();
            let mut var = None;
            if self.eat_punct("(") {
                var = Some(self.expect_ident()?);
                if self.ts && self.eat_punct(":") {
                    let _ = self.parse_ts_type()?;
                }
                self.expect_punct(")")?;
            }
            let cbody = self.parse_block()?;
            catches.push(CatchClause { var, ty: None, body: cbody });
        }
        if self.eat_kw("finally") {
            finally = Some(self.parse_block()?);
        }
        Ok(Some(Stmt::Try { body, catches, finally }))
    }

    // ---- functions & classes -------------------------------------------
    fn parse_function(&mut self, name: Option<String>, is_async: bool) -> PResult<FunctionDecl> {
        self.bump(); // function
        let fname = match name {
            Some(n) => n,
            None => self.expect_ident()?,
        };
        if self.ts && self.at_punct("<") {
            self.skip_type_args()?;
        }
        if self.at_punct("*") {
            return Err(self.unsupported("generator functions"));
        }
        let (params, mut prologue) = self.parse_params()?;
        let ret = if self.ts && self.eat_punct(":") {
            self.parse_ts_type()?
        } else {
            Ty::Any
        };
        let mut body = self.parse_block()?;
        prologue.append(&mut body);
        let mut f = FunctionDecl::new(&fname, params, ret, prologue);
        f.is_async = is_async;
        Ok(f)
    }

    fn parse_params(&mut self) -> PResult<(Vec<Param>, Vec<Stmt>)> {
        let mut params = Vec::new();
        let mut prologue = Vec::new();
        self.expect_punct("(")?;
        let mut i = 0;
        while !self.at_punct(")") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of parameter list"));
            }
            i += 1;
            if self.eat_punct("...") {
                let name = self.expect_ident()?;
                let ty = if self.ts && self.eat_punct(":") {
                    self.parse_ts_type()?
                } else {
                    Ty::Any
                };
                params.push(Param { name, ty, default: None, variadic: true });
                break;
            }
            while self.ts
                && (self.at_kw("public")
                    || self.at_kw("private")
                    || self.at_kw("protected")
                    || self.at_kw("readonly"))
            {
                self.bump();
            }
            let target = self.parse_binding_target()?;
            self.eat_punct("?");
            let ty = if self.ts && self.eat_punct(":") {
                self.parse_ts_type()?
            } else {
                Ty::Any
            };
            let default = if self.eat_punct("=") { Some(self.parse_assign()?) } else { None };
            match target {
                Expr::Ident(name) => {
                    params.push(Param { name, ty, default, variadic: false });
                }
                pat => {
                    let tmp = format!("$p{}", i);
                    prologue.extend(self.desugar_pattern(&pat, &Expr::ident(&tmp))?);
                    params.push(Param {
                        name: tmp,
                        ty: Ty::Any,
                        default: None,
                        variadic: false,
                    });
                }
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct(")")?;
        Ok((params, prologue))
    }

    fn parse_class(&mut self) -> PResult<ClassDecl> {
        self.bump(); // class
        let name = self.expect_ident()?;
        if self.ts && self.at_punct("<") {
            self.skip_type_args()?;
        }
        let extends = if self.eat_kw("extends") {
            if self.cur().kind == Tok::Ident {
                Some(self.bump().text)
            } else {
                return Err(self.unsupported("non-identifier extends clause"));
            }
        } else {
            None
        };
        if self.ts && self.eat_kw("implements") {
            loop {
                self.expect_ident()?;
                if self.at_punct("<") {
                    self.skip_type_args()?;
                }
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        self.expect_punct("{")?;
        let mut cls = ClassDecl {
            name,
            extends,
            fields: Vec::new(),
            methods: Vec::new(),
            ctor: None,
            kind: ClassKind::Class,
            is_data: false,
        };
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of class body"));
            }
            if self.eat_punct(";") {
                continue;
            }
            self.parse_class_member(&mut cls)?;
        }
        self.expect_punct("}")?;
        Ok(cls)
    }

    fn parse_class_member(&mut self, cls: &mut ClassDecl) -> PResult<()> {
        let mut is_static = false;
        let mut visibility = Visibility::Default;
        let mut is_async = false;
        loop {
            if self.at_kw("static") && !self.nxt(1).is_punct("(") {
                self.bump();
                is_static = true;
            } else if self.ts
                && (self.at_kw("public") || self.at_kw("private") || self.at_kw("protected"))
                && !self.nxt(1).is_punct("(")
            {
                visibility = if self.at_kw("public") {
                    Visibility::Public
                } else if self.at_kw("private") {
                    Visibility::Private
                } else {
                    Visibility::Protected
                };
                self.bump();
            } else if self.ts && self.at_kw("readonly") && !self.nxt(1).is_punct("(") {
                self.bump();
            } else if self.at_kw("async")
                && (self.nxt(1).kind == Tok::Ident || self.nxt(1).kind == Tok::Keyword)
                && !self.nxt(2).is_punct("=")
            {
                self.bump();
                is_async = true;
            } else {
                break;
            }
        }
        if (self.at_kw("get") || self.at_kw("set"))
            && (self.nxt(1).kind == Tok::Ident
                || self.nxt(1).kind == Tok::Keyword
                || self.nxt(1).kind == Tok::Str)
        {
            return Err(self.unsupported("getter/setter class members"));
        }
        if self.at_punct("*") {
            return Err(self.unsupported("generator methods"));
        }
        if self.at_punct("[") {
            return Err(self.unsupported("computed class member names"));
        }
        let name = if self.cur().kind == Tok::Str {
            self.bump().text
        } else if self.cur().kind == Tok::Int {
            self.bump().text
        } else {
            self.expect_ident()?
        };
        if self.ts && self.at_punct("<") {
            self.skip_type_args()?;
        }
        if self.at_punct("(") {
            let is_ctor = name == "constructor";
            let (params, mut prologue) = self.parse_params()?;
            let ret = if self.ts && self.eat_punct(":") {
                self.parse_ts_type()?
            } else if is_ctor {
                Ty::Void
            } else {
                Ty::Any
            };
            let mut body = self.parse_block()?;
            prologue.append(&mut body);
            if is_ctor {
                let mut f = FunctionDecl::new(&name, params, Ty::Void, prologue);
                f.is_ctor = true;
                f.is_async = is_async;
                cls.ctor = Some(f);
            } else {
                let mut f = FunctionDecl::new(&name, params, ret, prologue);
                f.is_static = is_static;
                f.is_async = is_async;
                f.visibility = visibility;
                cls.methods.push(f);
            }
            return Ok(());
        }
        self.eat_punct("?");
        self.eat_punct("!");
        let ty = if self.ts && self.eat_punct(":") {
            self.parse_ts_type()?
        } else {
            Ty::Any
        };
        let init = if self.eat_punct("=") { Some(self.parse_assign()?) } else { None };
        self.expect_semi()?;
        cls.fields.push(FieldDecl {
            name,
            ty,
            init,
            is_static,
            mutable: true,
            visibility,
        });
        Ok(())
    }

    fn skip_type_args(&mut self) -> PResult<()> {
        self.expect_punct("<")?;
        let mut depth = 1;
        while depth > 0 {
            let t = self.bump();
            if t.kind == Tok::Eof {
                return Err(self.err("unexpected end of type arguments"));
            }
            if t.is_punct("<") {
                depth += 1;
            }
            if t.is_punct(">") {
                depth -= 1;
            }
        }
        Ok(())
    }

    fn parse_ts_enum(&mut self) -> PResult<ClassDecl> {
        self.bump(); // enum
        let name = self.expect_ident()?;
        self.expect_punct("{")?;
        let mut cls = ClassDecl {
            name,
            extends: None,
            fields: Vec::new(),
            methods: Vec::new(),
            ctor: None,
            kind: ClassKind::Enum,
            is_data: false,
        };
        let mut next: i64 = 0;
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of enum"));
            }
            let fname = self.expect_ident()?;
            let init = if self.eat_punct("=") {
                let e = self.parse_assign()?;
                if let Expr::Int(v) = e {
                    next = v + 1;
                }
                Some(e)
            } else {
                let v = next;
                next += 1;
                Some(Expr::Int(v))
            };
            cls.fields.push(FieldDecl {
                name: fname,
                ty: Ty::Any,
                init,
                is_static: false,
                mutable: false,
                visibility: Visibility::Public,
            });
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct("}")?;
        Ok(cls)
    }

    fn parse_ts_interface(&mut self) -> PResult<ClassDecl> {
        self.bump(); // interface
        let name = self.expect_ident()?;
        if self.at_punct("<") {
            self.skip_type_args()?;
        }
        if self.eat_kw("extends") {
            loop {
                self.expect_ident()?;
                if self.at_punct("<") {
                    self.skip_type_args()?;
                }
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        self.expect_punct("{")?;
        let mut cls = ClassDecl {
            name,
            extends: None,
            fields: Vec::new(),
            methods: Vec::new(),
            ctor: None,
            kind: ClassKind::Interface,
            is_data: false,
        };
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of interface"));
            }
            if self.eat_punct(";") {
                continue;
            }
            while self.at_kw("readonly") {
                self.bump();
            }
            let mname = self.expect_ident()?;
            if self.eat_punct("?") {
                // optional
            }
            if self.at_punct("<") {
                self.skip_type_args()?;
            }
            if self.at_punct("(") {
                let (params, _) = self.parse_params()?;
                if self.eat_punct(":") {
                    let _ = self.parse_ts_type()?;
                }
                self.eat_punct(";");
                cls.methods.push(FunctionDecl::new(&mname, params, Ty::Any, vec![]));
            } else {
                if self.eat_punct(":") {
                    let _ = self.parse_ts_type()?;
                }
                self.eat_punct(";");
                cls.fields.push(FieldDecl {
                    name: mname,
                    ty: Ty::Any,
                    init: None,
                    is_static: false,
                    mutable: true,
                    visibility: Visibility::Public,
                });
            }
        }
        self.expect_punct("}")?;
        Ok(cls)
    }

    // ---- expressions ----------------------------------------------------
    pub fn parse_expr(&mut self) -> PResult<Expr> {
        let first = self.parse_assign()?;
        if !self.at_punct(",") {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat_punct(",") {
            items.push(self.parse_assign()?);
        }
        Ok(Expr::Seq(items))
    }

    fn parse_assign(&mut self) -> PResult<Expr> {
        if let Some(arrow) = self.try_parse_arrow()? {
            return Ok(arrow);
        }
        let lhs = self.parse_ternary()?;
        const OPS: &[(&str, BinOp)] = &[
            ("+=", BinOp::Add),
            ("-=", BinOp::Sub),
            ("*=", BinOp::Mul),
            ("/=", BinOp::Div),
            ("%=", BinOp::Mod),
            ("**=", BinOp::Pow),
            ("&=", BinOp::BitAnd),
            ("|=", BinOp::BitOr),
            ("^=", BinOp::BitXor),
            ("<<=", BinOp::Shl),
            (">>=", BinOp::Shr),
            (">>>=", BinOp::UShr),
        ];
        for (p, op) in OPS {
            if self.at_punct(p) {
                self.bump();
                let rhs = self.parse_assign()?;
                return Ok(Expr::AssignExpr {
                    target: Box::new(lhs),
                    op: Some(*op),
                    value: Box::new(rhs),
                });
            }
        }
        if self.at_punct("=") {
            self.bump();
            let rhs = self.parse_assign()?;
            return Ok(Expr::AssignExpr {
                target: Box::new(lhs),
                op: None,
                value: Box::new(rhs),
            });
        }
        if self.at_punct("&&=") || self.at_punct("||=") || self.at_punct("??=") {
            return Err(self.unsupported("logical assignment operators"));
        }
        Ok(lhs)
    }

    fn try_parse_arrow(&mut self) -> PResult<Option<Expr>> {
        let save = self.pos;
        if self.at_kw("async") && self.nxt(1).is_punct("(") {
            if let Some(end) = self.find_matching_paren(1) {
                if self.toks[end + 1].is_punct("=>") {
                    self.bump(); // async
                    let a = self.parse_arrow_rest()?;
                    return Ok(Some(a));
                }
            }
            self.pos = save;
        }
        if self.at_punct("(") {
            if let Some(end) = self.find_matching_paren(0) {
                if self.toks[end + 1].is_punct("=>") {
                    let a = self.parse_arrow_rest()?;
                    return Ok(Some(a));
                }
            }
            return Ok(None);
        }
        if self.cur().kind == Tok::Ident && self.nxt(1).is_punct("=>") {
            let a = self.parse_arrow_rest()?;
            return Ok(Some(a));
        }
        Ok(None)
    }

    fn find_matching_paren(&self, from: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut i = self.pos + from;
        while i < self.toks.len() {
            if self.toks[i].is_punct("(") {
                depth += 1;
            } else if self.toks[i].is_punct(")") {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            i += 1;
        }
        None
    }

    fn parse_arrow_rest(&mut self) -> PResult<Expr> {
        let (params, mut prologue) = if self.cur().kind == Tok::Ident && self.nxt(1).is_punct("=>")
        {
            let name = self.bump().text;
            (vec![Param::new(&name, Ty::Any)], vec![])
        } else {
            self.parse_params()?
        };
        self.expect_punct("=>")?;
        if self.at_punct("{") {
            let mut body = self.parse_block()?;
            prologue.append(&mut body);
            Ok(Expr::Closure { params, body: prologue, value: None })
        } else {
            let value = self.parse_assign()?;
            Ok(Expr::Closure { params, body: prologue, value: Some(Box::new(value)) })
        }
    }

    fn parse_ternary(&mut self) -> PResult<Expr> {
        let cond = self.binary_level(0)?;
        if self.eat_punct("?") {
            let then = self.parse_assign()?;
            self.expect_punct(":")?;
            let els = self.parse_assign()?;
            return Ok(Expr::Ternary {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            });
        }
        Ok(cond)
    }

    fn binary_level(&mut self, level: usize) -> PResult<Expr> {
        const LEVELS: &[&[(&str, BinOp)]] = &[
            &[("??", BinOp::Nullish)],
            &[("||", BinOp::Or), ("&&", BinOp::And)],
            &[("|", BinOp::BitOr)],
            &[("^", BinOp::BitXor)],
            &[("&", BinOp::BitAnd)],
            &[("==", BinOp::Eq), ("!=", BinOp::Ne), ("===", BinOp::Eq), ("!==", BinOp::Ne)],
            &[
                ("<", BinOp::Lt),
                ("<=", BinOp::Le),
                (">", BinOp::Gt),
                (">=", BinOp::Ge),
                ("instanceof", BinOp::Eq),
                ("in", BinOp::In),
            ],
            &[("<<", BinOp::Shl), (">>", BinOp::Shr), (">>>", BinOp::UShr)],
            &[("+", BinOp::Add), ("-", BinOp::Sub)],
            &[("*", BinOp::Mul), ("/", BinOp::Div), ("%", BinOp::Mod)],
        ];
        if level >= LEVELS.len() {
            return self.parse_exponent();
        }
        let mut lhs = self.binary_level(level + 1)?;
        loop {
            let mut matched = None;
            for (p, op) in LEVELS[level] {
                if self.at_punct(p) || (self.cur().is_kw(p) && level == 6) {
                    matched = Some((*op, self.cur().text.clone()));
                    break;
                }
            }
            let (op, text) = match matched {
                Some(m) => m,
                None => break,
            };
            self.bump();
            let rhs = self.binary_level(level + 1)?;
            if text == "instanceof" {
                lhs = Expr::call(Expr::ident("__instanceof__"), vec![lhs, rhs]);
            } else {
                lhs = Expr::binary(op, lhs, rhs);
            }
        }
        Ok(lhs)
    }

    fn parse_exponent(&mut self) -> PResult<Expr> {
        let base = self.parse_unary()?;
        if self.at_punct("**") {
            self.bump();
            let exp = self.parse_exponent()?;
            return Ok(Expr::binary(BinOp::Pow, base, exp));
        }
        Ok(base)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        if self.at_punct("!") {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(Expr::Unary { op: UnOp::Not, e: Box::new(e) });
        }
        if self.at_punct("-") {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(Expr::Unary { op: UnOp::Neg, e: Box::new(e) });
        }
        if self.at_punct("+") {
            self.bump();
            return self.parse_unary();
        }
        if self.at_punct("~") {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(Expr::Unary { op: UnOp::BitNot, e: Box::new(e) });
        }
        if self.at_punct("++") || self.at_punct("--") {
            let is_inc = self.at_punct("++");
            self.bump();
            let e = self.parse_unary()?;
            return Ok(Expr::Unary {
                op: if is_inc { UnOp::PreInc } else { UnOp::PreDec },
                e: Box::new(e),
            });
        }
        if self.at_kw("typeof") {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(Expr::call(Expr::ident("__typeof__"), vec![e]));
        }
        if self.at_kw("void") {
            self.bump();
            let _ = self.parse_unary()?;
            return Ok(Expr::Undefined);
        }
        if self.at_kw("delete") {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(Expr::call(Expr::ident("__delete__"), vec![e]));
        }
        if self.at_kw("await") {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(Expr::Await(Box::new(e)));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            if self.at_punct(".") {
                self.bump();
                let name = self.member_name()?;
                e = Expr::Field { obj: Box::new(e), name, nullsafe: false };
                continue;
            }
            if self.at_punct("?.") {
                self.bump();
                if self.at_punct("(") {
                    let args = self.parse_args()?;
                    if let Expr::Field { obj, name, .. } = e {
                        e = Expr::Method { obj, name, args, nullsafe: true };
                    } else {
                        return Err(self.err("optional call on non-member"));
                    }
                    continue;
                }
                if self.at_punct("[") {
                    self.bump();
                    let idx = self.parse_expr()?;
                    self.expect_punct("]")?;
                    e = Expr::Index { obj: Box::new(e), idx: Box::new(idx) };
                    continue;
                }
                let name = self.member_name()?;
                e = Expr::Field { obj: Box::new(e), name, nullsafe: true };
                continue;
            }
            if self.at_punct("(") {
                let args = self.parse_args()?;
                match e {
                    Expr::Field { obj, name, .. } => {
                        e = Expr::Method { obj, name, args, nullsafe: false };
                    }
                    other => {
                        e = Expr::Call { callee: Box::new(other), args };
                    }
                }
                continue;
            }
            if self.at_punct("[") {
                self.bump();
                let idx = self.parse_expr()?;
                self.expect_punct("]")?;
                e = Expr::Index { obj: Box::new(e), idx: Box::new(idx) };
                continue;
            }
            if (self.at_punct("++") || self.at_punct("--")) && self.same_line_as_prev() {
                let is_inc = self.at_punct("++");
                self.bump();
                e = Expr::Post {
                    op: if is_inc { UnOp::PreInc } else { UnOp::PreDec },
                    e: Box::new(e),
                };
                continue;
            }
            if self.ts && self.at_punct("!") {
                self.bump(); // non-null assertion: strip
                continue;
            }
            if self.ts
                && (self.at_kw("as") || self.at_kw("satisfies"))
                && !self.nxt(1).is_punct("=")
            {
                self.bump();
                let _ = self.parse_ts_type()?;
                continue;
            }
            break;
        }
        Ok(e)
    }

    fn member_name(&mut self) -> PResult<String> {
        if self.cur().kind == Tok::Ident
            || self.cur().kind == Tok::Keyword
            || (self.ts && self.cur().kind == Tok::Keyword && self.is_contextual_ts_word(&self.cur().text))
        {
            return Ok(self.bump().text);
        }
        Err(self.err(format!("expected member name, found '{}'", self.cur().text)))
    }

    fn parse_args(&mut self) -> PResult<Vec<Expr>> {
        self.expect_punct("(")?;
        let mut args = Vec::new();
        while !self.at_punct(")") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of arguments"));
            }
            if self.eat_punct("...") {
                let e = self.parse_assign()?;
                args.push(Expr::Spread(Box::new(e)));
            } else {
                args.push(self.parse_assign()?);
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct(")")?;
        Ok(args)
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let t = self.cur().clone();
        match t.kind {
            Tok::Int => {
                self.bump();
                let v = parse_int_text(&t.text)
                    .ok_or_else(|| self.err(format!("bad integer literal '{}'", t.text)))?;
                Ok(Expr::Int(v))
            }
            Tok::Float => {
                self.bump();
                let v = parse_float_text(&t.text)
                    .ok_or_else(|| self.err(format!("bad float literal '{}'", t.text)))?;
                Ok(Expr::Float(v))
            }
            Tok::Str => {
                self.bump();
                Ok(Expr::Str(t.text))
            }
            Tok::Template => {
                self.bump();
                self.parse_template(&t.text)
            }
            Tok::Ident => {
                self.bump();
                Ok(Expr::Ident(t.text))
            }
            Tok::Keyword => match t.text.as_str() {
                "true" => {
                    self.bump();
                    Ok(Expr::Bool(true))
                }
                "false" => {
                    self.bump();
                    Ok(Expr::Bool(false))
                }
                "null" => {
                    self.bump();
                    Ok(Expr::Null)
                }
                "undefined" => {
                    self.bump();
                    Ok(Expr::Undefined)
                }
                "this" => {
                    self.bump();
                    Ok(Expr::This)
                }
                "super" => {
                    self.bump();
                    Ok(Expr::Super)
                }
                "new" => {
                    self.bump();
                    let cname = self.expect_ident()?;
                    if self.ts && self.at_punct("<") {
                        self.skip_type_args()?;
                    }
                    let args = if self.at_punct("(") { self.parse_args()? } else { vec![] };
                    Ok(Expr::New { class: cname, args })
                }
                "function" => {
                    self.bump();
                    if self.cur().kind == Tok::Ident {
                        self.bump(); // function expression name (ignored)
                    }
                    if self.at_punct("*") {
                        return Err(self.unsupported("generator functions"));
                    }
                    let (params, mut prologue) = self.parse_params()?;
                    let mut body = self.parse_block()?;
                    prologue.append(&mut body);
                    Ok(Expr::Closure { params, body: prologue, value: None })
                }
                "async" if self.nxt(1).is_kw("function") => {
                    self.bump();
                    self.bump();
                    if self.cur().kind == Tok::Ident {
                        self.bump();
                    }
                    let (params, mut prologue) = self.parse_params()?;
                    let mut body = self.parse_block()?;
                    prologue.append(&mut body);
                    Ok(Expr::Closure { params, body: prologue, value: None })
                }
                "async" if self.nxt(1).kind == Tok::Ident && self.nxt(2).is_punct("=>") => {
                    self.bump();
                    self.parse_arrow_rest()
                }
                "async" if self.nxt(1).is_punct("(") => {
                    // async arrow without matching => handled in parse_assign
                    Err(self.err("unexpected 'async'"))
                }
                "class" => {
                    let cls = self.parse_class()?;
                    let bind = format!("__class_{}", cls.name);
                    self.hoisted.push(Decl::Class(cls));
                    Ok(Expr::call(Expr::ident(&bind), vec![]))
                }
                _ if self.ts && self.is_contextual_ts_word(&t.text) => {
                    self.bump();
                    Ok(Expr::Ident(t.text))
                }
                _ => Err(self.err(format!("unexpected keyword '{}' in expression", t.text))),
            },
            Tok::Punct => {
                if t.text == "(" {
                    self.bump();
                    let e = self.parse_expr()?;
                    self.expect_punct(")")?;
                    return Ok(Expr::Paren(Box::new(e)));
                }
                if t.text == "[" {
                    return self.parse_array_literal();
                }
                if t.text == "{" {
                    return self.parse_object_literal();
                }
                if t.text == "/" {
                    return Err(self.unsupported("regular expression literals"));
                }
                Err(self.err(format!("unexpected token '{}' in expression", t.text)))
            }
            _ => Err(self.err(format!("unexpected token '{}' in expression", t.text))),
        }
    }

    fn parse_array_literal(&mut self) -> PResult<Expr> {
        self.expect_punct("[")?;
        let mut items = Vec::new();
        while !self.at_punct("]") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of array literal"));
            }
            if self.at_punct(",") {
                self.bump();
                items.push(Expr::Undefined);
                continue;
            }
            if self.eat_punct("...") {
                items.push(Expr::Spread(Box::new(self.parse_assign()?)));
            } else {
                items.push(self.parse_assign()?);
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct("]")?;
        Ok(Expr::Array(items))
    }

    fn parse_object_literal(&mut self) -> PResult<Expr> {
        self.expect_punct("{")?;
        let mut fields = Vec::new();
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of object literal"));
            }
            if self.eat_punct("...") {
                let e = self.parse_assign()?;
                fields.push(("__SPREAD__".to_string(), e));
                if !self.eat_punct(",") {
                    break;
                }
                continue;
            }
            if (self.at_kw("get") || self.at_kw("set"))
                && !(self.nxt(1).is_punct("(") || self.nxt(1).is_punct(",") || self.nxt(1).is_punct("}") || self.nxt(1).is_punct(":"))
            {
                return Err(self.unsupported("object getter/setter"));
            }
            let (key, is_method) = if self.cur().kind == Tok::Ident
                || self.cur().kind == Tok::Keyword
                || (self.ts && self.cur().kind == Tok::Keyword && self.is_contextual_ts_word(&self.cur().text))
            {
                let k = self.bump().text;
                let m = self.at_punct("(");
                (k, m)
            } else if self.cur().kind == Tok::Str || self.cur().kind == Tok::Int {
                let k = self.bump().text;
                (k, self.at_punct("("))
            } else {
                return Err(self.unsupported("computed object literal key"));
            };
            if is_method {
                let (params, mut prologue) = self.parse_params()?;
                let mut body = self.parse_block()?;
                prologue.append(&mut body);
                fields.push((key, Expr::Closure { params, body: prologue, value: None }));
            } else if self.eat_punct(":") {
                let v = self.parse_assign()?;
                fields.push((key, v));
            } else {
                if self.eat_punct("=") {
                    let d = self.parse_assign()?;
                    fields.push((
                        key.clone(),
                        Expr::Ternary {
                            cond: Box::new(Expr::binary(
                                BinOp::Eq,
                                Expr::ident(&key),
                                Expr::Undefined,
                            )),
                            then: Box::new(d),
                            els: Box::new(Expr::ident(&key)),
                        },
                    ));
                } else {
                    fields.push((key.clone(), Expr::ident(&key)));
                }
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct("}")?;
        Ok(Expr::Object(fields))
    }

    fn parse_template(&mut self, raw: &str) -> PResult<Expr> {
        let mut parts: Vec<InterpPart> = Vec::new();
        let mut lit = String::new();
        let chars: Vec<char> = raw.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' && i + 1 < chars.len() {
                lit.push(c);
                lit.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '$' && i + 1 < chars.len() && chars[i + 1] == '{' {
                if !lit.is_empty() {
                    parts.push(InterpPart::Lit(transpiler_core::lexer::unescape(&lit)));
                    lit.clear();
                }
                let mut depth = 1;
                let mut j = i + 2;
                while j < chars.len() && depth > 0 {
                    match chars[j] {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                    j += 1;
                }
                if depth != 0 {
                    return Err(self.err("unterminated template expression"));
                }
                let inner: String = chars[i + 2..j].iter().collect();
                let sub = parse_js(&inner, self.lang, &self.file)?;
                let mut found = None;
                for d in sub.decls {
                    if let Decl::Function(f) = d {
                        if f.name == "__main__" {
                            match f.body.into_iter().next() {
                                Some(Stmt::Return(Some(e))) => found = Some(e),
                                Some(Stmt::Expr(e)) => found = Some(e),
                                _ => {}
                            }
                        }
                    }
                }
                parts.push(InterpPart::E(found.ok_or_else(|| {
                    self.err("empty template expression")
                })?));
                i = j + 1;
                continue;
            }
            lit.push(c);
            i += 1;
        }
        if !lit.is_empty() {
            parts.push(InterpPart::Lit(transpiler_core::lexer::unescape(&lit)));
        }
        Ok(Expr::Interp { parts })
    }

    // ---- TypeScript types ------------------------------------------------
    fn parse_ts_type(&mut self) -> PResult<Ty> {
        let base = self.parse_ts_type_primary()?;
        let mut ty = base;
        while self.at_punct("[") && self.nxt(1).is_punct("]") {
            self.bump();
            self.bump();
            ty = Ty::Array(Box::new(ty));
        }
        if self.at_punct("|") {
            let mut has_null = false;
            while self.eat_punct("|") {
                let arm = self.parse_ts_type_primary()?;
                if matches!(arm, Ty::Any) {
                    has_null = true;
                }
            }
            if has_null {
                ty = Ty::Option(Box::new(ty));
            } else {
                ty = Ty::Any;
            }
        }
        if self.eat_punct("&") {
            let _ = self.parse_ts_type()?;
            ty = Ty::Any;
        }
        Ok(ty)
    }

    fn parse_ts_type_primary(&mut self) -> PResult<Ty> {
        let t = self.cur().clone();
        match t.kind {
            Tok::Ident => {
                self.bump();
                if t.text == "Array" && self.at_punct("<") {
                    self.bump();
                    let el = self.parse_ts_type()?;
                    self.expect_punct(">")?;
                    return Ok(Ty::Array(Box::new(el)));
                }
                if self.at_punct("<") {
                    self.skip_type_args()?;
                    return Ok(Ty::Any);
                }
                Ok(Ty::Class(t.text.clone()))
            }
            Tok::Keyword => {
                self.bump();
                match t.text.as_str() {
                    "any" | "unknown" | "never" | "undefined" => Ok(Ty::Any),
                    "number" => Ok(Ty::Double),
                    "boolean" => Ok(Ty::Bool),
                    "string" => Ok(Ty::Str),
                    "object" => Ok(Ty::Any),
                    "void" => Ok(Ty::Void),
                    _ => Ok(Ty::Any),
                }
            }
            Tok::Punct => {
                if t.text == "(" {
                    self.bump();
                    let mut depth = 1;
                    while depth > 0 {
                        let tk = self.bump();
                        if tk.kind == Tok::Eof {
                            return Err(self.err("unterminated function type"));
                        }
                        if tk.is_punct("(") {
                            depth += 1;
                        }
                        if tk.is_punct(")") {
                            depth -= 1;
                        }
                    }
                    if self.at_punct("=>") {
                        self.bump();
                        let _ = self.parse_ts_type()?;
                    }
                    return Ok(Ty::Any);
                }
                if t.text == "{" {
                    let mut depth = 0;
                    loop {
                        let tk = self.bump();
                        if tk.kind == Tok::Eof {
                            return Err(self.err("unterminated object type"));
                        }
                        if tk.is_punct("{") {
                            depth += 1;
                        }
                        if tk.is_punct("}") {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                    }
                    return Ok(Ty::Any);
                }
                if t.text == "[" {
                    self.bump();
                    while !self.at_punct("]") {
                        if self.cur().kind == Tok::Eof {
                            return Err(self.err("unterminated tuple type"));
                        }
                        let _ = self.parse_ts_type()?;
                        if !self.eat_punct(",") {
                            break;
                        }
                    }
                    self.expect_punct("]")?;
                    return Ok(Ty::Array(Box::new(Ty::Any)));
                }
                Err(self.err(format!("unexpected token '{}' in type", t.text)))
            }
            Tok::Str => {
                self.bump();
                Ok(Ty::Any)
            }
            Tok::Int => {
                self.bump();
                Ok(Ty::Any)
            }
            _ => Err(self.err(format!("unexpected token '{}' in type", t.text))),
        }
    }
}

fn first_decl_name(stmts: &[Stmt]) -> Option<String> {
    for s in stmts {
        if let Stmt::Local { name, .. } = s {
            return Some(name.clone());
        }
    }
    None
}

fn blockify(s: Stmt) -> Block {
    match s {
        Stmt::Block(b) => b,
        other => vec![other],
    }
}

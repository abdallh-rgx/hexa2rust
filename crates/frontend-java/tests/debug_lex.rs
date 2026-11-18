#[test]
fn debug_java_lex() {
    let src = "for (;;) { g(i); }";
    let cfg = frontend_java::java_lexer_config();
    let toks = transpiler_core::lexer::Lexer::new(src, &cfg).tokenize().unwrap();
    for t in &toks { println!("{:?} {} {}:{}", t.kind, t.text, t.line, t.col); }
}

#![forbid(unsafe_code)]
//! Python 3 front-end: indentation-aware hand-written lexer + recursive-descent
//! parser lowering to the unified IR.

use transpiler_core::diag::{Diag, PResult};
use transpiler_core::ir::*;
use transpiler_core::lexer::unescape;

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum PTok {
    Ident,
    Keyword,
    Int,
    Float,
    Str,
    /// f-string raw content
    Template,
    /// raw string content (r"..."): keep backslashes as-is
    RawStr,
    Punct,
    Newline,
    Indent,
    Dedent,
    Eof,
}

#[derive(Debug, Clone)]
pub struct PToken {
    pub kind: PTok,
    pub text: String,
    pub line: usize,
    pub col: usize,
}

const PY_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
    "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
    "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
    "try", "while", "with", "yield",
];

const PY_PUNCTS: &[&str] = &[
    "**=", "//=", "<<=", ">>=", "...", ":=", "**", "//", "<<", ">>", "<=", ">=", "==", "!=",
    "->", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "+", "-", "*", "/", "%", "<", ">",
    "=", "(", ")", "[", "]", "{", "}", ",", ":", ".", ";", "|", "^", "&", "~", "@",
];

pub fn py_tokenize(src: &str, file: &str) -> PResult<Vec<PToken>> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut line_start = 0usize;
    let mut at_line_start = true;
    let mut indents: Vec<usize> = vec![0];
    let mut paren_depth = 0i32;
    let mut out: Vec<PToken> = Vec::new();
    let mut pending_newline = false;

    macro_rules! err {
        ($col:expr, $msg:expr) => {
            return Err(Diag::new(line, $col, $msg).at(file))
        };
    }

    let mut sync_line_start = |i: usize, chars: &[char]| -> usize {
        chars[..i].iter().rposition(|&c| c == '\n').map_or(0, |p| p + 1)
    };

    while i < chars.len() {
        // ----- line start: measure indentation -----
        if at_line_start && paren_depth == 0 {
            let mut width = 0usize;
            let start = i;
            while i < chars.len() {
                match chars[i] {
                    ' ' => {
                        width += 1;
                        i += 1;
                    }
                    '\t' => {
                        width = (width / 8 + 1) * 8;
                        i += 1;
                    }
                    _ => break,
                }
            }
            if i >= chars.len() {
                break;
            }
            // blank line or comment-only line
            if chars[i] == '\n' || chars[i] == '#' {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                if i < chars.len() {
                    i += 1; // consume \n
                    line += 1;
                    line_start = i;
                }
                continue;
            }
            if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1] == '\n' {
                i += 2;
                line += 1;
                line_start = i;
                continue; // continuation, still line start
            }
            let cur = *indents.last().unwrap();
            if width > cur {
                indents.push(width);
                out.push(PToken { kind: PTok::Indent, text: String::new(), line, col: 1 });
            } else if width < cur {
                while *indents.last().unwrap() > width {
                    indents.pop();
                    out.push(PToken { kind: PTok::Dedent, text: String::new(), line, col: 1 });
                }
                if *indents.last().unwrap() != width {
                    err!(1, format!("inconsistent indentation at line {}", line));
                }
            }
            at_line_start = false;
            let _ = start;
            continue;
        }

        let c = chars[i];

        // newline
        if c == '\n' {
            i += 1;
            line += 1;
            line_start = i;
            if paren_depth > 0 {
                continue; // implicit line joining
            }
            if pending_newline {
                out.push(PToken { kind: PTok::Newline, text: "\\n".into(), line, col: 1 });
                pending_newline = false;
            }
            at_line_start = true;
            continue;
        }
        // spaces / tabs mid-line
        if c == ' ' || c == '\t' || c == '\r' {
            i += 1;
            continue;
        }
        // backslash continuation
        if c == '\\' && i + 1 < chars.len() && chars[i + 1] == '\n' {
            i += 2;
            line += 1;
            line_start = i;
            continue;
        }
        // comment
        if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        pending_newline = true;

        // string with optional prefix
        if c == '"' || c == '\'' || is_string_prefix_start(&chars, i) {
            let (tok, ni) = lex_py_string(&chars, i, line)?;
            let nl = tok.text.matches('\n').count();
            i = ni;
            out.push(tok);
            line += nl;
            line_start = out_line_start(&chars, i);
            continue;
        }

        // identifier / keyword
        if c == '_' || c.is_alphabetic() || (c as u32) > 127 {
            let start = i;
            while i < chars.len()
                && (chars[i] == '_'
                    || chars[i].is_alphanumeric()
                    || (chars[i] as u32) > 127)
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let kind = if PY_KEYWORDS.contains(&word.as_str()) {
                PTok::Keyword
            } else {
                PTok::Ident
            };
            out.push(PToken { kind, text: word, line, col: start - line_start + 1 });
            continue;
        }

        // number
        if c.is_ascii_digit() {
            let start = i;
            let mut is_float = false;
            if c == '0' && i + 1 < chars.len() && "xXbBoO".contains(chars[i + 1]) {
                i += 2;
                while i < chars.len() && (chars[i].is_ascii_hexdigit() || chars[i] == '_') {
                    i += 1;
                }
            } else {
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '_') {
                    i += 1;
                }
                if i + 1 < chars.len()
                    && chars[i] == '.'
                    && chars[i + 1].is_ascii_digit()
                    && paren_depth >= 0
                {
                    is_float = true;
                    i += 1;
                    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '_') {
                        i += 1;
                    }
                }
                if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                    let save = i;
                    i += 1;
                    if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
                        i += 1;
                    }
                    if i < chars.len() && chars[i].is_ascii_digit() {
                        is_float = true;
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    } else {
                        i = save;
                    }
                }
                // int suffix j (complex) -> treat as float
                if i < chars.len() && chars[i] == 'j' {
                    is_float = true;
                    i += 1;
                }
            }
            let text: String = chars[start..i].iter().collect();
            let kind = if is_float || text.contains('.') { PTok::Float } else { PTok::Int };
            out.push(PToken { kind, text, line, col: start - line_start + 1 });
            continue;
        }

        // punctuator
        let mut matched = None;
        for p in PY_PUNCTS {
            let pc: Vec<char> = p.chars().collect();
            if i + pc.len() <= chars.len() && chars[i..i + pc.len()] == pc[..] {
                matched = Some(*p);
                break;
            }
        }
        if let Some(p) = matched {
            let n = p.chars().count();
            if p == "(" || p == "[" || p == "{" {
                paren_depth += 1;
            }
            if p == ")" || p == "]" || p == "}" {
                paren_depth -= 1;
            }
            out.push(PToken { kind: PTok::Punct, text: p.to_string(), line, col: i - line_start + 1 });
            i += n;
            continue;
        }

        let col = i - line_start + 1;
        err!(col, format!("unexpected character '{}'", c));
    }

    // final newline + dedents
    if pending_newline && paren_depth <= 0 {
        out.push(PToken { kind: PTok::Newline, text: "\\n".into(), line, col: 1 });
    }
    while indents.len() > 1 {
        indents.pop();
        out.push(PToken { kind: PTok::Dedent, text: String::new(), line, col: 1 });
    }
    out.push(PToken { kind: PTok::Eof, text: String::new(), line, col: 1 });
    Ok(out)
}

fn out_line_start(chars: &[char], i: usize) -> usize {
    chars[..i].iter().rposition(|&c| c == '\n').map_or(0, |p| p + 1)
}

fn is_string_prefix_start(chars: &[char], i: usize) -> bool {
    let c = chars[i];
    if !"fFrRbBuU".contains(c) {
        return false;
    }
    // must be prefix followed directly by a quote
    let mut j = i;
    while j < chars.len() && "fFrRbBuU".contains(chars[j]) {
        j += 1;
    }
    j < chars.len() && (chars[j] == '"' || chars[j] == '\'')
}

/// Returns (token, new index)
fn lex_py_string(chars: &[char], i: usize, line: usize) -> PResult<(PToken, usize)> {
    let mut j = i;
    let mut prefix = String::new();
    while j < chars.len() && "fFrRbBuU".contains(chars[j]) {
        prefix.push(chars[j]);
        j += 1;
    }
    if j >= chars.len() {
        return Err(Diag::new(line, i + 1, "unterminated string prefix"));
    }
    let quote = chars[j];
    // triple?
    let triple = j + 2 < chars.len()
        && chars[j + 1] == quote
        && chars[j + 2] == quote;
    let content_start = if triple { j + 3 } else { j + 1 };
    let mut k = content_start;
    let mut content = String::new();
    loop {
        if k >= chars.len() {
            return Err(Diag::new(line, i + 1, "unterminated string literal"));
        }
        if !triple && chars[k] == '\n' {
            return Err(Diag::new(line, i + 1, "unterminated string literal"));
        }
        if chars[k] == '\\' && k + 1 < chars.len() {
            content.push('\\');
            content.push(chars[k + 1]);
            k += 2;
            continue;
        }
        if chars[k] == quote {
            if triple {
                if k + 2 < chars.len() && chars[k + 1] == quote && chars[k + 2] == quote {
                    break;
                }
            } else {
                break;
            }
        }
        content.push(chars[k]);
        k += 1;
    }
    let end = if triple { k + 3 } else { k + 1 };
    let is_raw = prefix.contains('r') || prefix.contains('R');
    let is_f = prefix.contains('f') || prefix.contains('F');
    if is_f {
        let tok = PToken { kind: PTok::Template, text: content, line, col: i + 1 };
        Ok((tok, end))
    } else if is_raw {
        let tok = PToken { kind: PTok::RawStr, text: content, line, col: i + 1 };
        Ok((tok, end))
    } else {
        let tok = PToken { kind: PTok::Str, text: unescape(&content), line, col: i + 1 };
        Ok((tok, end))
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

pub struct PyParser {
    toks: Vec<PToken>,
    pos: usize,
    file: String,
    imports: Vec<String>,
    hoisted: Vec<Decl>,
    globals: Vec<String>,
}

/// Parse a Python 3 source file into the unified IR.
pub fn parse_python(src: &str, file: &str) -> PResult<Module> {
    let toks = py_tokenize(src, file)?;
    let mut p = PyParser {
        toks,
        pos: 0,
        file: file.to_string(),
        imports: vec![],
        hoisted: vec![],
        globals: vec![],
    };
    p.parse_module()
}

impl PyParser {
    fn cur(&self) -> &PToken {
        &self.toks[self.pos.min(self.toks.len() - 1)]
    }
    fn nxt(&self, n: usize) -> &PToken {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)]
    }
    fn bump(&mut self) -> PToken {
        let t = self.toks[self.pos.min(self.toks.len() - 1)].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn skip_newlines(&mut self) {
        while self.cur().kind == PTok::Newline {
            self.bump();
        }
    }
    fn at_punct(&self, p: &str) -> bool {
        self.cur().kind == PTok::Punct && self.cur().text == p
    }
    fn at_kw(&self, k: &str) -> bool {
        self.cur().kind == PTok::Keyword && self.cur().text == k
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
        if self.cur().kind == PTok::Ident {
            Ok(self.bump().text)
        } else {
            Err(self.err(format!("expected identifier, found '{}'", self.cur().text)))
        }
    }
    fn expect_newline(&mut self) -> PResult<()> {
        if self.cur().kind == PTok::Newline {
            self.bump();
            return Ok(());
        }
        if self.cur().kind == PTok::Eof || self.cur().kind == PTok::Dedent {
            return Ok(());
        }
        if self.cur().kind == PTok::Punct && self.cur().text == ";" {
            return Ok(());
        }
        Err(self.err(format!("expected end of line, found '{}'", self.cur().text)))
    }
    fn err(&self, msg: impl Into<String>) -> Diag {
        Diag::new(self.cur().line, self.cur().col, msg.into()).at(&self.file)
    }
    fn unsupported(&self, what: &str) -> Diag {
        self.err(format!("unsupported Python syntax: {}", what))
    }

    // ---- module / blocks ------------------------------------------------
    fn parse_module(&mut self) -> PResult<Module> {
        let mut decls: Vec<Decl> = Vec::new();
        let mut main_body: Block = Vec::new();
        self.skip_newlines();
        while self.cur().kind != PTok::Eof {
            if self.at_kw("def") {
                let f = self.parse_function()?;
                decls.push(Decl::Function(f));
                self.skip_newlines();
                continue;
            }
            if self.at_kw("class") {
                let c = self.parse_class()?;
                decls.push(Decl::Class(c));
                self.skip_newlines();
                continue;
            }
            if self.at_kw("import") || self.at_kw("from") {
                self.parse_import()?;
                self.skip_newlines();
                continue;
            }
            if let Some(s) = self.parse_stmt()? {
                main_body.push(s);
            }
            self.skip_newlines();
        }
        if !main_body.is_empty() {
            decls.push(Decl::Function(FunctionDecl::new(
                "__main__",
                vec![],
                Ty::Any,
                main_body,
            )));
        }
        let mut hoisted = std::mem::take(&mut self.hoisted);
        decls.append(&mut hoisted);
        // module-level assignments become Globals
        let decls = promote_globals(decls, &self.globals);
        Ok(Module {
            lang: Lang::Python,
            name: "module".into(),
            imports: std::mem::take(&mut self.imports),
            decls,
        })
    }

    /// INDENT ... DEDENT block after ':'
    fn parse_block(&mut self) -> PResult<Block> {
        self.expect_punct(":")?;
        if self.eat_punct(";") {
            return Err(self.unsupported("; separated suite"));
        }
        if self.cur().kind == PTok::Newline {
            self.skip_newlines();
            if self.cur().kind != PTok::Indent {
                return Err(self.err("expected an indented block"));
            }
            self.bump(); // Indent
            let mut b = Vec::new();
            loop {
                self.skip_newlines();
                if self.cur().kind == PTok::Dedent {
                    self.bump();
                    break;
                }
                if self.cur().kind == PTok::Eof {
                    break;
                }
                if self.at_kw("def") {
                    let f = self.parse_function()?;
                    b.push(Stmt::Local {
                        name: f.name.clone(),
                        ty: Ty::Any,
                        init: Some(Expr::Closure {
                            params: f.params,
                            body: f.body,
                            value: None,
                        }),
                        mutable: false,
                    });
                    continue;
                }
                if self.at_kw("class") {
                    let c = self.parse_class()?;
                    let bind = format!("__class_{}", c.name);
                    self.hoisted.push(Decl::Class(c));
                    b.push(Stmt::Local {
                        name: bind.trim_start_matches("__class_").to_string(),
                        ty: Ty::Any,
                        init: Some(Expr::call(Expr::ident(&bind), vec![])),
                        mutable: false,
                    });
                    continue;
                }
                if let Some(s) = self.parse_stmt()? {
                    b.push(s);
                }
            }
            return Ok(b);
        }
        // simple statements on the same line
        let mut b = Vec::new();
        loop {
            if let Some(s) = self.parse_stmt()? {
                b.push(s);
            }
            if self.eat_punct(";") {
                continue;
            }
            break;
        }
        self.expect_newline()?;
        Ok(b)
    }

    fn parse_import(&mut self) -> PResult<()> {
        self.bump(); // import / from
        while self.cur().kind != PTok::Newline && self.cur().kind != PTok::Eof {
            if self.cur().kind == PTok::Ident || self.cur().kind == PTok::Keyword {
                self.bump();
            } else {
                self.bump();
            }
        }
        Ok(())
    }

    // ---- statements ------------------------------------------------------
    fn parse_stmt(&mut self) -> PResult<Option<Stmt>> {
        let t = self.cur().clone();
        if t.kind == PTok::Newline {
            self.bump();
            return Ok(None);
        }
        if t.kind == PTok::Punct && t.text == ";" {
            self.bump();
            return Ok(None);
        }
        if t.kind == PTok::Keyword {
            match t.text.as_str() {
                "def" => {
                    // nested def handled in parse_block; here error
                    let f = self.parse_function()?;
                    self.hoisted.push(Decl::Function(f));
                    return Ok(Some(Stmt::Pass));
                }
                "if" => {
                    self.bump();
                    let cond = self.parse_expr(true)?;
                    let then = self.parse_block()?;
                    let mut els = Vec::new();
                    if self.at_kw("elif") || self.at_kw("else") {
                        els = self.parse_elif_chain()?;
                    }
                    return Ok(Some(Stmt::If { cond, then, els }));
                }
                "while" => {
                    self.bump();
                    let cond = self.parse_expr(true)?;
                    let body = self.parse_block()?;
                    if self.at_kw("else") {
                        // while-else: rare
                        let _ = self.parse_block()?;
                    }
                    return Ok(Some(Stmt::While { cond, body }));
                }
                "for" => {
                    self.bump();
                    let target = self.parse_target_list()?;
                    if !self.eat_kw("in") {
                        return Err(self.err("expected 'in' in for loop"));
                    }
                    let iter = self.parse_expr(true)?;
                    let body = self.parse_block()?;
                    if self.at_kw("else") {
                        let _ = self.parse_block()?;
                    }
                    let stmts = self.for_each_from_target(target, iter, body)?;
                    return Ok(Some(if stmts.len() == 1 {
                        stmts.into_iter().next().unwrap()
                    } else {
                        Stmt::Block(stmts)
                    }));
                }
                "try" => {
                    return self.parse_try();
                }
                "with" => return Err(self.unsupported("with statement")),
                "raise" => {
                    self.bump();
                    let e = if self.cur().kind == PTok::Newline {
                        Expr::Str("exception".into())
                    } else {
                        self.parse_expr(true)?
                    };
                    self.expect_newline()?;
                    return Ok(Some(Stmt::Throw(e)));
                }
                "return" => {
                    self.bump();
                    let v = if self.cur().kind == PTok::Newline
                        || self.cur().kind == PTok::Eof
                        || self.cur().kind == PTok::Dedent
                    {
                        None
                    } else {
                        Some(self.parse_expr(true)?)
                    };
                    self.expect_newline()?;
                    return Ok(Some(Stmt::Return(v)));
                }
                "break" => {
                    self.bump();
                    self.expect_newline()?;
                    return Ok(Some(Stmt::Break));
                }
                "continue" => {
                    self.bump();
                    self.expect_newline()?;
                    return Ok(Some(Stmt::Continue));
                }
                "pass" => {
                    self.bump();
                    self.expect_newline()?;
                    return Ok(Some(Stmt::Pass));
                }
                "global" | "nonlocal" => {
                    self.bump();
                    while self.cur().kind == PTok::Ident {
                        let n = self.bump().text;
                        self.globals.push(n);
                        if !self.eat_punct(",") {
                            break;
                        }
                    }
                    self.expect_newline()?;
                    return Ok(Some(Stmt::Pass));
                }
                "assert" => {
                    self.bump();
                    let cond = self.parse_expr(true)?;
                    let msg = if self.eat_punct(",") {
                        Some(self.parse_expr(true)?)
                    } else {
                        None
                    };
                    self.expect_newline()?;
                    // assert c, m  ->  if !c: throw m or "AssertionError"
                    let exc = msg.unwrap_or_else(|| Expr::Str("AssertionError".into()));
                    return Ok(Some(Stmt::If {
                        cond: Expr::Unary { op: UnOp::Not, e: Box::new(cond) },
                        then: vec![Stmt::Throw(exc)],
                        els: vec![],
                    }));
                }
                "del" => {
                    self.bump();
                    let e = self.parse_expr(true)?;
                    self.expect_newline()?;
                    return Ok(Some(Stmt::Expr(Expr::call(
                        Expr::ident("__delete__"),
                        vec![e],
                    ))));
                }
                "import" | "from" => {
                    self.parse_import()?;
                    return Ok(None);
                }
                "lambda" => {
                    // lambda statement? treat as expression stmt
                }
                "yield" => return Err(self.unsupported("yield / generators")),
                "async" | "await" => return Err(self.unsupported("async/await")),
                "class" => {
                    let c = self.parse_class()?;
                    self.hoisted.push(Decl::Class(c));
                    return Ok(Some(Stmt::Pass));
                }
                _ => {}
            }
        }
        // decorators '@'
        if t.kind == PTok::Punct && t.text == "@" {
            while self.at_punct("@") {
                self.bump();
                let _ = self.parse_expr(true)?;
                self.skip_newlines();
            }
            // the decorated def/class follows
            if self.at_kw("def") {
                let f = self.parse_function()?;
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
            if self.at_kw("class") {
                let c = self.parse_class()?;
                let bind = format!("__class_{}", c.name);
                self.hoisted.push(Decl::Class(c));
                return Ok(Some(Stmt::Local {
                    name: bind.trim_start_matches("__class_").to_string(),
                    ty: Ty::Any,
                    init: Some(Expr::call(Expr::ident(&bind), vec![])),
                    mutable: false,
                }));
            }
            return Err(self.unsupported("decorated statement"));
        }
        // simple statement: expression / assignment
        let first = self.parse_expr(true)?;
        // annotated assignment:  x: int = 5
        if self.at_punct(":") {
            self.bump();
            let _ = self.parse_expr(true)?; // annotation (erased)
            if self.eat_punct("=") {
                let value = self.parse_expr(true)?;
                self.expect_newline()?;
                return Ok(Some(self.make_assign(first, None, value)?));
            }
            self.expect_newline()?;
            if let Expr::Ident(name) = first {
                return Ok(Some(Stmt::Local { name, ty: Ty::Any, init: None, mutable: true }));
            }
            return Ok(Some(Stmt::Pass));
        }
        // assignment(s): a = b = c
        const AUG: &[(&str, BinOp)] = &[
            ("+=", BinOp::Add),
            ("-=", BinOp::Sub),
            ("*=", BinOp::Mul),
            ("/=", BinOp::Div),
            ("//=", BinOp::FloorDiv),
            ("%=", BinOp::Mod),
            ("**=", BinOp::Pow),
            ("&=", BinOp::BitAnd),
            ("|=", BinOp::BitOr),
            ("^=", BinOp::BitXor),
            ("<<=", BinOp::Shl),
            (">>=", BinOp::Shr),
        ];
        for (p, op) in AUG {
            if self.at_punct(p) {
                self.bump();
                let value = self.parse_expr(true)?;
                self.expect_newline()?;
                return Ok(Some(self.make_assign(first, Some(*op), value)?));
            }
        }
        if self.at_punct("=") {
            let mut targets = vec![first];
            let mut value = None;
            while self.eat_punct("=") {
                let e = self.parse_expr(true)?;
                if self.at_punct("=") {
                    targets.push(e);
                } else {
                    value = Some(e);
                }
            }
            let value = value.ok_or_else(|| self.err("missing assignment value"))?;
            self.expect_newline()?;
            let mut stmts = Vec::new();
            let tmp = "$chain";
            stmts.push(Stmt::Local {
                name: tmp.into(),
                ty: Ty::Any,
                init: Some(value),
                mutable: false,
            });
            for tgt in targets {
                stmts.push(self.make_assign(tgt, None, Expr::ident("$chain"))?);
            }
            return Ok(Some(if stmts.len() == 1 {
                stmts.pop().unwrap()
            } else {
                Stmt::Block(stmts)
            }));
        }
        self.expect_newline()?;
        Ok(Some(Stmt::Expr(first)))
    }

    fn parse_elif_chain(&mut self) -> PResult<Block> {
        // at 'elif' or 'else'
        if self.eat_kw("elif") {
            let cond = self.parse_expr(true)?;
            let then = self.parse_block()?;
            let mut els = Vec::new();
            if self.at_kw("elif") || self.at_kw("else") {
                els = self.parse_elif_chain()?;
            }
            return Ok(vec![Stmt::If { cond, then, els }]);
        }
        if self.eat_kw("else") {
            return self.parse_block();
        }
        Ok(vec![])
    }

    fn parse_try(&mut self) -> PResult<Option<Stmt>> {
        self.bump(); // try
        let body = self.parse_block()?;
        let mut catches = Vec::new();
        while self.at_kw("except") {
            self.bump();
            let mut var = None;
            let mut ty = None;
            if self.cur().kind == PTok::Ident {
                ty = Some(Ty::class(&self.bump().text));
                if self.eat_kw("as") {
                    var = Some(self.expect_ident()?);
                }
            } else if self.eat_punct("(") {
                // except (A, B) as e
                while !self.at_punct(")") {
                    self.bump();
                    if !self.eat_punct(",") {
                        break;
                    }
                }
                self.expect_punct(")")?;
                if self.eat_kw("as") {
                    var = Some(self.expect_ident()?);
                }
            }
            let cbody = self.parse_block()?;
            catches.push(CatchClause { var, ty, body: cbody });
        }
        let mut finally = None;
        if self.at_kw("finally") {
            self.bump();
            finally = Some(self.parse_block()?);
        } else if catches.is_empty() {
            return Err(self.err("try without except/finally"));
        }
        Ok(Some(Stmt::Try { body, catches, finally }))
    }

    /// for target(s): ident, tuple of idents, possibly parenthesised
    fn parse_target_list(&mut self) -> PResult<Expr> {
        let mut items = vec![self.parse_primary()?];
        while self.eat_punct(",") {
            if self.at_kw("in") {
                break;
            }
            items.push(self.parse_primary()?);
        }
        if items.len() == 1 {
            return Ok(items.pop().unwrap());
        }
        Ok(Expr::Array(items))
    }

    fn for_each_from_target(
        &mut self,
        target: Expr,
        iter: Expr,
        body: Block,
    ) -> PResult<Vec<Stmt>> {
        match target {
            Expr::Ident(var) => Ok(vec![Stmt::ForEach {
                var,
                var_ty: Ty::Any,
                iter,
                body,
            }]),
            pat => {
                let var = "$it".to_string();
                Ok(vec![Stmt::ForEach {
                    var: var.clone(),
                    var_ty: Ty::Any,
                    iter,
                    body: {
                        let mut b = vec![Stmt::Local {
                            name: "$it".into(),
                            ty: Ty::Any,
                            init: Some(Expr::ident("$it")),
                            mutable: false,
                        }];
                        // desugar pattern
                        let mut des = self.desugar_pattern(&pat, &Expr::ident("$it"))?;
                        b.append(&mut des);
                        b.extend(body);
                        b
                    },
                }])
            }
        }
    }

    fn desugar_pattern(&mut self, pat: &Expr, value: &Expr) -> PResult<Vec<Stmt>> {
        let mut out = Vec::new();
        match pat {
            Expr::Ident(name) => out.push(Stmt::Local {
                name: name.clone(),
                ty: Ty::Any,
                init: Some(value.clone()),
                mutable: true,
            }),
            Expr::Array(items) => {
                for (i, sub) in items.iter().enumerate() {
                    let access = Expr::index(value.clone(), Expr::Int(i as i64));
                    out.extend(self.desugar_pattern(sub, &access)?);
                }
            }
            Expr::Paren(e) => out.extend(self.desugar_pattern(e, value)?),
            _ => return Err(self.unsupported("complex assignment target")),
        }
        Ok(out)
    }

    fn make_assign(&mut self, target: Expr, op: Option<BinOp>, value: Expr) -> PResult<Stmt> {
        match &target {
            Expr::Ident(_) | Expr::Field { .. } | Expr::Index { .. } | Expr::Paren(_) => {}
            Expr::Array(_) | Expr::Object(_) => {
                // tuple/list destructuring assignment
                let tmp = "$tgt";
                let mut stmts = vec![Stmt::Local {
                    name: tmp.into(),
                    ty: Ty::Any,
                    init: Some(value),
                    mutable: false,
                }];
                stmts.extend(self.desugar_pattern(&target, &Expr::ident("$tgt"))?);
                return Ok(Stmt::Block(stmts));
            }
            _ => return Err(self.unsupported("invalid assignment target")),
        }
        let target = match target {
            Expr::Paren(b) => *b,
            other => other,
        };
        Ok(Stmt::Assign { target, op, value })
    }

    // ---- functions & classes ----------------------------------------------
    fn parse_function(&mut self) -> PResult<FunctionDecl> {
        self.bump(); // def
        let name = self.expect_ident()?;
        self.expect_punct("(")?;
        let mut params = Vec::new();
        while !self.at_punct(")") {
            if self.cur().kind == PTok::Eof {
                return Err(self.err("unexpected end of parameter list"));
            }
            if self.eat_punct("*") {
                if self.eat_punct("*") {
                    return Err(self.unsupported("**kwargs"));
                }
                let n = self.expect_ident()?;
                params.push(Param {
                    name: n,
                    ty: Ty::Any,
                    default: None,
                    variadic: true,
                });
                if !self.eat_punct(",") {
                    break;
                }
                continue;
            }
            if self.at_punct("/") {
                self.bump();
                if !self.eat_punct(",") {
                    break;
                }
                continue;
            }
            let pname = self.expect_ident()?;
            if self.eat_punct(":") {
                let _ = self.parse_expr(false)?; // annotation erased
            }
            let default = if self.eat_punct("=") {
                Some(self.parse_expr(false)?)
            } else {
                None
            };
            params.push(Param::new(&pname, Ty::Any).with_default(default));
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct(")")?;
        if self.eat_punct("->") {
            let _ = self.parse_expr(false)?; // return annotation erased
        }
        let body = self.parse_block()?;
        let mut f = FunctionDecl::new(&name, params, Ty::Any, body);
        f.is_ctor = name == "__init__";
        Ok(f)
    }

    fn parse_class(&mut self) -> PResult<ClassDecl> {
        self.bump(); // class
        let name = self.expect_ident()?;
        let mut extends = None;
        if self.eat_punct("(") {
            let mut bases = Vec::new();
            while !self.at_punct(")") {
                if self.cur().kind == PTok::Ident {
                    bases.push(self.bump().text);
                } else if self.cur().kind == PTok::Keyword {
                    // metaclass=... keywords
                    self.bump();
                    self.bump();
                } else {
                    self.bump();
                }
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.expect_punct(")")?;
            extends = bases.into_iter().next();
        }
        let body = self.parse_block()?;
        let mut cls = ClassDecl {
            name,
            extends,
            fields: Vec::new(),
            methods: Vec::new(),
            ctor: None,
            kind: ClassKind::Class,
            is_data: false,
        };
        let mut leftover: Block = Vec::new();
        for s in body {
            match s {
                Stmt::Local { name, init, .. } => {
                    match init {
                        Some(Expr::Closure { params, body, value }) => {
                            // method (or constructor)
                            if name == "__init__" {
                                let mut f = FunctionDecl::new(&name, params, Ty::Void, body);
                                f.is_ctor = true;
                                cls.ctor = Some(f);
                            } else {
                                let _ = value;
                                cls.methods.push(FunctionDecl::new(&name, params, Ty::Any, body));
                            }
                        }
                        init => {
                            cls.fields.push(FieldDecl {
                                name,
                                ty: Ty::Any,
                                init,
                                is_static: false,
                                mutable: true,
                                visibility: Visibility::Default,
                            });
                        }
                    }
                }
                Stmt::Pass => {}
                Stmt::Expr(Expr::Str(_)) => {} // docstring
                other => leftover.push(other),
            }
        }
        if !leftover.is_empty() {
            return Err(self.unsupported("executable statements in class body"));
        }
        Ok(cls)
    }

    // ---- expressions ------------------------------------------------------
    /// allow_tuple: statement/return context
    fn parse_expr(&mut self, allow_tuple: bool) -> PResult<Expr> {
        let first = self.parse_lambda()?;
        if !allow_tuple || !self.at_punct(",") {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat_punct(",") {
            if self.cur().kind == PTok::Newline
                || self.cur().kind == PTok::Eof
                || self.at_punct("=")
            {
                break;
            }
            items.push(self.parse_lambda()?);
        }
        Ok(Expr::Array(items))
    }

    fn parse_lambda(&mut self) -> PResult<Expr> {
        if self.at_kw("lambda") {
            self.bump();
            let mut params = Vec::new();
            while !self.at_punct(":") {
                if self.cur().kind == PTok::Eof {
                    return Err(self.err("unexpected end of lambda"));
                }
                let n = self.expect_ident()?;
                let default = if self.eat_punct("=") {
                    Some(self.parse_or()?)
                } else {
                    None
                };
                params.push(Param::new(&n, Ty::Any).with_default(default));
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.expect_punct(":")?;
            let value = self.parse_lambda()?;
            return Ok(Expr::Closure { params, body: vec![], value: Some(Box::new(value)) });
        }
        self.parse_ternary()
    }

    fn parse_ternary(&mut self) -> PResult<Expr> {
        let v = self.parse_or()?;
        if self.at_kw("if") {
            self.bump();
            let cond = self.parse_or()?;
            if !self.eat_kw("else") {
                return Err(self.err("expected 'else' in conditional expression"));
            }
            let els = self.parse_lambda()?;
            return Ok(Expr::Ternary {
                cond: Box::new(cond),
                then: Box::new(v),
                els: Box::new(els),
            });
        }
        Ok(v)
    }

    fn parse_or(&mut self) -> PResult<Expr> {
        let mut l = self.parse_and()?;
        while self.at_kw("or") {
            self.bump();
            let r = self.parse_and()?;
            l = Expr::binary(BinOp::Or, l, r);
        }
        Ok(l)
    }
    fn parse_and(&mut self) -> PResult<Expr> {
        let mut l = self.parse_not()?;
        while self.at_kw("and") {
            self.bump();
            let r = self.parse_not()?;
            l = Expr::binary(BinOp::And, l, r);
        }
        Ok(l)
    }
    fn parse_not(&mut self) -> PResult<Expr> {
        if self.at_kw("not") {
            self.bump();
            let e = self.parse_not()?;
            return Ok(Expr::Unary { op: UnOp::Not, e: Box::new(e) });
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> PResult<Expr> {
        let first = self.parse_bitor()?;
        let mut operands: Vec<Expr> = vec![first];
        let mut ops: Vec<(BinOp, bool)> = Vec::new(); // (op, negated)
        loop {
            if self.at_punct("<") {
                self.bump();
                ops.push((BinOp::Lt, false));
            } else if self.at_punct("<=") {
                self.bump();
                ops.push((BinOp::Le, false));
            } else if self.at_punct(">") {
                self.bump();
                ops.push((BinOp::Gt, false));
            } else if self.at_punct(">=") {
                self.bump();
                ops.push((BinOp::Ge, false));
            } else if self.at_punct("==") {
                self.bump();
                ops.push((BinOp::Eq, false));
            } else if self.at_punct("!=") {
                self.bump();
                ops.push((BinOp::Ne, false));
            } else if self.at_kw("in") {
                self.bump();
                ops.push((BinOp::In, false));
            } else if self.at_kw("is") {
                self.bump();
                let neg = self.eat_kw("not");
                ops.push((BinOp::Eq, neg));
            } else if self.at_kw("not")
                && self.nxt(1).kind == PTok::Keyword
                && self.nxt(1).text == "in"
            {
                self.bump();
                self.bump();
                ops.push((BinOp::In, true));
            } else {
                break;
            }
            let r = self.parse_bitor()?;
            operands.push(r);
        }
        if ops.is_empty() {
            return Ok(operands.pop().unwrap());
        }
        // chained comparison: a < b < c  ==  (a < b) and (b < c)
        let mut result = apply_cmp(operands[0].clone(), &ops[0], operands[1].clone());
        for k in 1..ops.len() {
            let e = apply_cmp(operands[k].clone(), &ops[k], operands[k + 1].clone());
            result = Expr::binary(BinOp::And, result, e);
        }
        Ok(result)
    }

    fn parse_bitor(&mut self) -> PResult<Expr> {
        let mut l = self.parse_bitxor()?;
        while self.at_punct("|") {
            self.bump();
            let r = self.parse_bitxor()?;
            l = Expr::binary(BinOp::BitOr, l, r);
        }
        Ok(l)
    }
    fn parse_bitxor(&mut self) -> PResult<Expr> {
        let mut l = self.parse_bitand()?;
        while self.at_punct("^") {
            self.bump();
            let r = self.parse_bitand()?;
            l = Expr::binary(BinOp::BitXor, l, r);
        }
        Ok(l)
    }
    fn parse_bitand(&mut self) -> PResult<Expr> {
        let mut l = self.parse_shift()?;
        while self.at_punct("&") {
            self.bump();
            let r = self.parse_shift()?;
            l = Expr::binary(BinOp::BitAnd, l, r);
        }
        Ok(l)
    }
    fn parse_shift(&mut self) -> PResult<Expr> {
        let mut l = self.parse_additive()?;
        while self.at_punct("<<") || self.at_punct(">>") {
            let op = if self.at_punct("<<") { BinOp::Shl } else { BinOp::Shr };
            self.bump();
            let r = self.parse_additive()?;
            l = Expr::binary(op, l, r);
        }
        Ok(l)
    }
    fn parse_additive(&mut self) -> PResult<Expr> {
        let mut l = self.parse_multiplicative()?;
        while self.at_punct("+") || self.at_punct("-") {
            let op = if self.at_punct("+") { BinOp::Add } else { BinOp::Sub };
            self.bump();
            let r = self.parse_multiplicative()?;
            l = Expr::binary(op, l, r);
        }
        Ok(l)
    }
    fn parse_multiplicative(&mut self) -> PResult<Expr> {
        let mut l = self.parse_unary()?;
        loop {
            let op = if self.at_punct("*") {
                BinOp::Mul
            } else if self.at_punct("/") {
                BinOp::Div
            } else if self.at_punct("//") {
                BinOp::FloorDiv
            } else if self.at_punct("%") {
                BinOp::Mod
            } else if self.at_punct("@") {
                return Err(self.unsupported("@ matrix multiply"));
            } else {
                break;
            };
            self.bump();
            let r = self.parse_unary()?;
            l = Expr::binary(op, l, r);
        }
        Ok(l)
    }
    fn parse_unary(&mut self) -> PResult<Expr> {
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
        self.parse_power()
    }
    fn parse_power(&mut self) -> PResult<Expr> {
        let base = self.parse_postfix()?;
        if self.at_punct("**") {
            self.bump();
            let exp = self.parse_unary()?; // right assoc
            return Ok(Expr::binary(BinOp::Pow, base, exp));
        }
        Ok(base)
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            if self.at_punct(".") {
                self.bump();
                let name = if self.cur().kind == PTok::Ident || self.cur().kind == PTok::Keyword {
                    self.bump().text
                } else {
                    return Err(self.err("expected attribute name"));
                };
                e = Expr::Field { obj: Box::new(e), name, nullsafe: false };
                continue;
            }
            if self.at_punct("(") {
                let args = self.parse_call_args()?;
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
                // slice or index
                let lo = if self.at_punct(":") {
                    None
                } else {
                    Some(Box::new(self.parse_lambda()?))
                };
                if self.eat_punct(":") {
                    let hi = if self.at_punct("]") || self.at_punct(":") {
                        None
                    } else {
                        Some(Box::new(self.parse_lambda()?))
                    };
                    if self.at_punct(":") {
                        return Err(self.unsupported("slice step"));
                    }
                    self.expect_punct("]")?;
                    e = Expr::Slice { obj: Box::new(e), lo, hi };
                } else {
                    let idx = match lo {
                        Some(i) => i,
                        None => return Err(self.err("empty index")),
                    };
                    self.expect_punct("]")?;
                    e = Expr::Index { obj: Box::new(e), idx };
                }
                continue;
            }
            break;
        }
        Ok(e)
    }

    fn parse_call_args(&mut self) -> PResult<Vec<Expr>> {
        self.expect_punct("(")?;
        let mut args = Vec::new();
        while !self.at_punct(")") {
            if self.cur().kind == PTok::Eof {
                return Err(self.err("unexpected end of arguments"));
            }
            if self.eat_punct("*") {
                let e = self.parse_lambda()?;
                args.push(Expr::Spread(Box::new(e)));
            } else if self.eat_punct("**") {
                return Err(self.unsupported("**kwargs call"));
            } else if self.cur().kind == PTok::Ident
                && self.nxt(1).kind == PTok::Punct
                && self.nxt(1).text == "="
                && !(self.nxt(2).kind == PTok::Punct && self.nxt(2).text == "=")
            {
                let name = self.bump().text;
                self.bump(); // =
                let v = self.parse_lambda()?;
                args.push(Expr::Named { name, e: Box::new(v) });
            } else {
                args.push(self.parse_lambda()?);
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
            PTok::Int => {
                self.bump();
                let v = transpiler_core::lexer::parse_int_text(&t.text)
                    .ok_or_else(|| self.err(format!("bad integer '{}'", t.text)))?;
                Ok(Expr::Int(v))
            }
            PTok::Float => {
                self.bump();
                let v = transpiler_core::lexer::parse_float_text(&t.text)
                    .ok_or_else(|| self.err(format!("bad float '{}'", t.text)))?;
                Ok(Expr::Float(v))
            }
            PTok::Str => {
                self.bump();
                Ok(Expr::Str(t.text))
            }
            PTok::RawStr => {
                self.bump();
                Ok(Expr::Str(t.text))
            }
            PTok::Template => {
                self.bump();
                self.parse_fstring(&t.text)
            }
            PTok::Ident => {
                self.bump();
                Ok(Expr::Ident(t.text))
            }
            PTok::Keyword => match t.text.as_str() {
                "True" => {
                    self.bump();
                    Ok(Expr::Bool(true))
                }
                "False" => {
                    self.bump();
                    Ok(Expr::Bool(false))
                }
                "None" => {
                    self.bump();
                    Ok(Expr::Null)
                }
                "not" => {
                    self.bump();
                    let e = self.parse_not()?;
                    Ok(Expr::Unary { op: UnOp::Not, e: Box::new(e) })
                }
                "lambda" => self.parse_lambda(),
                "await" => Err(self.unsupported("await")),
                "yield" => Err(self.unsupported("yield")),
                _ => Err(self.err(format!("unexpected keyword '{}'", t.text))),
            },
            PTok::Punct => {
                if t.text == "(" {
                    self.bump();
                    if self.eat_punct(")") {
                        return Ok(Expr::Array(vec![])); // empty tuple
                    }
                    let e = self.parse_expr(true)?;
                    self.expect_punct(")")?;
                    return Ok(Expr::Paren(Box::new(e)));
                }
                if t.text == "[" {
                    return self.parse_list_literal();
                }
                if t.text == "{" {
                    return self.parse_dict_literal();
                }
                Err(self.err(format!("unexpected token '{}'", t.text)))
            }
            _ => Err(self.err(format!("unexpected token '{}'", t.text))),
        }
    }

    fn parse_list_literal(&mut self) -> PResult<Expr> {
        self.expect_punct("[")?;
        if self.eat_punct("]") {
            return Ok(Expr::Array(vec![]));
        }
        let first = self.parse_lambda()?;
        // list comprehension?
        if self.at_kw("for") {
            let comp = self.parse_comprehension(first)?;
            self.expect_punct("]")?;
            return Ok(comp);
        }
        let mut items = vec![first];
        while self.eat_punct(",") {
            if self.at_punct("]") {
                break;
            }
            items.push(self.parse_lambda()?);
        }
        self.expect_punct("]")?;
        Ok(Expr::Array(items))
    }

    fn parse_dict_literal(&mut self) -> PResult<Expr> {
        self.expect_punct("{")?;
        if self.eat_punct("}") {
            return Ok(Expr::Dict(vec![]));
        }
        let first_key = self.parse_lambda()?;
        if self.at_kw("for") {
            // set comprehension {x for ...} -> array
            let comp = self.parse_comprehension(first_key)?;
            self.expect_punct("}")?;
            return Ok(comp);
        }
        if self.at_punct(":") {
            self.bump(); // :
            let first_val = self.parse_lambda()?;
            if self.at_kw("for") {
                // dict comprehension {k: v for ...}
                let comp = self.parse_comprehension_kv(first_key, first_val)?;
                self.expect_punct("}")?;
                return Ok(comp);
            }
            // dict literal
            let mut pairs = vec![(first_key, first_val)];
            while self.eat_punct(",") {
                if self.at_punct("}") {
                    break;
                }
                let k = self.parse_lambda()?;
                self.expect_punct(":")?;
                let v = self.parse_lambda()?;
                pairs.push((k, v));
            }
            self.expect_punct("}")?;
            return Ok(Expr::Dict(pairs));
        }
        // set literal -> approximate as array
        let mut items = vec![first_key];
        while self.eat_punct(",") {
            if self.at_punct("}") {
                break;
            }
            items.push(self.parse_lambda()?);
        }
        self.expect_punct("}")?;
        Ok(Expr::Array(items))
    }

    /// [elt for target in iter (if cond)* (for target in iter ...)*]
    fn parse_comprehension(&mut self, elt: Expr) -> PResult<Expr> {
        let clauses = self.parse_comp_clauses_all()?;
        let push = Stmt::Expr(Expr::method(Expr::ident("$acc"), "append", vec![elt]));
        let body = self.build_comp_body(clauses, push)?;
        let mut fbody: Block = vec![Stmt::Local {
            name: "$acc".into(),
            ty: Ty::Any,
            init: Some(Expr::Array(vec![])),
            mutable: true,
        }];
        fbody.extend(body);
        fbody.push(Stmt::Return(Some(Expr::ident("$acc"))));
        Ok(Expr::Call {
            callee: Box::new(Expr::Closure { params: vec![], body: fbody, value: None }),
            args: vec![],
        })
    }

    fn parse_comprehension_kv(&mut self, key: Expr, value: Expr) -> PResult<Expr> {
        let clauses = self.parse_comp_clauses_all()?;
        let push = Stmt::Expr(Expr::method(
            Expr::ident("$acc"),
            "__setitem__",
            vec![key, value],
        ));
        let body = self.build_comp_body(clauses, push)?;
        let mut fbody: Block = vec![Stmt::Local {
            name: "$acc".into(),
            ty: Ty::Any,
            init: Some(Expr::Dict(vec![])),
            mutable: true,
        }];
        fbody.extend(body);
        fbody.push(Stmt::Return(Some(Expr::ident("$acc"))));
        Ok(Expr::Call {
            callee: Box::new(Expr::Closure { params: vec![], body: fbody, value: None }),
            args: vec![],
        })
    }

    fn build_comp_body(
        &mut self,
        clauses: Vec<(Expr, Expr, Vec<Expr>)>,
        innermost: Stmt,
    ) -> PResult<Block> {
        let mut body = vec![innermost];
        for (idx, (target, iter, conds)) in clauses.into_iter().enumerate().rev() {
            let mut inner = body;
            if !conds.is_empty() {
                let cond = conds
                    .into_iter()
                    .reduce(|a, b| Expr::binary(BinOp::And, a, b))
                    .unwrap();
                inner = vec![Stmt::If { cond, then: inner, els: vec![] }];
            }
            let var = format!("$it{}", idx);
            let stmts = match target {
                Expr::Ident(name) => vec![Stmt::ForEach {
                    var: name,
                    var_ty: Ty::Any,
                    iter,
                    body: inner,
                }],
                pat => vec![Stmt::ForEach {
                    var: var.clone(),
                    var_ty: Ty::Any,
                    iter,
                    body: {
                        let mut b = self.desugar_pattern(&pat, &Expr::ident(&var))?;
                        b.extend(inner);
                        b
                    },
                }],
            };
            body = stmts;
        }
        Ok(body)
    }

    /// all `for target in iter (if cond)*` clauses of a comprehension
    fn parse_comp_clauses_all(&mut self) -> PResult<Vec<(Expr, Expr, Vec<Expr>)>> {
        let mut out = Vec::new();
        while self.at_kw("for") {
            self.bump();
            let target = self.parse_target_list()?;
            if !self.eat_kw("in") {
                return Err(self.err("expected 'in' in comprehension"));
            }
            let iter = self.parse_or()?;
            let mut conds = Vec::new();
            while self.at_kw("if") {
                self.bump();
                conds.push(self.parse_or()?);
            }
            out.push((target, iter, conds));
        }
        if out.is_empty() {
            return Err(self.err("comprehension without for"));
        }
        Ok(out)
    }

    fn parse_fstring(&mut self, raw: &str) -> PResult<Expr> {
        let mut parts: Vec<InterpPart> = Vec::new();
        let chars: Vec<char> = raw.chars().collect();
        let mut lit = String::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '{' && i + 1 < chars.len() && chars[i + 1] == '{' {
                lit.push('{');
                i += 2;
                continue;
            }
            if c == '}' && i + 1 < chars.len() && chars[i + 1] == '}' {
                lit.push('}');
                i += 2;
                continue;
            }
            if c == '{' {
                if !lit.is_empty() {
                    parts.push(InterpPart::Lit(unescape(&lit)));
                    lit.clear();
                }
                let mut depth = 1;
                let mut j = i + 1;
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
                    return Err(self.err("unterminated f-string expression"));
                }
                let mut inner: String = chars[i + 1..j].iter().collect();
                // strip conversion/format suffix: !r, :spec (only outside nested braces)
                if let Some(pos) = find_top_level(&inner, '!') {
                    inner = inner[..pos].to_string();
                }
                if let Some(pos) = find_top_level(&inner, ':') {
                    inner = inner[..pos].to_string();
                }
                let sub = parse_python(&inner, &self.file)?;
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
                    self.err("empty f-string expression")
                })?));
                i = j + 1;
                continue;
            }
            if c == '\\' && i + 1 < chars.len() {
                lit.push(c);
                lit.push(chars[i + 1]);
                i += 2;
                continue;
            }
            lit.push(c);
            i += 1;
        }
        if !lit.is_empty() {
            parts.push(InterpPart::Lit(unescape(&lit)));
        }
        Ok(Expr::Interp { parts })
    }
}

fn find_top_level(s: &str, target: char) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in s.chars().enumerate() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ if depth == 0 && c == target => return Some(i),
            _ => {}
        }
    }
    None
}

fn apply_cmp(l: Expr, (op, neg): &(BinOp, bool), r: Expr) -> Expr {
    let e = Expr::binary(*op, l, r);
    if *neg {
        Expr::Unary { op: UnOp::Not, e: Box::new(e) }
    } else {
        e
    }
}

fn promote_globals(decls: Vec<Decl>, declared: &[String]) -> Vec<Decl> {
    let mut out = Vec::new();
    for d in decls {
        match d {
            Decl::Function(f) if f.name == "__main__" => {
                let mut new_body = Vec::new();
                for s in f.body {
                    match s {
                        Stmt::Local { name, ty, init, mutable } => {
                            out.push(Decl::Global(GlobalDecl {
                                name: name.clone(),
                                ty,
                                init,
                                mutable,
                                visibility: Visibility::Default,
                            }));
                        }
                        Stmt::Assign { target, op: None, value } => {
                            if let Expr::Ident(name) = target {
                                out.push(Decl::Global(GlobalDecl {
                                    name,
                                    ty: Ty::Any,
                                    init: Some(value),
                                    mutable: true,
                                    visibility: Visibility::Default,
                                }));
                            } else {
                                new_body.push(Stmt::Assign { target, op: None, value });
                            }
                        }
                        other => new_body.push(other),
                    }
                }
                if !new_body.is_empty() {
                    out.push(Decl::Function(FunctionDecl::new(
                        "__main__",
                        vec![],
                        Ty::Any,
                        new_body,
                    )));
                }
            }
            other => out.push(other),
        }
    }
    let _ = declared;
    out
}

trait ParamExt {
    fn with_default(self, d: Option<Expr>) -> Self;
}
impl ParamExt for Param {
    fn with_default(mut self, d: Option<Expr>) -> Self {
        self.default = d;
        self
    }
}

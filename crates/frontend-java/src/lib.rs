#![forbid(unsafe_code)]
//! Java front-end: hand-written lexer + recursive-descent parser lowering to
//! the unified IR. Covers a practical subset of Java 8-17: classes,
//! interfaces, enums, fields, methods, constructors, generics (erased),
//! arrays, control flow, lambdas, try/catch/finally, string concatenation.

use transpiler_core::diag::{Diag, PResult};
use transpiler_core::ir::*;
use transpiler_core::lexer::*;

const JAVA_KEYWORDS: &[&str] = &[
    "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class",
    "const", "continue", "default", "do", "double", "else", "enum", "extends", "final",
    "finally", "float", "for", "goto", "if", "implements", "import", "instanceof", "int",
    "interface", "long", "native", "new", "package", "private", "protected", "public",
    "return", "short", "static", "strictfp", "super", "switch", "synchronized", "this",
    "throw", "throws", "transient", "try", "void", "volatile", "while", "var", "record",
    "sealed", "permits", "yield",
];

const JAVA_PUNCTS: &[&str] = &[
    "<<=", ">>=", ">>>=", "...", "->", "::", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=",
    "|=", "^=", "<<", ">>", ">>>", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/",
    "%", "=", "<", ">", "!", "~", "?", ":", ";", ",", ".", "(", ")", "[", "]", "{", "}",
    "&", "|", "^", "@",
];

pub fn java_lexer_config() -> LexerConfig {
    LexerConfig {
        keywords: JAVA_KEYWORDS.to_vec(),
        puncts: JAVA_PUNCTS.to_vec(),
        backtick: Backtick::None,
        dollar_strings: false,
        char_literals: true,
    }
}

pub struct JavaParser {
    toks: Vec<Token>,
    pos: usize,
    file: String,
    imports: Vec<String>,
}

/// Parse a Java source file into the unified IR.
pub fn parse_java(src: &str, file: &str) -> PResult<Module> {
    let cfg = java_lexer_config();
    let toks = Lexer::new(src, &cfg).tokenize()?;
    let mut p = JavaParser {
        toks,
        pos: 0,
        file: file.to_string(),
        imports: vec![],
    };
    p.parse_compilation_unit()
}

impl JavaParser {
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
        if self.cur().kind == Tok::Ident {
            Ok(self.bump().text)
        } else {
            Err(self.err(format!("expected identifier, found '{}'", self.cur().text)))
        }
    }
    fn err(&self, msg: impl Into<String>) -> Diag {
        Diag::new(self.cur().line, self.cur().col, msg.into()).at(&self.file)
    }
    fn unsupported(&self, what: &str) -> Diag {
        self.err(format!("unsupported Java syntax: {}", what))
    }

    // ---- compilation unit -------------------------------------------------
    fn parse_compilation_unit(&mut self) -> PResult<Module> {
        let mut name = String::new();
        // package decl
        if self.eat_kw("package") {
            let mut parts = vec![self.expect_ident()?];
            while self.eat_punct(".") {
                parts.push(self.expect_ident()?);
            }
            name = parts.join(".");
            self.expect_punct(";")?;
        }
        while self.eat_kw("import") {
            if self.eat_kw("static") {}
            let mut path = String::new();
            if self.cur().kind == Tok::Ident || self.at_kw("var") {
                path.push_str(&self.bump().text);
            }
            while self.eat_punct(".") {
                if self.eat_punct("*") {
                    break;
                }
                path.push('.');
                path.push_str(&self.expect_ident()?);
            }
            self.expect_punct(";")?;
            if !path.is_empty() {
                self.imports.push(path);
            }
        }
        let mut decls = Vec::new();
        while self.cur().kind != Tok::Eof {
            if self.at_punct(";") {
                self.bump();
                continue;
            }
            if self.at_punct("@") {
                // annotation: @Name(...) or @Name
                self.bump();
                self.expect_ident()?;
                if self.at_punct("(") {
                    self.skip_balanced()?;
                }
                continue;
            }
            let d = self.parse_top_decl()?;
            if let Some(d) = d {
                decls.push(d);
            }
        }
        Ok(Module {
            lang: Lang::Java,
            name,
            imports: std::mem::take(&mut self.imports),
            decls,
        })
    }

    fn skip_balanced(&mut self) -> PResult<()> {
        let open = self.cur().text.clone();
        let close = match open.as_str() {
            "(" => ")",
            "[" => "]",
            "{" => "}",
            _ => return Ok(()),
        };
        self.bump();
        let mut depth = 1;
        while depth > 0 {
            let t = self.bump();
            if t.kind == Tok::Eof {
                return Err(self.err("unexpected end of balanced token"));
            }
            if t.text == open {
                depth += 1;
            }
            if t.text == close {
                depth -= 1;
            }
        }
        Ok(())
    }

    fn skip_generics(&mut self) -> PResult<()> {
        self.expect_punct("<")?;
        let mut depth = 1;
        while depth > 0 {
            let t = self.bump();
            if t.kind == Tok::Eof {
                return Err(self.err("unexpected end of generic arguments"));
            }
            if t.is_punct("<") {
                depth += 1;
            }
            if t.is_punct(">") {
                depth -= 1;
            }
            if t.is_punct(">>") {
                depth -= 2;
            }
            if t.is_punct(">>>") {
                depth -= 3;
            }
        }
        Ok(())
    }

    /// modifiers: public/private/protected/static/final/abstract/synchronized/
    /// transient/volatile/strictfp/native + annotations already skipped
    fn parse_modifiers(&mut self) -> (bool, bool, Visibility) {
        let mut is_static = false;
        let mut vis = Visibility::Default;
        loop {
            if self.at_kw("public") {
                vis = Visibility::Public;
                self.bump();
            } else if self.at_kw("private") {
                vis = Visibility::Private;
                self.bump();
            } else if self.at_kw("protected") {
                vis = Visibility::Protected;
                self.bump();
            } else if self.at_kw("static") {
                is_static = true;
                self.bump();
            } else if self.at_kw("final")
                || self.at_kw("abstract")
                || self.at_kw("synchronized")
                || self.at_kw("transient")
                || self.at_kw("volatile")
                || self.at_kw("strictfp")
                || self.at_kw("native")
                || self.at_kw("sealed")
                || self.at_kw("non-sealed")
            {
                self.bump();
            } else if self.at_punct("@") {
                self.bump();
                if self.cur().kind == Tok::Ident {
                    self.bump();
                }
                if self.at_punct("(") {
                    let _ = self.skip_balanced();
                }
            } else {
                break;
            }
        }
        (is_static, false, vis)
    }

    fn parse_top_decl(&mut self) -> PResult<Option<Decl>> {
        let (is_static, _, vis) = self.parse_modifiers();
        let _ = is_static;
        if self.at_kw("class") || self.at_kw("interface") || self.at_kw("enum") || self.at_kw("record") {
            let cls = self.parse_class_or_interface()?;
            return Ok(Some(Decl::Class(cls)));
        }
        if self.at_punct(";") {
            self.bump();
            return Ok(None);
        }
        Err(self.err(format!("expected class/interface/enum, found '{}'", self.cur().text)))
    }

    fn parse_class_or_interface(&mut self) -> PResult<ClassDecl> {
        let kind_kw = self.bump().text; // class | interface | enum | record
        let name = self.expect_ident()?;
        if self.at_punct("<") {
            self.skip_generics()?;
        }
        let mut kind = match kind_kw.as_str() {
            "interface" => ClassKind::Interface,
            "enum" => ClassKind::Enum,
            "record" => ClassKind::Struct,
            _ => ClassKind::Class,
        };
        let mut extends = None;
        if self.eat_kw("extends") {
            extends = Some(self.parse_type_name()?);
            if self.at_punct("<") {
                self.skip_generics()?;
            }
        }
        if self.eat_kw("implements") {
            loop {
                let _ = self.parse_type_name()?;
                if self.at_punct("<") {
                    self.skip_generics()?;
                }
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        if self.eat_kw("permits") {
            loop {
                let _ = self.parse_type_name()?;
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
            kind,
            is_data: false,
        };
        if kind_kw == "record" {
            // record components -> fields
            if self.eat_punct("(") {
                while !self.at_punct(")") {
                    let _ = self.parse_type()?;
                    let fname = self.expect_ident()?;
                    cls.fields.push(FieldDecl {
                        name: fname,
                        ty: Ty::Any,
                        init: None,
                        is_static: false,
                        mutable: true,
                        visibility: Visibility::Public,
                    });
                    if !self.eat_punct(",") {
                        break;
                    }
                }
                self.expect_punct(")")?;
            }
        }
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of class body"));
            }
            if self.eat_punct(";") {
                continue;
            }
            if self.at_punct("{") {
                // static/instance initializer block
                let _ = self.parse_block()?;
                continue;
            }
            self.parse_class_member(&mut cls)?;
        }
        self.expect_punct("}")?;
        // enums with bodies become classes
        if kind == ClassKind::Enum && (!cls.methods.is_empty() || cls.ctor.is_some()) {
            kind = ClassKind::Enum;
        }
        Ok(cls)
    }

    fn parse_type_name(&mut self) -> PResult<String> {
        let mut name = self.expect_ident()?;
        while self.eat_punct(".") {
            name.push('.');
            name.push_str(&self.expect_ident()?);
        }
        Ok(name)
    }

    fn parse_class_member(&mut self, cls: &mut ClassDecl) -> PResult<()> {
        let (is_static, _, vis) = self.parse_modifiers();
        // constructor?
        if self.cur().kind == Tok::Ident && self.cur().text == cls.name && self.nxt(1).is_punct("(") {
            let cname = self.bump().text;
            if self.at_punct("<") {
                self.skip_generics()?;
            }
            let (params, _) = self.parse_params()?;
            // throws
            if self.eat_kw("throws") {
                loop {
                    let _ = self.parse_type_name()?;
                    if !self.eat_punct(",") {
                        break;
                    }
                }
            }
            let body = if self.at_punct("{") { self.parse_block()? } else { self.expect_punct(";")?; vec![] };
            let mut f = FunctionDecl::new(&cname, params, Ty::Void, body);
            f.is_ctor = true;
            f.visibility = vis;
            cls.ctor = Some(f);
            return Ok(());
        }
        // method or field: type name ( ... ) or type name [= init] ;
        if self.at_kw("class") || self.at_kw("interface") || self.at_kw("enum") {
            // nested type: parse and skip by making it a nested class
            let inner = self.parse_class_or_interface()?;
            cls.fields.push(FieldDecl {
                name: format!("__nested_{}", inner.name),
                ty: Ty::class(&inner.name),
                init: None,
                is_static: true,
                mutable: false,
                visibility: Visibility::Private,
            });
            return Ok(());
        }
        // generic method
        if self.at_punct("<") {
            self.skip_generics()?;
        }
        let ty = self.parse_type()?;
        let mname = self.expect_ident()?;
        if self.at_punct("(") {
            let (params, _) = self.parse_params()?;
            let _ = self.eat_punct("[");
            let _ = self.eat_punct("]");
            if self.eat_kw("throws") {
                loop {
                    let _ = self.parse_type_name()?;
                    if !self.eat_punct(",") {
                        break;
                    }
                }
            }
            if self.eat_kw("default") {
                // default method body expression
                let _ = self.parse_expr()?;
                self.expect_punct(";")?;
                cls.methods.push(FunctionDecl::new(&mname, params, ty, vec![]));
                return Ok(());
            }
            let body = if self.at_punct("{") { self.parse_block()? } else { self.expect_punct(";")?; vec![] };
            if body.is_empty() && cls.kind == ClassKind::Interface {
                cls.methods.push(FunctionDecl::new(&mname, params, ty, vec![]));
                return Ok(());
            }
            let mut f = FunctionDecl::new(&mname, params, ty, body);
            f.is_static = is_static;
            f.visibility = vis;
            if cls.kind == ClassKind::Interface {
                cls.methods.push(f);
            } else {
                cls.methods.push(f);
            }
            return Ok(());
        }
        // field(s)
        loop {
            let mut fty = ty.clone();
            // array suffix
            while self.at_punct("[") && self.nxt(1).is_punct("]") {
                self.bump();
                self.bump();
                fty = Ty::Array(Box::new(fty));
            }
            let init = if self.eat_punct("=") {
                Some(self.parse_var_init()?)
            } else {
                None
            };
            cls.fields.push(FieldDecl {
                name: mname.clone(),
                ty: fty,
                init,
                is_static,
                mutable: true,
                visibility: vis,
            });
            if self.eat_punct(",") {
                // next declarator shares the type
                let next = self.expect_ident()?;
                let mut nty = ty.clone();
                while self.at_punct("[") && self.nxt(1).is_punct("]") {
                    self.bump();
                    self.bump();
                    nty = Ty::Array(Box::new(nty));
                }
                let ninit = if self.eat_punct("=") {
                    Some(self.parse_var_init()?)
                } else {
                    None
                };
                cls.fields.push(FieldDecl {
                    name: next,
                    ty: nty,
                    init: ninit,
                    is_static,
                    mutable: true,
                    visibility: vis,
                });
                continue;
            }
            self.expect_punct(";")?;
            break;
        }
        Ok(())
    }

    fn parse_var_init(&mut self) -> PResult<Expr> {
        if self.at_punct("{") {
            return self.parse_array_init();
        }
        self.parse_expr()
    }

    fn parse_array_init(&mut self) -> PResult<Expr> {
        self.expect_punct("{")?;
        let mut items = Vec::new();
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of array initializer"));
            }
            if self.at_punct("{") {
                items.push(self.parse_array_init()?);
            } else {
                items.push(self.parse_expr()?);
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct("}")?;
        Ok(Expr::Array(items))
    }

    // ---- types ---------------------------------------------------------------
    fn parse_type(&mut self) -> PResult<Ty> {
        let base = if self.at_kw("int")
            || self.at_kw("long")
            || self.at_kw("short")
            || self.at_kw("byte")
            || self.at_kw("float")
            || self.at_kw("double")
            || self.at_kw("boolean")
            || self.at_kw("char")
            || self.at_kw("void")
            || self.at_kw("var")
        {
            let t = self.bump().text;
            match t.as_str() {
                "int" | "short" | "byte" => Ty::Int,
                "long" => Ty::Long,
                "float" | "double" => Ty::Double,
                "boolean" => Ty::Bool,
                "char" => Ty::Char,
                "void" => Ty::Void,
                _ => Ty::Any, // var
            }
        } else if self.at_kw("String") {
            self.bump();
            Ty::Str
        } else {
            let name = self.parse_type_name()?;
            if name == "String" || name == "CharSequence" {
                Ty::Str
            } else if self.at_punct("<") {
                self.skip_generics()?;
                if name == "List" || name == "ArrayList" || name == "Collection" {
                    Ty::Array(Box::new(Ty::Any))
                } else if name == "Map" || name == "HashMap" {
                    Ty::Map(Box::new(Ty::Any), Box::new(Ty::Any))
                } else if name == "Optional" {
                    Ty::Option(Box::new(Ty::Any))
                } else {
                    Ty::Any
                }
            } else {
                Ty::class(&name)
            }
        };
        let mut ty = base;
        while self.at_punct("[") && self.nxt(1).is_punct("]") {
            self.bump();
            self.bump();
            ty = Ty::Array(Box::new(ty));
        }
        if self.eat_punct("...") {
            ty = Ty::Array(Box::new(ty));
        }
        Ok(ty)
    }

    fn parse_params(&mut self) -> PResult<(Vec<Param>, Option<String>)> {
        self.expect_punct("(")?;
        let mut params = Vec::new();
        let mut variadic = None;
        while !self.at_punct(")") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of parameters"));
            }
            while self.eat_kw("final") {}
            // parameter annotations: @Nullable, @Nonnull...
            while self.at_punct("@") {
                self.bump();
                if self.cur().kind == Tok::Ident {
                    self.bump();
                }
                if self.at_punct(".") {
                    while self.eat_punct(".") {
                        let _ = self.expect_ident()?;
                    }
                }
                if self.at_punct("(") {
                    self.skip_balanced()?;
                }
            }
            let ty = self.parse_type()?;
            // trailing annotations after the type: `Object @Nullable ...`
            while self.at_punct("@") {
                self.bump();
                if self.cur().kind == Tok::Ident {
                    self.bump();
                }
                if self.at_punct("(") {
                    self.skip_balanced()?;
                }
            }
            // `...` may appear before the name (annotated variadics)
            let was_variadic = self.eat_punct("...");
            let name = self.expect_ident()?;
            let mut pty = ty;
            while self.at_punct("[") && self.nxt(1).is_punct("]") {
                self.bump();
                self.bump();
                pty = Ty::Array(Box::new(pty));
            }
            if was_variadic || self.at_punct("...") {
                if !was_variadic {
                    self.bump();
                }
                variadic = Some(name.clone());
                params.push(Param {
                    name,
                    ty: Ty::Array(Box::new(pty)),
                    default: None,
                    variadic: true,
                });
                break;
            }
            params.push(Param::new(&name, pty));
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct(")")?;
        Ok((params, variadic))
    }

    // ---- statements -------------------------------------------------------------
    fn parse_block(&mut self) -> PResult<Block> {
        self.expect_punct("{")?;
        let mut b = Vec::new();
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of block"));
            }
            if self.eat_punct(";") {
                continue;
            }
            if let Some(s) = self.parse_stmt()? {
                b.push(s);
            }
        }
        self.expect_punct("}")?;
        Ok(b)
    }

    fn parse_stmt(&mut self) -> PResult<Option<Stmt>> {
        let t = self.cur().clone();
        if t.is_punct("{") {
            return Ok(Some(Stmt::Block(self.parse_block()?)));
        }
        if t.is_punct(";") {
            self.bump();
            return Ok(Some(Stmt::Pass));
        }
        if t.kind == Tok::Keyword {
            match t.text.as_str() {
                "if" => {
                    self.bump();
                    self.expect_punct("(")?;
                    let cond = self.parse_expr()?;
                    self.expect_punct(")")?;
                    let then = self.parse_stmt()?.unwrap_or(Stmt::Pass);
                    let then = if let Stmt::Block(b) = then { b } else { vec![then] };
                    let els = if self.eat_kw("else") {
                        let s = self.parse_stmt()?.unwrap_or(Stmt::Pass);
                        if let Stmt::Block(b) = s { b } else { vec![s] }
                    } else {
                        vec![]
                    };
                    return Ok(Some(Stmt::If { cond, then, els }));
                }
                "while" => {
                    self.bump();
                    self.expect_punct("(")?;
                    let cond = self.parse_expr()?;
                    self.expect_punct(")")?;
                    let body = self.parse_stmt()?.unwrap_or(Stmt::Pass);
                    let body = if let Stmt::Block(b) = body { b } else { vec![body] };
                    return Ok(Some(Stmt::While { cond, body }));
                }
                "do" => {
                    self.bump();
                    let body = self.parse_stmt()?.unwrap_or(Stmt::Pass);
                    let body = if let Stmt::Block(b) = body { b } else { vec![body] };
                    if !self.eat_kw("while") {
                        return Err(self.err("expected 'while' after do"));
                    }
                    self.expect_punct("(")?;
                    let cond = self.parse_expr()?;
                    self.expect_punct(")")?;
                    self.expect_punct(";")?;
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
                "return" => {
                    self.bump();
                    let v = if self.at_punct(";") { None } else { Some(self.parse_expr()?) };
                    self.expect_punct(";")?;
                    return Ok(Some(Stmt::Return(v)));
                }
                "break" => {
                    self.bump();
                    if self.cur().kind == Tok::Ident {
                        self.bump();
                    }
                    self.expect_punct(";")?;
                    return Ok(Some(Stmt::Break));
                }
                "continue" => {
                    self.bump();
                    if self.cur().kind == Tok::Ident {
                        self.bump();
                    }
                    self.expect_punct(";")?;
                    return Ok(Some(Stmt::Continue));
                }
                "throw" => {
                    self.bump();
                    let e = self.parse_expr()?;
                    self.expect_punct(";")?;
                    return Ok(Some(Stmt::Throw(e)));
                }
                "try" => return self.parse_try(),
                "assert" => {
                    self.bump();
                    let cond = self.parse_expr()?;
                    let msg = if self.eat_punct(":") { Some(self.parse_expr()?) } else { None };
                    self.expect_punct(";")?;
                    let exc = msg.unwrap_or_else(|| Expr::Str("AssertionError".into()));
                    return Ok(Some(Stmt::If {
                        cond: Expr::Unary { op: UnOp::Not, e: Box::new(cond) },
                        then: vec![Stmt::Throw(exc)],
                        els: vec![],
                    }));
                }
                "synchronized" => {
                    self.bump();
                    if self.at_punct("(") {
                        self.skip_balanced()?;
                    }
                    let b = self.parse_block()?;
                    return Ok(Some(Stmt::Block(b)));
                }
                _ => {}
            }
            // local class?
            if self.at_kw("class") || self.at_kw("interface") || self.at_kw("enum") {
                return Err(self.unsupported("local classes"));
            }
        }
        // local variable declaration?
        if self.is_type_lookahead() {
            // Type name [= init] [, name = init]* ;
            let ty = self.parse_type()?;
            loop {
                let name = self.expect_ident()?;
                let mut fty = ty.clone();
                while self.at_punct("[") && self.nxt(1).is_punct("]") {
                    self.bump();
                    self.bump();
                    fty = Ty::Array(Box::new(fty));
                }
                let init = if self.eat_punct("=") {
                    Some(self.parse_var_init()?)
                } else {
                    None
                };
                let stmt = Stmt::Local { name, ty: fty, init, mutable: true };
                if self.eat_punct(",") {
                    // multiple declarators: wrap later
                    return self.parse_multi_decls(ty, stmt);
                }
                self.expect_punct(";")?;
                return Ok(Some(stmt));
            }
        }
        // expression statement / assignment
        let e = self.parse_expr()?;
        const AUG: &[(&str, BinOp)] = &[
            ("+=", BinOp::Add),
            ("-=", BinOp::Sub),
            ("*=", BinOp::Mul),
            ("/=", BinOp::Div),
            ("%=", BinOp::Mod),
            ("&=", BinOp::BitAnd),
            ("|=", BinOp::BitOr),
            ("^=", BinOp::BitXor),
            ("<<=", BinOp::Shl),
            (">>=", BinOp::Shr),
            (">>>=", BinOp::UShr),
        ];
        for (p, op) in AUG {
            if self.at_punct(p) {
                self.bump();
                let value = self.parse_expr()?;
                self.expect_punct(";")?;
                return Ok(Some(Stmt::Assign { target: e, op: Some(*op), value }));
            }
        }
        if self.at_punct("=") {
            self.bump();
            let value = self.parse_expr()?;
            self.expect_punct(";")?;
            return Ok(Some(Stmt::Assign { target: e, op: None, value }));
        }
        self.expect_punct(";")?;
        Ok(Some(Stmt::Expr(e)))
    }

    fn parse_multi_decls(&mut self, ty: Ty, first: Stmt) -> PResult<Option<Stmt>> {
        let mut stmts = vec![first];
        loop {
            let name = self.expect_ident()?;
            let mut fty = ty.clone();
            while self.at_punct("[") && self.nxt(1).is_punct("]") {
                self.bump();
                self.bump();
                fty = Ty::Array(Box::new(fty));
            }
            let init = if self.eat_punct("=") { Some(self.parse_var_init()?) } else { None };
            stmts.push(Stmt::Local { name, ty: fty, init, mutable: true });
            if self.eat_punct(",") {
                continue;
            }
            self.expect_punct(";")?;
            break;
        }
        Ok(Some(Stmt::Block(stmts)))
    }

    /// heuristic: current token starts a type (keyword type or Ident), and the
    /// token after (ignoring generics/qualified names) is an identifier ->
    /// local variable declaration.
    fn is_type_lookahead(&self) -> bool {
        let t = self.cur();
        if t.kind != Tok::Keyword && t.kind != Tok::Ident {
            return false;
        }
        let type_kw = matches!(
            t.text.as_str(),
            "int" | "long" | "short" | "byte" | "float" | "double" | "boolean" | "char" | "var"
        );
        if type_kw {
            // `int x` or `char[] x`
            if self.nxt(1).is_punct("[") && self.nxt(2).is_punct("]") {
                return self.nxt(3).kind == Tok::Ident;
            }
            return self.nxt(1).kind == Tok::Ident;
        }
        if t.kind == Tok::Ident {
            // Name name | Name[] name | Name<...> name | a.b.c name
            let mut i = 1;
            // qualified type: seg . seg . seg
            while self.nxt(i).kind == Tok::Ident && self.nxt(i + 1).is_punct(".") {
                i += 2; // skip segment and dot
            }
            // generic type: Name<...> name
            if self.nxt(i).is_punct("<") {
                return true;
            }
            // array type: Name[] name
            if self.nxt(i).is_punct("[") {
                return true;
            }
            // declaration iff the token after the type is an identifier
            return self.nxt(i).kind == Tok::Ident;
        }
        false
    }

    fn parse_for(&mut self) -> PResult<Option<Stmt>> {
        self.bump(); // for
        self.expect_punct("(")?;
        // for-each: [Type] name : expr
        // try: parse type then ident then ':'
        let save = self.pos;
        if !self.at_punct(";") {
            let try_ty = self.parse_type();
            if let Ok(ty) = try_ty {
                if self.cur().kind == Tok::Ident && self.nxt(1).is_punct(":") {
                    let var = self.bump().text;
                    self.bump(); // :
                    let iter = self.parse_expr()?;
                    self.expect_punct(")")?;
                    let body = self.parse_stmt()?.unwrap_or(Stmt::Pass);
                    let body = if let Stmt::Block(b) = body { b } else { vec![body] };
                    return Ok(Some(Stmt::ForEach {
                        var,
                        var2: None,
                        var_ty: ty,
                        iter,
                        body,
                    }));
                }
                // array var decl in classic for? restore
            }
        }
        self.pos = save;
        // classic for (the empty-init branch consumes its own ';')
        let init: Vec<Stmt> = if self.at_punct(";") {
            self.bump();
            vec![]
        } else if self.is_type_lookahead() {
            let ty = self.parse_type()?;
            let mut stmts = Vec::new();
            loop {
                let name = self.expect_ident()?;
                let init_e = if self.eat_punct("=") { Some(self.parse_expr()?) } else { None };
                stmts.push(Stmt::Local { name, ty: ty.clone(), init: init_e, mutable: true });
                if self.eat_punct(",") {
                    continue;
                }
                break;
            }
            self.expect_punct(";")?;
            stmts
        } else {
            let e = self.parse_expr()?;
            self.expect_punct(";")?;
            vec![Stmt::Expr(e)]
        };
        let cond = if self.at_punct(";") { None } else { Some(self.parse_expr()?) };
        self.expect_punct(";")?;
        let mut step = Vec::new();
        if !self.at_punct(")") {
            let e = self.parse_expr()?;
            step.push(Stmt::Expr(e));
        }
        self.expect_punct(")")?;
        let body = self.parse_stmt()?.unwrap_or(Stmt::Pass);
        let body = if let Stmt::Block(b) = body { b } else { vec![body] };
        Ok(Some(Stmt::For { init, cond, step, body }))
    }

    fn parse_switch(&mut self) -> PResult<Option<Stmt>> {
        self.bump(); // switch
        if self.at_punct("(") {
            self.bump();
            // possible switch on pattern (Java 17) unsupported
            let subject = self.parse_expr()?;
            self.expect_punct(")")?;
            self.expect_punct("{")?;
            return self.parse_switch_body(subject);
        }
        Err(self.unsupported("switch expressions"))
    }

    fn parse_switch_body(&mut self, subject: Expr) -> PResult<Option<Stmt>> {
        let mut arms: Vec<MatchArm> = Vec::new();
        let mut default: Option<Block> = None;
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of switch"));
            }
            if self.eat_kw("case") {
                let mut pats = vec![self.parse_expr()?];
                while self.eat_punct(",") {
                    pats.push(self.parse_expr()?);
                }
                self.expect_punct(":")?;
                let mut body = Vec::new();
                while !self.at_kw("case") && !self.at_kw("default") && !self.at_punct("}") {
                    if self.cur().kind == Tok::Eof {
                        return Err(self.err("unexpected end of case body"));
                    }
                    if let Some(s) = self.parse_stmt()? {
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
                if !has_exit && !body.is_empty() {
                    return Err(self.unsupported("switch fallthrough"));
                }
                arms.push(MatchArm { pattern: pats, guard: None, body });
                continue;
            }
            if self.eat_kw("default") {
                self.expect_punct(":")?;
                let mut body = Vec::new();
                while !self.at_kw("case") && !self.at_kw("default") && !self.at_punct("}") {
                    if let Some(s) = self.parse_stmt()? {
                        body.push(s);
                    }
                }
                if matches!(body.last(), Some(Stmt::Break)) {
                    body.pop();
                }
                default = Some(body);
                continue;
            }
            return Err(self.err("malformed switch"));
        }
        self.expect_punct("}")?;
        Ok(Some(Stmt::Match { subject, arms, default }))
    }

    fn parse_try(&mut self) -> PResult<Option<Stmt>> {
        self.bump(); // try
        // try-with-resources
        if self.at_punct("(") {
            return Err(self.unsupported("try-with-resources"));
        }
        let body = self.parse_block()?;
        let mut catches = Vec::new();
        while self.at_kw("catch") {
            self.bump();
            self.expect_punct("(")?;
            if self.eat_kw("final") {}
            let mut var = None;
            let mut ty = None;
            // types, possibly union
            let mut first = true;
            loop {
                if self.at_punct(")") {
                    break;
                }
                if !first && self.eat_punct("|") {
                    let _ = self.parse_type()?;
                    continue;
                }
                first = false;
                if self.cur().kind == Tok::Ident && self.nxt(1).is_punct(")") {
                    var = Some(self.bump().text);
                    break;
                }
                ty = Some(self.parse_type()?);
                if self.cur().kind == Tok::Ident {
                    var = Some(self.bump().text);
                    break;
                }
                if !self.eat_punct("|") {
                    break;
                }
            }
            self.expect_punct(")")?;
            let cbody = self.parse_block()?;
            catches.push(CatchClause { var, ty, body: cbody });
        }
        let mut finally = None;
        if self.eat_kw("finally") {
            finally = Some(self.parse_block()?);
        }
        Ok(Some(Stmt::Try { body, catches, finally }))
    }

    // ---- expressions ------------------------------------------------------------
    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_assign()
    }

    fn parse_assign(&mut self) -> PResult<Expr> {
        // lambda?
        if let Some(l) = self.try_parse_lambda()? {
            return Ok(l);
        }
        let lhs = self.parse_ternary()?;
        const OPS: &[(&str, BinOp)] = &[
            ("+=", BinOp::Add),
            ("-=", BinOp::Sub),
            ("*=", BinOp::Mul),
            ("/=", BinOp::Div),
            ("%=", BinOp::Mod),
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
                let value = self.parse_assign()?;
                return Ok(Expr::AssignExpr {
                    target: Box::new(lhs),
                    op: Some(*op),
                    value: Box::new(value),
                });
            }
        }
        if self.at_punct("=") {
            self.bump();
            let value = self.parse_assign()?;
            return Ok(Expr::AssignExpr {
                target: Box::new(lhs),
                op: None,
                value: Box::new(value),
            });
        }
        Ok(lhs)
    }

    fn try_parse_lambda(&mut self) -> PResult<Option<Expr>> {
        // (params) -> or single ident ->
        if self.at_punct("(") {
            // find matching paren then '->'
            let mut depth = 0;
            let mut i = self.pos;
            let mut arrow = false;
            while i < self.toks.len() {
                if self.toks[i].is_punct("(") {
                    depth += 1;
                } else if self.toks[i].is_punct(")") {
                    depth -= 1;
                    if depth == 0 {
                        arrow = self.toks.get(i + 1).map_or(false, |t| t.is_punct("->"));
                        break;
                    }
                }
                i += 1;
            }
            if arrow {
                let (params, _) = self.parse_params()?;
                self.expect_punct("->")?;
                return Ok(Some(self.parse_lambda_body(params)?));
            }
            return Ok(None);
        }
        if self.cur().kind == Tok::Ident && self.nxt(1).is_punct("->") {
            let name = self.bump().text;
            self.bump();
            let params = vec![Param::new(&name, Ty::Any)];
            return Ok(Some(self.parse_lambda_body(params)?));
        }
        Ok(None)
    }

    fn parse_lambda_body(&mut self, params: Vec<Param>) -> PResult<Expr> {
        if self.at_punct("{") {
            let body = self.parse_block()?;
            Ok(Expr::Closure { params, body, value: None })
        } else {
            let value = self.parse_expr()?;
            Ok(Expr::Closure { params, body: vec![], value: Some(Box::new(value)) })
        }
    }

    fn parse_ternary(&mut self) -> PResult<Expr> {
        let cond = self.binary_level(0)?;
        if self.eat_punct("?") {
            let then = self.parse_expr()?;
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
            &[("||", BinOp::Or)],
            &[("&&", BinOp::And)],
            &[("|", BinOp::BitOr)],
            &[("^", BinOp::BitXor)],
            &[("&", BinOp::BitAnd)],
            &[("==", BinOp::Eq), ("!=", BinOp::Ne)],
            &[
                ("<", BinOp::Lt),
                ("<=", BinOp::Le),
                (">", BinOp::Gt),
                (">=", BinOp::Ge),
                ("instanceof", BinOp::Eq),
            ],
            &[("<<", BinOp::Shl), (">>", BinOp::Shr), (">>>", BinOp::UShr)],
            &[("+", BinOp::Add), ("-", BinOp::Sub)],
            &[("*", BinOp::Mul), ("/", BinOp::Div), ("%", BinOp::Mod)],
        ];
        if level >= LEVELS.len() {
            return self.parse_unary();
        }
        let mut l = self.binary_level(level + 1)?;
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
            if text == "instanceof" {
                let ty = self.parse_type()?;
                let _ = ty;
                l = Expr::call(Expr::ident("__instanceof__"), vec![l]);
                continue;
            }
            let r = self.binary_level(level + 1)?;
            l = Expr::binary(op, l, r);
        }
        Ok(l)
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
        // cast: (Type) expr — distinguishable by lookahead
        if self.at_punct("(") && self.cast_lookahead() {
            self.bump();
            let ty = self.parse_type()?;
            self.expect_punct(")")?;
            let e = self.parse_unary()?;
            return Ok(Expr::Cast { e: Box::new(e), ty });
        }
        self.parse_postfix()
    }

    fn cast_lookahead(&self) -> bool {
        // ( Ident | prim ) ... ) followed by ident/literal/( for the operand
        let n1 = self.nxt(1);
        let prim = matches!(
            n1.text.as_str(),
            "int" | "long" | "short" | "byte" | "float" | "double" | "boolean" | "char"
        );
        if prim {
            return self.nxt(2).is_punct(")");
        }
        if n1.kind == Tok::Ident {
            // (Name) or (Name<...>) or (Name[]) then operand
            let mut i = 2;
            if self.nxt(i).is_punct("<") {
                return true; // generic cast
            }
            while self.nxt(i).kind == Tok::Ident && self.nxt(i + 1).is_punct(".") {
                i += 2;
            }
            // (qualified.Name) expr
            if self.nxt(i).is_punct(")") {
                // name cast if followed by operand start (ident, literal, '(')
                let after = self.nxt(i + 1);
                return matches!(
                    after.kind,
                    Tok::Ident | Tok::Int | Tok::Float | Tok::Str | Tok::Char
                ) || after.is_punct("(")
                    || after.is_kw("new")
                    || after.is_kw("this")
                    || after.is_kw("super");
            }
        }
        false
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            if self.at_punct(".") {
                self.bump();
                if self.at_kw("new") {
                    // qualified new — unsupported
                    return Err(self.unsupported("qualified class instance creation"));
                }
                if self.at_kw("this") {
                    self.bump();
                    self.expect_punct("(")?;
                    let args = self.parse_args_rest()?;
                    // X.this(...) -> this(...) call
                    e = Expr::call(Expr::ident("__ctor_call__"), args);
                    continue;
                }
                if self.at_kw("class") {
                    self.bump();
                    continue;
                }
                if self.at_kw("super") {
                    self.bump();
                    if self.eat_punct("(") {
                        let args = self.parse_args_rest()?;
                        e = Expr::call(Expr::ident("__super_ctor__"), args);
                    } else if self.eat_punct(".") {
                        let name = self.expect_ident()?;
                        let args = if self.at_punct("(") {
                            self.expect_punct("(")?;
                            self.parse_args_rest()?
                        } else {
                            vec![]
                        };
                        e = Expr::Method {
                            obj: Box::new(Expr::Super),
                            name,
                            args,
                            nullsafe: false,
                        };
                    }
                    continue;
                }
                let name = if self.cur().kind == Tok::Ident || self.cur().kind == Tok::Keyword {
                    self.bump().text
                } else {
                    return Err(self.err("expected member name"));
                };
                e = Expr::Field { obj: Box::new(e), name, nullsafe: false };
                continue;
            }
            if self.at_punct("(") {
                self.bump(); // (
                let args = self.parse_args_rest()?;
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
            if self.at_punct("++") || self.at_punct("--") {
                let is_inc = self.at_punct("++");
                self.bump();
                e = Expr::Post {
                    op: if is_inc { UnOp::PreInc } else { UnOp::PreDec },
                    e: Box::new(e),
                };
                continue;
            }
            if self.at_punct("::") {
                return Err(self.unsupported("method references"));
            }
            break;
        }
        Ok(e)
    }

    fn parse_args_rest(&mut self) -> PResult<Vec<Expr>> {
        let mut args = Vec::new();
        while !self.at_punct(")") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of arguments"));
            }
            args.push(self.parse_expr()?);
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
                    .ok_or_else(|| self.err(format!("bad integer '{}'", t.text)))?;
                Ok(Expr::Int(v))
            }
            Tok::Float => {
                self.bump();
                let v = parse_float_text(&t.text)
                    .ok_or_else(|| self.err(format!("bad float '{}'", t.text)))?;
                Ok(Expr::Float(v))
            }
            Tok::Str => {
                self.bump();
                Ok(Expr::Str(t.text))
            }
            Tok::Char => {
                self.bump();
                Ok(Expr::Str(t.text))
            }
            Tok::Ident => {
                self.bump();
                if self.at_punct("(") {
                    // generic method call: name<...>(...)
                    // handled via postfix: not needed here
                }
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
                    // array creation: new int[10]
                    let ty = self.parse_type()?;
                    if self.at_punct("[") {
                        self.bump();
                        let size = self.parse_expr()?;
                        self.expect_punct("]")?;
                        // extra empty dims
                        while self.at_punct("[") && self.nxt(1).is_punct("]") {
                            self.bump();
                            self.bump();
                        }
                        return Ok(Expr::call(
                            Expr::ident("__new_array__"),
                            vec![size],
                        ));
                    }
                    if self.at_punct("<") {
                        self.skip_generics()?;
                    }
                    // qualified: new a.b.C()
                    let mut cname = match &ty {
                        Ty::Class(n) => n.clone(),
                        Ty::Str => "String".to_string(),
                        _ => "Object".to_string(),
                    };
                    while self.eat_punct(".") {
                        let seg = self.expect_ident()?;
                        cname = seg; // use last segment
                    }
                    if self.at_punct("(") {
                        self.bump();
                        let args = self.parse_args_rest()?;
                        return Ok(Expr::New { class: cname, args });
                    }
                    if self.at_punct("{") {
                        // anonymous class or array init body
                        if self.at_punct("{") {
                            let _ = self.parse_array_init()?;
                        }
                        return Ok(Expr::New { class: cname, args: vec![] });
                    }
                    Err(self.err("malformed new expression"))
                }
                _ => Err(self.err(format!("unexpected keyword '{}'", t.text))),
            },
            Tok::Punct => {
                if t.text == "(" {
                    self.bump();
                    let e = self.parse_expr()?;
                    self.expect_punct(")")?;
                    return Ok(Expr::Paren(Box::new(e)));
                }
                Err(self.err(format!("unexpected token '{}'", t.text)))
            }
            _ => Err(self.err(format!("unexpected token '{}'", t.text))),
        }
    }
}

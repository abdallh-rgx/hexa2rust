#![forbid(unsafe_code)]
//! Go front-end: hand-written lexer + recursive-descent parser lowering to the
//! unified IR. Covers a practical subset: packages, imports, var/const, type
//! declarations (struct/interface), functions and methods, control flow,
//! slices/maps/composite literals, range loops, defer (approximated), go
//! (cooperative single-threaded scheduling at runtime), generics (type
//! parameters parsed and erased).

use transpiler_core::diag::{Diag, PResult};
use transpiler_core::ir::*;
use transpiler_core::lexer::*;

const GO_KEYWORDS: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough",
    "for", "func", "go", "goto", "if", "import", "interface", "map", "package", "range",
    "return", "select", "struct", "switch", "type", "var",
];

const GO_PUNCTS: &[&str] = &[
    "<<=", ">>=", "&^=", "...", "&&", "||", "==", "!=", "<-", ":=", "<<", ">>", "&^", "<=",
    ">=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "++", "--", "+", "-", "*", "/", "%",
    "=", "<", ">", "!", "~", "&", "|", "^", "(", ")", "[", "]", "{", "}", ",", ":", ".", ";",
];

pub fn go_lexer_config() -> LexerConfig {
    LexerConfig {
        keywords: GO_KEYWORDS.to_vec(),
        puncts: GO_PUNCTS.to_vec(),
        backtick: Backtick::RawString,
        dollar_strings: false,
        char_literals: true,
    }
}

pub struct GoParser {
    toks: Vec<Token>,
    pos: usize,
    file: String,
    imports: Vec<String>,
    classes: Vec<Decl>,
    /// Ident{...} composite literals allowed (false in block-statement position)
    allow_composite: bool,
    type_names: std::collections::HashSet<String>,
    /// methods to attach to classes after parsing
    pending_methods: Vec<(String, FunctionDecl)>,
}

/// Parse a Go source file into the unified IR.
pub fn parse_go(src: &str, file: &str) -> PResult<Module> {
    let cfg = go_lexer_config();
    let toks = Lexer::new(src, &cfg).tokenize()?;
    let mut p = GoParser {
        toks,
        pos: 0,
        file: file.to_string(),
        imports: vec![],
        classes: vec![],
        allow_composite: true,
        type_names: Default::default(),
        pending_methods: vec![],
    };
    p.parse_file()
}

impl GoParser {
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
        if self.cur().kind == Tok::Ident || self.cur().kind == Tok::Keyword && !self.at_kw("func") {
            Ok(self.bump().text)
        } else {
            Err(self.err(format!("expected identifier, found '{}'", self.cur().text)))
        }
    }
    fn err(&self, msg: impl Into<String>) -> Diag {
        Diag::new(self.cur().line, self.cur().col, msg.into()).at(&self.file)
    }
    fn unsupported(&self, what: &str) -> Diag {
        self.err(format!("unsupported Go syntax: {}", what))
    }
    fn expect_semi(&mut self) -> PResult<()> {
        if self.eat_punct(";") {
            return Ok(());
        }
        if self.cur().kind == Tok::Eof || self.at_punct("}") {
            return Ok(());
        }
        // Go automatic semicolon insertion: newline ends the statement
        let prev = &self.toks[self.pos.saturating_sub(1)];
        if self.cur().line > prev.line {
            return Ok(());
        }
        Err(self.err(format!("expected ';' or newline, found '{}'", self.cur().text)))
    }

    // ---- file ------------------------------------------------------------
    fn parse_file(&mut self) -> PResult<Module> {
        // package clause
        let mut name = "main".to_string();
        if self.eat_kw("package") {
            name = self.expect_ident()?;
            self.eat_punct(";");
        }
        // imports
        while self.at_kw("import") {
            self.bump();
            if self.eat_punct("(") {
                while !self.at_punct(")") {
                    if self.cur().kind == Tok::Eof {
                        return Err(self.err("unexpected end of imports"));
                    }
                    if self.cur().kind == Tok::Str || self.cur().kind == Tok::RawStr {
                        let path = self.bump().text;
                        self.imports.push(path);
                    } else {
                        self.bump(); // alias or dot
                    }
                }
                self.expect_punct(")")?;
            } else {
                // import alias? "path"
                if self.cur().kind != Tok::Str && self.cur().kind != Tok::RawStr {
                    self.bump();
                }
                if self.cur().kind == Tok::Str || self.cur().kind == Tok::RawStr {
                    let path = self.bump().text;
                    self.imports.push(path);
                }
                self.expect_semi()?;
            }
        }
        let mut decls: Vec<Decl> = Vec::new();
        let mut main_body: Block = Vec::new();
        while self.cur().kind != Tok::Eof {
            match self.parse_top_decl()? {
                Some(item) => match item {
                    TopItem::Decl(d) => decls.push(d),
                    TopItem::Stmt(s) => main_body.push(s),
                    TopItem::Method(ty, m) => self.pending_methods.push((ty, m)),
                },
                None => {}
            }
        }
        // attach pending methods
        let mut classes = std::mem::take(&mut self.classes);
        for (ty, m) in std::mem::take(&mut self.pending_methods) {
            let idx = classes
                .iter()
                .position(|c| matches!(c, Decl::Class(cls) if cls.name == ty));
            match idx {
                Some(i) => {
                    if let Decl::Class(cls) = &mut classes[i] {
                        cls.methods.push(m);
                    }
                }
                None => {
                    // method on unknown type: create a shell class
                    classes.push(Decl::Class(ClassDecl {
                        name: ty,
                        extends: None,
                        fields: vec![],
                        methods: vec![m],
                        ctor: None,
                        kind: ClassKind::Class,
                        is_data: false,
                    }));
                }
            }
        }
        let mut all = classes;
        all.append(&mut decls);
        if !main_body.is_empty() {
            all.push(Decl::Function(FunctionDecl::new("__main__", vec![], Ty::Void, main_body)));
        }
        Ok(Module {
            lang: Lang::Go,
            name,
            imports: std::mem::take(&mut self.imports),
            decls: all,
        })
    }

    fn parse_top_decl(&mut self) -> PResult<Option<TopItem>> {
        if self.eat_kw("func") {
            // method with receiver?
            if self.at_punct("(") {
                self.bump(); // (
                if self.eat_punct("*") {}
                let first = self.expect_ident()?;
                // Go receivers are (name Type); handle bare (Type) too
                let recv_ty = if self.cur().kind == Tok::Ident {
                    let _name = first;
                    self.bump().text // the type follows the name
                } else {
                    first // (Type) without a receiver name
                };
                self.expect_punct(")")?;
                let fname = self.expect_ident()?;
                if self.at_punct("[") {
                    self.skip_brackets()?;
                }
                let f = self.parse_func_rest(Some(fname))?;
                return Ok(Some(TopItem::Method(recv_ty, f)));
            }
            let fname = self.expect_ident()?;
            // generics: func Name[T any](...)
            if self.at_punct("[") {
                self.skip_brackets()?;
            }
            let f = self.parse_func_rest(Some(fname))?;
            return Ok(Some(TopItem::Decl(Decl::Function(f))));
        }
        if self.at_kw("type") {
            self.bump();
            let tname = self.expect_ident()?;
            self.type_names.insert(tname.clone());
            if self.at_punct("[") {
                self.skip_brackets()?; // generic type params
            }
            if self.eat_kw("struct") {
                let c = self.parse_struct_body(tname)?;
                self.classes.push(Decl::Class(c.clone()));
                return Ok(Some(TopItem::Decl(Decl::Class(c))));
            }
            if self.eat_kw("interface") {
                let c = self.parse_interface_body(tname)?;
                self.classes.push(Decl::Class(c.clone()));
                return Ok(Some(TopItem::Decl(Decl::Class(c))));
            }
            // type alias / defined type
            let ty = self.parse_type()?;
            self.classes.push(Decl::Class(ClassDecl {
                name: tname.clone(),
                extends: None,
                fields: vec![],
                methods: vec![],
                ctor: None,
                kind: ClassKind::Class,
                is_data: false,
            }));
            return Ok(Some(TopItem::Decl(Decl::TypeAlias { name: tname, ty })));
        }
        if self.at_kw("var") || self.at_kw("const") {
            let is_const = self.at_kw("const");
            self.bump();
            let mut out = Vec::new();
            if self.eat_punct("(") {
                while !self.at_punct(")") {
                    if self.cur().kind == Tok::Eof {
                        return Err(self.err("unexpected end of var block"));
                    }
                    let mut group = self.parse_var_spec(is_const)?;
                    out.append(&mut group);
                    self.eat_punct(";");
                }
                self.expect_punct(")")?;
            } else {
                let mut group = self.parse_var_spec(is_const)?;
                out.append(&mut group);
                self.expect_semi()?;
            }
            return Ok(Some(TopItem::Stmt(Stmt::Block(out))));
        }
        if self.eat_kw("go") {
            let e = self.parse_expr()?;
            self.expect_semi()?;
            return Ok(Some(TopItem::Stmt(Stmt::Spawn(e))));
        }
        // statement
        match self.parse_stmt()? {
            Some(s) => Ok(Some(TopItem::Stmt(s))),
            None => Ok(None),
        }
    }

    fn skip_brackets(&mut self) -> PResult<()> {
        self.expect_punct("[")?;
        let mut depth = 1;
        while depth > 0 {
            let t = self.bump();
            if t.kind == Tok::Eof {
                return Err(self.err("unexpected end of type parameters"));
            }
            if t.is_punct("[") {
                depth += 1;
            }
            if t.is_punct("]") {
                depth -= 1;
            }
        }
        Ok(())
    }

    fn parse_var_spec(&mut self, is_const: bool) -> PResult<Vec<Stmt>> {
        let mut names = vec![self.expect_ident()?];
        while self.eat_punct(",") {
            names.push(self.expect_ident()?);
        }
        let mut ty = Ty::Any;
        if !self.at_punct("=") && !self.at_punct(":=") {
            ty = self.parse_type()?;
        }
        let mut inits = Vec::new();
        if self.eat_punct("=") {
            loop {
                inits.push(self.parse_expr()?);
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        if inits.iter().any(|e| matches!(e, Expr::Ident(n) if n == "iota")) {
            return Err(self.unsupported("iota"));
        }
        if names.len() > 1 && inits.len() == 1 {
            // a, b := f() multi-return: unsupported
            if matches!(inits[0], Expr::Call { .. }) {
                return Err(self.unsupported("multiple return values"));
            }
            return Err(self.unsupported("multi-name declaration with single init"));
        }
        let mut out = Vec::new();
        for (i, n) in names.into_iter().enumerate() {
            out.push(Stmt::Local {
                name: n,
                ty: ty.clone(),
                init: inits.get(i).cloned(),
                mutable: !is_const,
            });
        }
        Ok(out)
    }

    fn parse_struct_body(&mut self, name: String) -> PResult<ClassDecl> {
        self.expect_punct("{")?;
        let mut cls = ClassDecl {
            name,
            extends: None,
            fields: Vec::new(),
            methods: Vec::new(),
            ctor: None,
            kind: ClassKind::Struct,
            is_data: false,
        };
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of struct"));
            }
            self.eat_punct(";");
            if self.at_punct("}") {
                break;
            }
            // embedded field: just a type name
            if self.cur().kind == Tok::Ident
                && (self.nxt(1).is_punct(";") || self.nxt(1).is_punct("}") || self.nxt(1).is_punct("\n"))
                || (self.cur().kind == Tok::Ident && self.nxt(1).kind == Tok::Ident && (self.nxt(2).is_punct(";") || self.nxt(2).is_punct("}")))
            {
                // field name + type
                let fname = self.bump().text;
                let fty = self.parse_type()?;
                cls.fields.push(FieldDecl {
                    name: fname,
                    ty: fty,
                    init: None,
                    is_static: false,
                    mutable: true,
                    visibility: Visibility::Public,
                });
                self.eat_punct(";");
                continue;
            }
            // try: embedded type
            if self.cur().kind == Tok::Ident {
                let tyname = self.cur().text.clone();
                // embedded (no field name)
                if self.nxt(1).is_punct(";") || self.nxt(1).is_punct("}") {
                    self.bump();
                    cls.fields.push(FieldDecl {
                        name: tyname.to_lowercase(),
                        ty: Ty::class(&tyname),
                        init: None,
                        is_static: false,
                        mutable: true,
                        visibility: Visibility::Public,
                    });
                    self.eat_punct(";");
                    continue;
                }
                let fname = self.bump().text;
                let fty = self.parse_type()?;
                cls.fields.push(FieldDecl {
                    name: fname,
                    ty: fty,
                    init: None,
                    is_static: false,
                    mutable: true,
                    visibility: Visibility::Public,
                });
                self.eat_punct(";");
                continue;
            }
            return Err(self.unsupported("struct member"));
        }
        self.expect_punct("}")?;
        Ok(cls)
    }

    fn parse_interface_body(&mut self, name: String) -> PResult<ClassDecl> {
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
            self.eat_punct(";");
            if self.at_punct("}") {
                break;
            }
            if self.eat_kw("interface") {
                // embedded interface
                let _ = self.expect_ident()?;
                self.eat_punct(";");
                continue;
            }
            let mname = self.expect_ident()?;
            if self.at_punct("[") {
                self.skip_brackets()?;
            }
            if self.at_punct("(") {
                let (params, _) = self.parse_params()?;
                let ret = self.parse_results()?;
                cls.methods.push(FunctionDecl::new(&mname, params, ret, vec![]));
                self.eat_punct(";");
            } else {
                // embedded type name
                cls.fields.push(FieldDecl {
                    name: mname.to_lowercase(),
                    ty: Ty::class(&mname),
                    init: None,
                    is_static: false,
                    mutable: true,
                    visibility: Visibility::Public,
                });
                self.eat_punct(";");
            }
        }
        self.expect_punct("}")?;
        Ok(cls)
    }

    fn parse_func_rest(&mut self, name: Option<String>) -> PResult<FunctionDecl> {
        let (params, variadic_name) = self.parse_params()?;
        let ret = self.parse_results()?;
        let mut body = Vec::new();
        if self.at_punct("{") {
            body = self.parse_block()?;
        } else {
            self.expect_semi()?;
        }
        let fname = name.unwrap_or_else(|| "anonymous".to_string());
        let mut f = FunctionDecl::new(&fname, params, ret, body);
        if let Some(v) = variadic_name {
            f.lang_name = Some(v);
        }
        Ok(f)
    }

    /// returns params and optional variadic param name (Go `...T`)
    fn parse_params(&mut self) -> PResult<(Vec<Param>, Option<String>)> {
        self.expect_punct("(")?;
        let mut params = Vec::new();
        let mut variadic = None;
        while !self.at_punct(")") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of parameters"));
            }
            // name type | type (unnamed)
            let mut names = Vec::new();
            while self.cur().kind == Tok::Ident && !self.is_type_start() {
                names.push(self.bump().text);
                if !self.eat_punct(",") {
                    break;
                }
            }
            if self.eat_punct("...") {
                let ty = self.parse_type()?;
                if names.len() != 1 {
                    return Err(self.unsupported("unnamed variadic"));
                }
                params.push(Param {
                    name: names[0].clone(),
                    ty,
                    default: None,
                    variadic: true,
                });
                variadic = Some(names[0].clone());
                break;
            }
            let ty = self.parse_type()?;
            if names.is_empty() {
                params.push(Param::new(&format!("$p{}", params.len()), ty));
            } else {
                for n in names {
                    params.push(Param::new(&n, ty.clone()));
                }
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct(")")?;
        Ok((params, variadic))
    }

    /// is the current token a type start (vs parameter name)? Heuristic:
    /// `name Type` vs `Type` — if the NEXT token continues a type it is unnamed.
    fn is_type_start(&self) -> bool {
        // qualified package type: `time.Duration`
        if self.cur().kind == Tok::Ident && self.nxt(1).is_punct(".") {
            return true;
        }
        matches!(
            self.cur().text.as_str(),
            "map" | "chan" | "func" | "interface" | "struct" | "any" | "string" | "int"
                | "int8" | "int16" | "int32" | "int64" | "uint" | "uint8" | "uint16" | "uint32"
                | "uint64" | "uintptr" | "float32" | "float64" | "bool" | "byte" | "rune"
                | "error" | "complex64" | "complex128"
        )
    }

    fn parse_results(&mut self) -> PResult<Ty> {
        if self.at_punct("(") {
            self.bump();
            let mut tys = Vec::new();
            while !self.at_punct(")") {
                if self.cur().kind == Tok::Eof {
                    return Err(self.err("unexpected end of results"));
                }
                // named results: name Type | Type
                while self.cur().kind == Tok::Ident && !self.is_type_start() && !self.nxt(1).is_punct(")")
                    && !self.nxt(1).is_punct(",")
                {
                    self.bump();
                }
                tys.push(self.parse_type()?);
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.expect_punct(")")?;
            if tys.len() > 1 {
                // multiple return values: modelled as a dynamic Value array
                return Ok(Ty::Any);
            }
            return Ok(tys.into_iter().next().unwrap_or(Ty::Void));
        }
        if self.at_punct("{") || self.at_punct(";") || self.cur().kind == Tok::Eof {
            return Ok(Ty::Void);
        }
        self.parse_type()
    }

    // ---- types -------------------------------------------------------------
    fn parse_type(&mut self) -> PResult<Ty> {
        if self.at_punct("[") {
            // []T or [N]T
            self.bump();
            if self.eat_punct("]") {
                let el = self.parse_type()?;
                return Ok(Ty::Array(Box::new(el)));
            }
            // fixed size: skip length
            let mut depth = 0;
            loop {
                let t = self.bump();
                if t.kind == Tok::Eof {
                    return Err(self.err("unexpected end of array type"));
                }
                if t.is_punct("[") {
                    depth += 1;
                }
                if t.is_punct("]") {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
            }
            let el = self.parse_type()?;
            return Ok(Ty::Array(Box::new(el)));
        }
        if self.at_punct("*") {
            self.bump();
            let inner = self.parse_type()?;
            return Ok(inner); // pointers modelled as reference cells
        }
        if self.eat_kw("map") {
            self.expect_punct("[")?;
            let k = self.parse_type()?;
            self.expect_punct("]")?;
            let v = self.parse_type()?;
            return Ok(Ty::Map(Box::new(k), Box::new(v)));
        }
        if self.eat_kw("chan") {
            return Err(self.unsupported("channels"));
        }
        if self.eat_kw("func") {
            if self.at_punct("(") {
                let (_, _) = self.parse_params()?;
                let _ = self.parse_results()?;
            }
            return Ok(Ty::Any);
        }
        if self.eat_kw("interface") {
            if self.at_punct("{") {
                let mut depth = 0;
                loop {
                    let t = self.bump();
                    if t.kind == Tok::Eof {
                        return Err(self.err("unterminated interface type"));
                    }
                    if t.is_punct("{") {
                        depth += 1;
                    }
                    if t.is_punct("}") {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            }
            return Ok(Ty::Any);
        }
        if self.eat_kw("struct") {
            if self.at_punct("{") {
                let mut depth = 0;
                loop {
                    let t = self.bump();
                    if t.kind == Tok::Eof {
                        return Err(self.err("unterminated struct type"));
                    }
                    if t.is_punct("{") {
                        depth += 1;
                    }
                    if t.is_punct("}") {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            }
            return Ok(Ty::Any);
        }
        if self.at_kw("any") {
            self.bump();
            return Ok(Ty::Any);
        }
        if self.cur().kind == Tok::Ident || self.cur().kind == Tok::Keyword {
            let mut name = self.bump().text;
            // qualified type: pkg.Type -> use the last segment
            while self.at_punct(".") {
                self.bump();
                name = self.expect_ident()?;
            }
            // generic instantiation List[T]
            if self.at_punct("[") {
                self.skip_brackets()?;
                return Ok(Ty::Any);
            }
            return Ok(map_go_type(&name));
        }
        Err(self.err(format!("expected type, found '{}'", self.cur().text)))
    }

    // ---- statements ----------------------------------------------------------
    fn parse_stmt(&mut self) -> PResult<Option<Stmt>> {
        let t = self.cur().clone();
        if t.is_punct(";") {
            self.bump();
            return Ok(None);
        }
        if t.is_punct("{") {
            return Ok(Some(Stmt::Block(self.parse_block()?)));
        }
        if t.kind == Tok::Keyword {
            match t.text.as_str() {
                "if" => return self.parse_if(),
                "for" => return self.parse_for(),
                "switch" => return self.parse_switch(),
                "select" => return Err(self.unsupported("select")),
                "return" => {
                    self.bump();
                    let v = if self.at_punct("}") || self.at_punct(";") || self.cur().kind == Tok::Eof {
                        None
                    } else {
                        let first = self.parse_expr()?;
                        if self.at_punct(",") {
                            // multiple return values -> Value array
                            let mut items = vec![first];
                            while self.eat_punct(",") {
                                if self.at_punct("}") || self.at_punct(";") {
                                    break;
                                }
                                items.push(self.parse_expr()?);
                            }
                            Some(Expr::Array(items))
                        } else {
                            Some(first)
                        }
                    };
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Return(v)));
                }
                "break" => {
                    self.bump();
                    if self.cur().kind == Tok::Ident {
                        self.bump(); // label
                    }
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Break));
                }
                "continue" => {
                    self.bump();
                    if self.cur().kind == Tok::Ident {
                        self.bump();
                    }
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Continue));
                }
                "goto" => return Err(self.unsupported("goto")),
                "fallthrough" => return Err(self.unsupported("fallthrough")),
                "defer" => {
                    self.bump();
                    let e = self.parse_expr()?;
                    self.expect_semi()?;
                    // defer f(args) -> Defer block
                    if let Expr::Call { callee, args } = e {
                        return Ok(Some(Stmt::Defer(vec![Stmt::Expr(Expr::Call {
                            callee,
                            args,
                        })])));
                    }
                    return Err(self.unsupported("defer of non-call"));
                }
                "go" => {
                    self.bump();
                    let e = self.parse_expr()?;
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Spawn(e)));
                }
                "var" | "const" => {
                    let is_const = self.at_kw("const");
                    self.bump();
                    let mut out = self.parse_var_spec(is_const)?;
                    self.expect_semi()?;
                    if out.len() == 1 {
                        return Ok(Some(out.pop().unwrap()));
                    }
                    return Ok(Some(Stmt::Block(out)));
                }
                "type" => {
                    // local type decl
                    self.bump();
                    let tname = self.expect_ident()?;
                    if self.at_punct("[") {
                        self.skip_brackets()?;
                    }
                    if self.eat_kw("struct") {
                        let c = self.parse_struct_body(tname)?;
                        self.classes.push(Decl::Class(c));
                    } else if self.eat_kw("interface") {
                        let c = self.parse_interface_body(tname)?;
                        self.classes.push(Decl::Class(c));
                    } else {
                        let ty = self.parse_type()?;
                        self.classes.push(Decl::Class(ClassDecl {
                            name: tname,
                            extends: None,
                            fields: vec![],
                            methods: vec![],
                            ctor: None,
                            kind: ClassKind::Class,
                            is_data: false,
                        }));
                        let _ = ty;
                    }
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Pass));
                }
                "func" => {
                    // local function
                    self.bump();
                    let fname = self.expect_ident()?;
                    if self.at_punct("[") {
                        self.skip_brackets()?;
                    }
                    let f = self.parse_func_rest(Some(fname.clone()))?;
                    return Ok(Some(Stmt::Local {
                        name: fname,
                        ty: Ty::Any,
                        init: Some(Expr::Closure {
                            params: f.params,
                            body: f.body,
                            value: None,
                        }),
                        mutable: false,
                    }));
                }
                _ => {}
            }
        }
        // simple statement sequence: expr or assignment or :=
        let first = self.parse_expr()?;
        if self.at_punct(":=") {
            return self.parse_short_var_decl(first);
        }
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
        ];
        for (p, op) in AUG {
            if self.at_punct(p) {
                self.bump();
                let value = self.parse_expr()?;
                self.expect_semi()?;
                return Ok(Some(Stmt::Assign {
                    target: first,
                    op: Some(*op),
                    value,
                }));
            }
        }
        if self.at_punct("=") {
            self.bump();
            let mut targets = vec![first];
            if self.at_kw("range") {
                // handled by for
                return Err(self.err("range in wrong position"));
            }
            let value = self.parse_expr()?;
            let _ = &mut targets;
            self.expect_semi()?;
            return Ok(Some(Stmt::Assign { target: targets.pop().unwrap(), op: None, value }));
        }
        self.expect_semi()?;
        Ok(Some(Stmt::Expr(first)))
    }

    fn parse_if(&mut self) -> PResult<Option<Stmt>> {
        self.bump(); // if
        let mut init = Vec::new();
        let mut precond: Option<Expr> = None;
        if self.at_kw("var") || self.at_kw("const") {
            let is_const = self.at_kw("const");
            self.bump();
            init = self.parse_var_spec(is_const)?;
        } else if !self.at_punct("{") {
            let save = self.pos;
            let e = self.parse_simple()?;
            if self.at_punct(":=") {
                if let Expr::Ident(name) = &e {
                    self.bump();
                    let value = self.parse_simple()?;
                    init.push(Stmt::Local {
                        name: name.clone(),
                        ty: Ty::Any,
                        init: Some(value),
                        mutable: true,
                    });
                } else {
                    return Err(self.unsupported("complex := in if"));
                }
            } else if self.at_punct(",") {
                // v, ok := m[k]
                self.pos = save;
                let names = self.parse_ident_list()?;
                if self.at_punct(":=") {
                    self.bump();
                    if names.len() == 2 {
                        let value = self.parse_expr()?;
                        if let Expr::Index { obj, idx } = value {
                            let obj = *obj;
                            let idx = *idx;
                            let has = Expr::call(
                                Expr::ident("__map_has__"),
                                vec![obj.clone(), idx.clone()],
                            );
                            let get = Expr::index(obj, idx);
                            init.push(Stmt::Local {
                                name: names[0].clone(),
                                ty: Ty::Any,
                                init: Some(get),
                                mutable: true,
                            });
                            init.push(Stmt::Local {
                                name: names[1].clone(),
                                ty: Ty::Any,
                                init: Some(has),
                                mutable: true,
                            });
                        } else {
                            return Err(self.unsupported("multiple return values"));
                        }
                    } else {
                        return Err(self.unsupported("multiple := targets"));
                    }
                } else {
                    return Err(self.unsupported("tuple assignment in if"));
                }
            } else if self.at_punct("{") {
                // no `;` follows: the simple statement WAS the condition
                precond = Some(e);
            } else {
                init.push(Stmt::Expr(e));
            }
        }
        if precond.is_none() && !init.is_empty() {
            self.eat_punct(";"); // Go: `if init; cond {`
        }
        let saved_ac = self.allow_composite;
        self.allow_composite = false;
        let cond = match precond {
            Some(c) => c,
            None => self.parse_expr()?,
        };
        self.allow_composite = saved_ac;
        let then = self.parse_block()?;
        let els = if self.eat_kw("else") {
            if self.at_kw("if") {
                match self.parse_if()? {
                    Some(s) => vec![s],
                    None => vec![],
                }
            } else {
                self.parse_block()?
            }
        } else {
            vec![]
        };
        let mut stmt = Stmt::If { cond, then, els };
        if !init.is_empty() {
            let mut b = init;
            b.push(stmt);
            stmt = Stmt::Block(b);
        }
        Ok(Some(stmt))
    }

    fn parse_for(&mut self) -> PResult<Option<Stmt>> {
        self.bump(); // for
        // forms:
        // for {}
        // for cond {}
        // for init; cond; post {}
        // for k, v := range x {}
        // for i := range x {}
        if self.at_punct("{") {
            let body = self.parse_block()?;
            return Ok(Some(Stmt::While { cond: Expr::Bool(true), body }));
        }
        if self.at_kw("range") {
            self.bump();
            let iter = self.parse_expr()?;
            let body = self.parse_block()?;
            return Ok(Some(Stmt::ForEach {
                var: "$i".into(),
                var2: None,
                var_ty: Ty::Any,
                iter: Expr::call(Expr::ident("__indices__"), vec![iter]),
                body,
            }));
        }
        if self.at_kw("var") || self.at_kw("const") {
            return Err(self.unsupported("var in for init"));
        }
        // try init := or init =
        if (self.cur().kind == Tok::Ident || self.at_punct("("))
            && !self.at_punct("{")
        {
            let save = self.pos;
            let e = self.parse_simple()?;
            if self.at_punct(":=") {
                if let Expr::Ident(name) = &e {
                    self.bump();
                    // range?
                    if self.at_kw("range") {
                        self.bump();
                        let iter = self.parse_expr()?;
                        let body = self.parse_block()?;
                        return Ok(Some(Stmt::ForEach {
                            var: name.clone(),
                            var2: None,
                            var_ty: Ty::Any,
                            iter: Expr::call(Expr::ident("__indices__"), vec![iter]),
                            body,
                        }));
                    }
                    let value = self.parse_simple()?;
                    let init = vec![Stmt::Local {
                        name: name.clone(),
                        ty: Ty::Any,
                        init: Some(value),
                        mutable: true,
                    }];
                    self.expect_punct(";")?;
                    return self.parse_for_rest(Some(init));
                }
                // multi-name: v, ok := m[k]
                self.pos = save;
                let names = self.parse_ident_list()?;
                self.bump(); // :=
                if names.len() == 2 && !self.at_kw("range") {
                    let value = self.parse_expr()?;
                    if let Expr::Index { obj, idx } = value {
                        let obj = *obj;
                        let idx = *idx;
                        let has = Expr::call(
                            Expr::ident("__map_has__"),
                            vec![obj.clone(), idx.clone()],
                        );
                        let get = Expr::index(obj, idx);
                        let init = vec![
                            Stmt::Local {
                                name: names[0].clone(),
                                ty: Ty::Any,
                                init: Some(get),
                                mutable: true,
                            },
                            Stmt::Local {
                                name: names[1].clone(),
                                ty: Ty::Any,
                                init: Some(has),
                                mutable: true,
                            },
                        ];
                        self.expect_punct(";")?;
                        return self.parse_for_rest(Some(init));
                    }
                    return Err(self.unsupported("multiple return values"));
                }
                if names.len() == 2 && self.eat_kw("range") {
                    let iter = self.parse_expr()?;
                    let body = self.parse_block()?;
                    return Ok(Some(Stmt::ForEach {
                        var: names[0].clone(),
                        var2: Some(names[1].clone()),
                        var_ty: Ty::Any,
                        iter,
                        body,
                    }));
                }
                return Err(self.unsupported("multi-assign"));
            }
            if self.at_punct(",") {
                // k, v := range / k, v = ...
                let mut names = match &e {
                    Expr::Ident(n) => vec![n.clone()],
                    _ => return Err(self.unsupported("complex range target")),
                };
                while self.eat_punct(",") {
                    match self.parse_expr()? {
                        Expr::Ident(n) => names.push(n),
                        _ => return Err(self.unsupported("complex range target")),
                    }
                }
                if self.at_punct(":=") {
                    self.bump();
                    if !self.eat_kw("range") {
                        return Err(self.unsupported("multi-assign without range"));
                    }
                    let iter = self.parse_expr()?;
                    let body = self.parse_block()?;
                    if names.len() == 2 {
                        return Ok(Some(Stmt::ForEach {
                            var: names[0].clone(),
                            var2: Some(names[1].clone()),
                            var_ty: Ty::Any,
                            iter,
                            body,
                        }));
                    }
                    // 1 name over map -> keys
                    return Ok(Some(Stmt::ForEach {
                        var: names[0].clone(),
                        var2: None,
                        var_ty: Ty::Any,
                        iter: Expr::call(Expr::ident("__keys__"), vec![iter]),
                        body,
                    }));
                }
                return Err(self.unsupported("tuple assignment"));
            }
            if self.at_punct("=") {
                self.bump();
                let value = self.parse_simple()?;
                let init = vec![Stmt::Assign { target: e, op: None, value }];
                self.expect_punct(";")?;
                return self.parse_for_rest(Some(init));
            }
            // it was just the condition
            self.pos = save;
            let cond = self.parse_expr()?;
            let body = self.parse_block()?;
            return Ok(Some(Stmt::While { cond, body }));
        }
        // condition-only
        if self.at_punct(";") {
            return self.parse_for_rest(None);
        }
        let cond = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(Some(Stmt::While { cond, body }))
    }

    fn parse_for_rest(&mut self, init: Option<Vec<Stmt>>) -> PResult<Option<Stmt>> {
        let cond = if self.at_punct(";") {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect_punct(";")?;
        let mut step = Vec::new();
        if !self.at_punct("{") {
            let e = self.parse_simple()?;
            step.push(Stmt::Expr(e));
        }
        let body = self.parse_block()?;
        Ok(Some(Stmt::For {
            init: init.unwrap_or_default(),
            cond,
            step,
            body,
        }))
    }

    fn parse_ident_list(&mut self) -> PResult<Vec<String>> {
        let mut names = vec![self.expect_ident()?];
        while self.eat_punct(",") {
            names.push(self.expect_ident()?);
        }
        Ok(names)
    }

    /// `a := e` or `v, ok := m[k]` (two-value map lookup idiom)
    fn parse_short_var_decl(&mut self, first: Expr) -> PResult<Option<Stmt>> {
        self.bump(); // :=
        let mut names: Vec<String> = match &first {
            Expr::Ident(n) => vec![n.clone()],
            Expr::Array(items) => {
                let mut ns: Vec<String> = Vec::new();
                for it in items {
                    match it {
                        Expr::Ident(n) => ns.push(n.clone()),
                        _ => return Err(self.unsupported("complex := target")),
                    }
                }
                ns
            }
            _ => return Err(self.unsupported("complex := target")),
        };
        if names.len() >= 2 {
            let value = self.parse_expr()?;
            self.expect_semi()?;
            if let Expr::Call { .. } = value {
                // a, b := f() — multiple return values modelled as an array
                let mut stmts = Vec::new();
                stmts.push(Stmt::Local {
                    name: "$ret".into(),
                    ty: Ty::Any,
                    init: Some(value),
                    mutable: false,
                });
                for (i, n) in names.iter().enumerate() {
                    stmts.push(Stmt::Local {
                        name: n.clone(),
                        ty: Ty::Any,
                        init: Some(Expr::index(Expr::ident("$ret"), Expr::Int(i as i64))),
                        mutable: true,
                    });
                }
                return Ok(Some(Stmt::Block(stmts)));
            }
            if let Expr::Index { obj, idx } = value {
                let obj = *obj;
                let idx = *idx;
                let has = Expr::call(
                    Expr::ident("__map_has__"),
                    vec![obj.clone(), idx.clone()],
                );
                let get = Expr::index(obj, idx);
                return Ok(Some(Stmt::Block(vec![
                    Stmt::Local {
                        name: names[0].clone(),
                        ty: Ty::Any,
                        init: Some(get),
                        mutable: true,
                    },
                    Stmt::Local {
                        name: names[1].clone(),
                        ty: Ty::Any,
                        init: Some(has),
                        mutable: true,
                    },
                ])));
            }
            return Err(self.unsupported("multiple return values"));
        }
        if names.len() > 1 {
            return Err(self.unsupported("multiple return values"));
        }
        let value = self.parse_expr()?;
        self.expect_semi()?;
        Ok(Some(Stmt::Local {
            name: names.pop().unwrap(),
            ty: Ty::Any,
            init: Some(value),
            mutable: true,
        }))
    }

    /// simple statement (no semicolon): used in if/for headers
    fn parse_simple(&mut self) -> PResult<Expr> {
        self.parse_expr()
    }

    fn parse_switch(&mut self) -> PResult<Option<Stmt>> {
        self.bump(); // switch
        // optional init; then optional subject
        let mut init = Vec::new();
        if !self.at_punct("{") {
            let e = self.parse_simple()?;
            if self.at_punct(":=") {
                self.bump();
                let name = match &e {
                    Expr::Ident(n) => n.clone(),
                    _ => return Err(self.unsupported("complex := in switch")),
                };
                let value = self.parse_simple()?;
                init.push(Stmt::Local { name, ty: Ty::Any, init: Some(value), mutable: true });
                self.expect_punct(";")?;
            } else {
                init.push(Stmt::Expr(e));
                self.expect_punct(";")?;
            }
        }
        let subject = if self.at_punct("{") {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect_punct("{")?;
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
                        return Err(self.err("unexpected end of case"));
                    }
                    if let Some(s) = self.parse_stmt()? {
                        body.push(s);
                    }
                }
                if matches!(body.last(), Some(Stmt::Break)) {
                    body.pop();
                }
                // Go cases never fall through implicitly: no exit needed
                arms.push(MatchArm { pattern: pats, guard: None, body });
                continue;
            }
            if self.eat_kw("default") {
                self.expect_punct(":")?;
                let mut body = Vec::new();
                while !self.at_kw("case") && !self.at_kw("default") && !self.at_punct("}") {
                    if self.cur().kind == Tok::Eof {
                        return Err(self.err("unexpected end of default"));
                    }
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
        let subject = match subject {
            Some(s) => s,
            None => Expr::Bool(true),
        };
        let mut stmt = Stmt::Match { subject, arms, default };
        if !init.is_empty() {
            let mut b = init;
            b.push(stmt);
            stmt = Stmt::Block(b);
        }
        Ok(Some(stmt))
    }

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

    // ---- expressions -----------------------------------------------------------
    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_binary(0)
    }

    fn parse_binary(&mut self, level: usize) -> PResult<Expr> {
        const LEVELS: &[&[(&str, BinOp)]] = &[
            &[("||", BinOp::Or)],
            &[("&&", BinOp::And)],
            &[("==", BinOp::Eq), ("!=", BinOp::Ne), ("<", BinOp::Lt), ("<=", BinOp::Le), (">", BinOp::Gt), (">=", BinOp::Ge)],
            &[("+", BinOp::Add), ("-", BinOp::Sub), ("|", BinOp::BitOr), ("^", BinOp::BitXor)],
            &[("*", BinOp::Mul), ("/", BinOp::Div), ("%", BinOp::Mod), ("<<", BinOp::Shl), (">>", BinOp::Shr), ("&", BinOp::BitAnd), ("&^", BinOp::BitAnd)],
        ];
        if level >= LEVELS.len() {
            return self.parse_unary();
        }
        let mut l = self.parse_binary(level + 1)?;
        loop {
            let mut matched = None;
            for (p, op) in LEVELS[level] {
                if self.at_punct(p) {
                    matched = Some(*op);
                    break;
                }
            }
            let op = match matched {
                Some(o) => o,
                None => break,
            };
            self.bump();
            let r = self.parse_binary(level + 1)?;
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
        if self.at_punct("!") {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(Expr::Unary { op: UnOp::Not, e: Box::new(e) });
        }
        if self.at_punct("^") {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(Expr::Unary { op: UnOp::BitNot, e: Box::new(e) });
        }
        if self.at_punct("*") {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(Expr::method(e, "__deref__", vec![]));
        }
        if self.at_punct("&") {
            self.bump();
            // &T{...} -> composite address = just the composite
            let e = self.parse_unary()?;
            return Ok(Expr::method(e, "__ref__", vec![]));
        }
        if self.at_punct("<-") {
            return Err(self.unsupported("channel receive"));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            if self.at_punct(".") {
                self.bump();
                if self.at_punct("(") {
                    // type assertion x.(T)
                    let _ = self.parse_type_in_parens()?;
                    continue;
                }
                let name = if self.cur().kind == Tok::Ident || self.cur().kind == Tok::Keyword {
                    self.bump().text
                } else {
                    return Err(self.err("expected field name"));
                };
                e = Expr::Field { obj: Box::new(e), name, nullsafe: false };
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
                if self.at_punct(":") {
                    // slice [:hi]
                    let hi = if self.at_punct("]") {
                        None
                    } else {
                        Some(Box::new(self.parse_expr()?))
                    };
                    self.expect_punct("]")?;
                    e = Expr::Slice { obj: Box::new(e), lo: None, hi };
                    continue;
                }
                let idx = self.parse_expr()?;
                if self.eat_punct(":") {
                    let lo = Box::new(idx);
                    let hi = if self.at_punct("]") {
                        None
                    } else {
                        Some(Box::new(self.parse_expr()?))
                    };
                    self.expect_punct("]")?;
                    e = Expr::Slice { obj: Box::new(e), lo: Some(lo), hi };
                    continue;
                }
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
            break;
        }
        Ok(e)
    }

    fn parse_type_in_parens(&mut self) -> PResult<Ty> {
        self.expect_punct("(")?;
        let ty = self.parse_type()?;
        self.expect_punct(")")?;
        Ok(ty)
    }

    fn parse_args(&mut self) -> PResult<Vec<Expr>> {
        self.expect_punct("(")?;
        let mut args = Vec::new();
        while !self.at_punct(")") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of arguments"));
            }
            if self.eat_punct("...") {
                let e = self.parse_expr()?;
                args.push(Expr::Spread(Box::new(e)));
            } else {
                args.push(self.parse_expr()?);
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
            Tok::RawStr => {
                self.bump();
                Ok(Expr::Str(t.text))
            }
            Tok::Char => {
                self.bump();
                Ok(Expr::Str(t.text))
            }
            Tok::Ident => {
                self.bump();
                // generic instantiation f[T](...)
                if self.at_punct("[") && self.looks_like_type_args() {
                    self.skip_brackets()?;
                }
                // struct composite literal: Type{...} when Type is known or in
                // expression context (RHS, args, return) where blocks cannot appear
                if self.at_punct("{")
                    && (self.type_names.contains(&t.text) || self.allow_composite)
                {
                    return self.parse_composite(Ty::Class(t.text.clone()), false);
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
                "nil" => {
                    self.bump();
                    Ok(Expr::Null)
                }
                "func" => {
                    self.bump();
                    let (params, _) = self.parse_params()?;
                    let ret = self.parse_results()?;
                    let body = self.parse_block()?;
                    let _ = ret;
                    Ok(Expr::Closure { params, body, value: None })
                }
                "map" => {
                    // map[K]V{...}
                    let ty = self.parse_type()?;
                    if self.at_punct("{") {
                        return self.parse_composite(ty, true);
                    }
                    Ok(Expr::ident("__map_new__"))
                }
                "struct" => {
                    return Err(self.unsupported("anonymous struct literal"));
                }
                "chan" => return Err(self.unsupported("channels")),
                "interface" => {
                    // nil interface value
                    let ty = self.parse_type()?;
                    let _ = ty;
                    Ok(Expr::Null)
                }
                "range" => Err(self.err("range outside for")),
                _ => Err(self.err(format!("unexpected keyword '{}'", t.text))),
            },
            Tok::Punct => {
                if t.text == "(" {
                    self.bump();
                    let e = self.parse_expr()?;
                    self.expect_punct(")")?;
                    return Ok(Expr::Paren(Box::new(e)));
                }
                if t.text == "[" {
                    // slice/array literal: []T{...} or []T{...}[i]? just literal
                    let ty = self.parse_type()?;
                    if self.at_punct("{") {
                        return self.parse_composite(ty, false);
                    }
                    return Err(self.unsupported("array type expression"));
                }
                Err(self.err(format!("unexpected token '{}'", t.text)))
            }
            _ => Err(self.err(format!("unexpected token '{}'", t.text))),
        }
    }

    /// Distinguish `f[T]` (type args) from `f[expr]` (index): peek if content
    /// looks like a type list.
    fn looks_like_type_args(&self) -> bool {
        // heuristic: next token is Ident/keyword type or * and then ] or ,
        let n = self.nxt(1);
        if n.kind == Tok::Ident {
            // ident followed by ] or , or any/interface
            let n2 = self.nxt(2);
            if n2.is_punct("]") || n2.is_punct(",") {
                return true;
            }
        }
        matches!(n.text.as_str(), "any" | "int" | "string" | "bool" | "float64" | "byte")
    }

    /// composite literal after a type: T{...} / []T{...} / map[K]V{...}
    fn parse_composite(&mut self, ty: Ty, is_map: bool) -> PResult<Expr> {
        self.expect_punct("{")?;
        if is_map {
            let mut pairs = Vec::new();
            while !self.at_punct("}") {
                if self.cur().kind == Tok::Eof {
                    return Err(self.err("unexpected end of map literal"));
                }
                let k = self.parse_expr()?;
                self.expect_punct(":")?;
                let v = self.parse_expr()?;
                pairs.push((k, v));
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.expect_punct("}")?;
            return Ok(Expr::Dict(pairs));
        }
        // struct composite literal (keyed or positional)
        let class = match &ty {
            Ty::Class(n) => Some(n.clone()),
            _ => None,
        };
        if self.at_punct("}") {
            self.bump();
            if let Some(c) = class {
                return Ok(Expr::call(Expr::ident(&format!("__structinit_{}", c)), vec![]));
            }
            return Ok(Expr::Array(vec![]));
        }
        // peek: ident ':' -> keyed
        if self.cur().kind == Tok::Ident && self.nxt(1).is_punct(":") {
            let mut fields = Vec::new();
            while !self.at_punct("}") {
                if self.cur().kind == Tok::Eof {
                    return Err(self.err("unexpected end of composite literal"));
                }
                let fname = self.expect_ident()?;
                self.expect_punct(":")?;
                let v = self.parse_expr()?;
                fields.push(Expr::Named { name: fname, e: Box::new(v) });
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.expect_punct("}")?;
            let c = class.ok_or_else(|| self.unsupported("keyed literal of unknown type"))?;
            return Ok(Expr::call(
                Expr::ident(&format!("__structinit_{}", c)),
                fields,
            ));
        }
        let mut positional = Vec::new();
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of composite literal"));
            }
            positional.push(self.parse_expr()?);
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct("}")?;
        if let Some(c) = class {
            return Ok(Expr::call(
                Expr::ident(&format!("__structinit_{}", c)),
                positional,
            ));
        }
        Ok(Expr::Array(positional))
    }
}

fn map_go_type(name: &str) -> Ty {
    match name {
        "int" | "int8" | "int16" | "int32" | "int64" | "uint" | "uint8" | "uint16" | "uint32"
        | "uint64" | "uintptr" | "byte" | "rune" => Ty::Long,
        "float32" | "float64" => Ty::Double,
        "bool" => Ty::Bool,
        "string" => Ty::Str,
        "error" => Ty::Any,
        "any" => Ty::Any,
        _ => Ty::class(name),
    }
}

enum TopItem {
    Decl(Decl),
    Stmt(Stmt),
    Method(String, FunctionDecl),
}

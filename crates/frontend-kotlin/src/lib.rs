#![forbid(unsafe_code)]
//! Kotlin front-end: hand-written lexer + recursive-descent parser lowering to
//! the unified IR. Covers a practical subset: top-level functions with default
//! arguments, classes/data classes/objects with primary constructors, when
//! expressions, if-expressions, ranges, string templates, null-safety
//! operators, lambdas with implicit `it`, for-in loops.

use transpiler_core::diag::{Diag, PResult};
use transpiler_core::ir::*;
use transpiler_core::lexer::*;

const KOTLIN_KEYWORDS: &[&str] = &[
    "as", "break", "by", "class", "continue", "do", "else", "false", "for", "fun", "if",
    "import", "in", "interface", "is", "null", "object", "package", "return", "super", "this",
    "throw", "true", "try", "typealias", "typeof", "val", "var", "when", "while",
];

const KOTLIN_PUNCTS: &[&str] = &[
    "..<", "...", "->", "?.", "?:", "::", "!!", "++", "--", "+=", "-=", "*=", "/=", "%=",
    "..", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "(", ")", "[", "]", "{", "}", ",",
    ":", ".", ";", "=", "+", "-", "*", "/", "%", "<", ">", "!", "?", "&", "|", "^", "@",
];

pub fn kotlin_lexer_config() -> LexerConfig {
    LexerConfig {
        keywords: KOTLIN_KEYWORDS.to_vec(),
        puncts: KOTLIN_PUNCTS.to_vec(),
        backtick: Backtick::None,
        dollar_strings: true,
        char_literals: false,
    }
}

pub struct KotlinParser {
    toks: Vec<Token>,
    pos: usize,
    file: String,
    imports: Vec<String>,
    hoisted: Vec<Decl>,
}

/// Parse a Kotlin source file into the unified IR.
pub fn parse_kotlin(src: &str, file: &str) -> PResult<Module> {
    let cfg = kotlin_lexer_config();
    let toks = Lexer::new(src, &cfg).tokenize()?;
    let mut p = KotlinParser {
        toks,
        pos: 0,
        file: file.to_string(),
        imports: vec![],
        hoisted: vec![],
    };
    p.parse_file()
}

impl KotlinParser {
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
        self.err(format!("unsupported Kotlin syntax: {}", what))
    }
    fn expect_semi(&mut self) -> PResult<()> {
        if self.eat_punct(";") {
            return Ok(());
        }
        if self.cur().kind == Tok::Eof || self.at_punct("}") {
            return Ok(());
        }
        Ok(()) // newline-separated: accept anything
    }

    // ---- file ---------------------------------------------------------
    fn parse_file(&mut self) -> PResult<Module> {
        let mut name = String::new();
        if self.eat_kw("package") {
            let mut parts = vec![self.expect_ident()?];
            while self.eat_punct(".") {
                parts.push(self.expect_ident()?);
            }
            name = parts.join(".");
        }
        while self.eat_kw("import") {
            let mut path = String::new();
            loop {
                if self.cur().kind != Tok::Ident {
                    break;
                }
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(&self.bump().text);
                if self.at_punct(".") {
                    self.bump();
                } else {
                    break;
                }
            }
            if self.eat_punct(".*") || self.eat_kw("as") {
                if self.cur().kind == Tok::Ident {
                    self.bump();
                }
            }
            if !path.is_empty() {
                self.imports.push(path);
            }
        }
        let mut decls: Vec<Decl> = Vec::new();
        let mut main_body: Block = Vec::new();
        while self.cur().kind != Tok::Eof {
            self.skip_annotations()?;
            if self.at_kw("fun") {
                // extension function?
                let save = self.pos;
                self.bump();
                let mut fname = self.expect_ident()?;
                // type params
                if self.at_punct("<") {
                    self.skip_generics()?;
                }
                if self.at_punct(".") {
                    // extension: skip receiver, keep name
                    self.bump();
                    fname = self.expect_ident()?;
                    if self.at_punct("<") {
                        self.skip_generics()?;
                    }
                }
                if self.at_punct("(") {
                    let f = self.parse_fun_rest(Some(fname.clone()))?;
                    decls.push(Decl::Function(f));
                    let _ = save;
                    continue;
                }
                return Err(self.err("malformed function declaration"));
            }
            if self.at_kw("class") || self.at_kw("interface") || self.at_kw("object") {
                let c = self.parse_class()?;
                decls.push(Decl::Class(c));
                continue;
            }
            if self.cur().kind == Tok::Ident
                && self.cur().text == "data"
                && self.nxt(1).is_kw("class")
            {
                self.bump();
                let mut c = self.parse_class()?;
                c.is_data = true;
                decls.push(Decl::Class(c));
                continue;
            }
            if self.at_kw("typealias") {
                self.bump();
                let name = self.expect_ident()?;
                if self.at_punct("<") {
                    self.skip_generics()?;
                }
                self.expect_punct("=")?;
                let ty = self.parse_type()?;
                decls.push(Decl::TypeAlias { name, ty });
                continue;
            }
            if self.at_kw("val") || self.at_kw("var") {
                let mutable = self.at_kw("var");
                self.bump();
                let stmts = self.parse_property_decl(mutable, false)?;
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
                        other => main_body.push(other),
                    }
                }
                continue;
            }
            if let Some(s) = self.parse_stmt()? {
                main_body.push(s);
            }
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
        Ok(Module {
            lang: Lang::Kotlin,
            name,
            imports: std::mem::take(&mut self.imports),
            decls,
        })
    }

    fn skip_annotations(&mut self) -> PResult<()> {
        while self.at_punct("@") {
            self.bump();
            let _ = self.expect_ident()?;
            if self.at_punct("(") {
                self.skip_balanced("(", ")")?;
            }
        }
        Ok(())
    }

    fn skip_balanced(&mut self, open: &str, close: &str) -> PResult<()> {
        self.expect_punct(open)?;
        let mut depth = 1;
        while depth > 0 {
            let t = self.bump();
            if t.kind == Tok::Eof {
                return Err(self.err("unexpected end of block"));
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
                return Err(self.err("unexpected end of generics"));
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

    fn parse_class(&mut self) -> PResult<ClassDecl> {
        let kind_kw = self.bump().text; // class | interface | object
        let name = self.expect_ident()?;
        if self.at_punct("<") {
            self.skip_generics()?;
        }
        let mut cls = ClassDecl {
            name,
            extends: None,
            fields: Vec::new(),
            methods: Vec::new(),
            ctor: None,
            kind: match kind_kw.as_str() {
                "interface" => ClassKind::Interface,
                "object" => ClassKind::Object,
                _ => ClassKind::Class,
            },
            is_data: false,
        };
        // primary constructor
        if self.at_punct("(") {
            let mut ctor_params = Vec::new();
            let mut prologue = Vec::new();
            self.bump();
            while !self.at_punct(")") {
                if self.cur().kind == Tok::Eof {
                    return Err(self.err("unexpected end of primary constructor"));
                }
                // modifiers val/var
                let mut is_prop = false;
                loop {
                    if self.at_kw("val") || self.at_kw("var") {
                        is_prop = true;
                        self.bump();
                    } else if self.cur().kind == Tok::Ident
                        && (self.nxt(1).is_kw("val") || self.nxt(1).is_kw("var"))
                    {
                        self.bump(); // annotation-like name
                    } else if self.at_punct("@") {
                        self.skip_annotations()?;
                    } else {
                        break;
                    }
                }
                let pname = self.expect_ident()?;
                self.eat_punct(":");
                let ty = if self.cur().kind == Tok::Ident || self.cur().kind == Tok::Keyword {
                    self.parse_type()?
                } else {
                    Ty::Any
                };
                let default = if self.eat_punct("=") { Some(self.parse_expr()?) } else { None };
                ctor_params.push(Param::new(&pname, ty.clone()));
                if is_prop {
                    cls.fields.push(FieldDecl {
                        name: pname.clone(),
                        ty,
                        init: Some(Expr::ident(&pname)),
                        is_static: false,
                        mutable: true,
                        visibility: Visibility::Public,
                    });
                }
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.expect_punct(")")?;
            // ctor body = assignments of property params
            let mut body = std::mem::take(&mut prologue);
            for f in &cls.fields {
                body.push(Stmt::Expr(Expr::AssignExpr {
                    target: Box::new(Expr::field(Expr::This, &f.name)),
                    op: None,
                    value: Box::new(Expr::ident(&f.name)),
                }));
            }
            let mut f = FunctionDecl::new("constructor", ctor_params, Ty::Void, body);
            f.is_ctor = true;
            cls.ctor = Some(f);
        }
        if self.eat_punct(":") {
            // superclass and/or interfaces
            loop {
                let base = self.parse_type_name()?;
                if cls.extends.is_none() && !base.chars().next().map_or(false, |c| c.is_lowercase()) {
                    cls.extends = Some(base);
                }
                if self.at_punct("(") {
                    // super constructor args: skip
                    self.skip_balanced("(", ")")?;
                }
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        self.expect_punct("{")?;
        let is_enum = false;
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of class body"));
            }
            if self.eat_punct(";") {
                continue;
            }
            self.skip_annotations()?;
            // enum class entries: NAME or NAME(args)
            if is_enum {}
            if (self.cur().kind == Tok::Ident || self.cur().kind == Tok::Keyword)
                && (self.nxt(1).is_punct(",")
                    || self.nxt(1).is_punct(";")
                    || self.nxt(1).is_punct("}")
                    || self.nxt(1).is_punct("("))
                && !self.at_kw("companion")
                && !self.at_kw("fun")
                && !self.at_kw("val")
                && !self.at_kw("var")
                && !self.at_kw("class")
                && !self.at_kw("object")
            {
                let ename = self.bump().text;
                let init = if self.at_punct("(") {
                    let _ = self.skip_balanced("(", ")")?;
                    Expr::Str(ename.clone())
                } else {
                    Expr::Str(ename.clone())
                };
                cls.fields.push(FieldDecl {
                    name: ename,
                    ty: Ty::Any,
                    init: Some(init),
                    is_static: false,
                    mutable: false,
                    visibility: Visibility::Public,
                });
                if !self.eat_punct(",") {
                    self.eat_punct(";");
                }
                continue;
            }
            if self.at_kw("companion") {
                self.bump();
                if self.at_kw("object") {
                    self.bump();
                }
                if self.eat_punct(":") {
                    let _ = self.parse_type_name()?;
                }
                // parse companion members as statics
                self.expect_punct("{")?;
                while !self.at_punct("}") {
                    if self.cur().kind == Tok::Eof {
                        return Err(self.err("unexpected end of companion object"));
                    }
                    if self.eat_punct(";") {
                        continue;
                    }
                    self.parse_class_member(&mut cls, true)?;
                }
                self.expect_punct("}")?;
                continue;
            }
            if self.at_kw("init") && self.nxt(1).is_punct("{") {
                self.bump();
                let block = self.parse_block()?;
                if let Some(ctor) = &mut cls.ctor {
                    ctor.body.push(Stmt::Block(block));
                }
                continue;
            }
            if self.at_kw("constructor") {
                self.bump();
                if self.at_punct("(") {
                    // secondary constructor: parse params, ignore
                    self.skip_balanced("(", ")")?;
                    if self.eat_punct(":") {
                        // this(...) / super(...)
                        let _ = self.expect_ident()?;
                        self.skip_balanced("(", ")")?;
                    }
                    let _ = self.parse_block()?;
                }
                continue;
            }
            self.parse_class_member(&mut cls, false)?;
        }
        self.expect_punct("}")?;
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

    fn parse_class_member(&mut self, cls: &mut ClassDecl, in_companion: bool) -> PResult<()> {
        self.skip_annotations()?;
        // modifiers
        let mut is_static = in_companion;
        loop {
            if self.at_kw("public") || self.at_kw("private") || self.at_kw("protected")
                || self.at_kw("internal") || self.at_kw("final") || self.at_kw("open")
                || self.at_kw("override") || self.at_kw("abstract") || self.at_kw("lateinit"
            ) {
                self.bump();
            } else if self.at_kw("const") {
                self.bump();
                is_static = true;
            } else if self.cur().kind == Tok::Ident
                && (self.nxt(1).is_kw("val") || self.nxt(1).is_kw("var") || self.nxt(1).is_kw("fun"))
            {
                // annotation name before member
                self.bump();
            } else {
                break;
            }
        }
        if self.at_kw("fun") {
            self.bump();
            let mut fname = self.expect_ident()?;
            if self.at_punct("<") {
                self.skip_generics()?;
            }
            if self.at_punct(".") {
                self.bump();
                fname = self.expect_ident()?;
                if self.at_punct("<") {
                    self.skip_generics()?;
                }
            }
            let mut f = self.parse_fun_rest(Some(fname))?;
            f.is_static = is_static;
            cls.methods.push(f);
            return Ok(());
        }
        if self.at_kw("val") || self.at_kw("var") {
            let mutable = self.at_kw("var");
            self.bump();
            let name = self.expect_ident()?;
            let ty = if self.eat_punct(":") {
                self.parse_type()?
            } else {
                Ty::Any
            };
            let init = if self.eat_punct("=") { Some(self.parse_expr()?) } else { None };
            // accessors: get() = ... / set (get/set are identifiers in Kotlin)
            let is_accessor = |p: &Self| {
                (p.cur().kind == Tok::Ident || p.cur().kind == Tok::Keyword)
                    && (p.cur().text == "get" || p.cur().text == "set")
            };
            if is_accessor(self) {
                // property with accessors -> represent as methods
                while is_accessor(self) {
                    let which = self.bump().text;
                    let _ = which;
                    if self.at_punct("(") {
                        let _ = self.skip_balanced("(", ")")?;
                    }
                    if self.eat_punct("=") {
                        let body = vec![Stmt::Return(Some(self.parse_expr()?))];
                        let mname = format!("get_{}", name);
                        cls.methods.push(FunctionDecl::new(&mname, vec![], ty.clone(), body));
                    } else if self.at_punct("{") {
                        let body = self.parse_block()?;
                        let mname = format!("get_{}", name);
                        cls.methods.push(FunctionDecl::new(&mname, vec![], ty.clone(), body));
                    }
                    self.expect_semi()?;
                }
                return Ok(());
            }
            cls.fields.push(FieldDecl {
                name,
                ty,
                init,
                is_static,
                mutable,
                visibility: Visibility::Default,
            });
            return Ok(());
        }
        Err(self.unsupported("class member"))
    }

    /// val/var name [: Type] [= expr]
    fn parse_property_decl(&mut self, mutable: bool, in_class: bool) -> PResult<Vec<Stmt>> {
        let name = self.expect_ident()?;
        let ty = if self.eat_punct(":") {
            self.parse_type()?
        } else {
            Ty::Any
        };
        if self.at_punct(".") {
            return Err(self.unsupported("extension property"));
        }
        let init = if self.eat_punct("=") { Some(self.parse_expr()?) } else { None };
        // custom getter/setter (only possible without an initializer)
        if init.is_none()
            && (self.at_kw("get") || self.at_kw("set"))
        {
            return Err(self.unsupported("property accessors"));
        }
        if in_class {
            return Ok(vec![Stmt::Local { name, ty, init, mutable }]);
        }
        self.expect_semi()?;
        Ok(vec![Stmt::Local { name, ty, init, mutable }])
    }

    /// after `fun name`: (params) [: Type] body
    fn parse_fun_rest(&mut self, name: Option<String>) -> PResult<FunctionDecl> {
        let mut params = Vec::new();
        self.expect_punct("(")?;
        while !self.at_punct(")") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of parameters"));
            }
            self.skip_annotations()?;
            let pname = self.expect_ident()?;
            self.eat_punct(":");
            let ty = if self.cur().kind == Tok::Ident
                || self.cur().kind == Tok::Keyword
                || self.at_punct("(")
            {
                self.parse_type()?
            } else {
                Ty::Any
            };
            let default = if self.eat_punct("=") { Some(self.parse_expr()?) } else { None };
            params.push(Param { name: pname, ty, default, variadic: false });
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct(")")?;
        let ret = if self.eat_punct(":") {
            let mut ty = self.parse_type()?;
            if self.eat_punct("?") {
                ty = Ty::Option(Box::new(ty));
            }
            ty
        } else {
            Ty::Void
        };
        // body: block or = expr
        if self.eat_punct("=") {
            let value = self.parse_expr()?;
            let fname = name.unwrap_or_else(|| "anonymous".into());
            return Ok(FunctionDecl::new(
                &fname,
                params,
                ret,
                vec![Stmt::Return(Some(value))],
            ));
        }
        let body = self.parse_block()?;
        let fname = name.unwrap_or_else(|| "anonymous".into());
        Ok(FunctionDecl::new(&fname, params, ret, body))
    }

    // ---- types -----------------------------------------------------------
    fn parse_type(&mut self) -> PResult<Ty> {
        let ty = if self.at_punct("(") {
            // function type
            self.skip_balanced("(", ")")?;
            if self.eat_punct("->") {
                let _ = self.parse_type()?;
            }
            Ty::Any
        } else if self.cur().kind == Tok::Keyword {
            let t = self.bump().text;
            match t.as_str() {
                "val" | "var" => Ty::Any,
                _ => Ty::Any,
            }
        } else {
            let name = self.parse_type_name()?;
            if self.at_punct("<") {
                self.skip_generics()?;
                match name.as_str() {
                    "List" | "MutableList" | "Array" | "Set" | "Collection" | "Iterable"
                    | "Sequence" => Ty::Array(Box::new(Ty::Any)),
                    "Map" | "MutableMap" => Ty::Map(Box::new(Ty::Any), Box::new(Ty::Any)),
                    _ => Ty::Any,
                }
            } else {
                match name.as_str() {
                    "Int" | "Long" | "Short" | "Byte" => Ty::Int,
                    "Double" | "Float" => Ty::Double,
                    "Boolean" => Ty::Bool,
                    "String" | "CharSequence" => Ty::Str,
                    "Char" => Ty::Char,
                    "Unit" => Ty::Void,
                    "Any" => Ty::Any,
                    "List" | "Array" => Ty::Array(Box::new(Ty::Any)),
                    "Map" => Ty::Map(Box::new(Ty::Any), Box::new(Ty::Any)),
                    _ => Ty::class(&name),
                }
            }
        };
        let mut ty = ty;
        // nullable
        while self.eat_punct("?") {
            ty = Ty::Option(Box::new(ty));
        }
        // vararg
        if self.at_kw("vararg") {
            self.bump();
        }
        Ok(ty)
    }

    // ---- statements ----------------------------------------------------------
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
        // `enum class` in statement position (enum is an identifier in Kotlin)
        if t.kind == Tok::Ident && t.text == "enum" && self.nxt(1).is_kw("class") {
            self.bump();
            let c = self.parse_class()?;
            self.hoisted.push(Decl::Class(c));
            return Ok(Some(Stmt::Pass));
        }
        if t.kind == Tok::Keyword {
            match t.text.as_str() {
                "val" | "var" => {
                    let mutable = t.text == "var";
                    self.bump();
                    let stmts = self.parse_property_decl(mutable, false)?;
                    if stmts.len() == 1 {
                        return Ok(Some(stmts.into_iter().next().unwrap()));
                    }
                    return Ok(Some(Stmt::Block(stmts)));
                }
                "fun" => {
                    // local function
                    self.bump();
                    let fname = self.expect_ident()?;
                    if self.at_punct("<") {
                        self.skip_generics()?;
                    }
                    let f = self.parse_fun_rest(Some(fname.clone()))?;
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
                "class" | "object" => {
                    let c = self.parse_class()?;
                    self.hoisted.push(Decl::Class(c));
                    return Ok(Some(Stmt::Pass));
                }
                _ if t.kind == Tok::Ident && t.text == "enum" && self.nxt(1).is_kw("class") => {
                    // enum class in statement position
                    self.bump();
                    let c = self.parse_class()?;
                    self.hoisted.push(Decl::Class(c));
                    return Ok(Some(Stmt::Pass));
                }
                "if" => {
                    // if as expression or statement: parse uniformly; if used
                    // as expression the parent handles conversion.
                    let e = self.parse_if_expr()?;
                    return Ok(Some(Stmt::Expr(e)));
                }
                "when" => {
                    let (s, _) = self.parse_when()?;
                    return Ok(Some(s));
                }
                "while" => {
                    self.bump();
                    let cond = self.parse_expr()?;
                    let body = self.parse_block()?;
                    return Ok(Some(Stmt::While { cond, body }));
                }
                "do" => {
                    self.bump();
                    let body = self.parse_block()?;
                    if !self.eat_kw("while") {
                        return Err(self.err("expected 'while'"));
                    }
                    let cond = self.parse_expr()?;
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
                "for" => {
                    self.bump();
                    self.expect_punct("(")?;
                    let var = self.expect_ident()?;
                    if !self.eat_kw("in") {
                        return Err(self.err("expected 'in' in for loop"));
                    }
                    let iter = self.parse_expr()?;
                    self.expect_punct(")")?;
                    let body = self.parse_block()?;
                    return Ok(Some(Stmt::ForEach {
                        var,
                        var2: None,
                        var_ty: Ty::Any,
                        iter,
                        body,
                    }));
                }
                "return" => {
                    self.bump();
                    let v = if self.at_punct("}") || self.cur().kind == Tok::Eof {
                        None
                    } else if self.cur().kind == Tok::Keyword
                        && !matches!(self.cur().text.as_str(), "this" | "super" | "if" | "when" | "try" | "throw")
                    {
                        None
                    } else {
                        Some(self.parse_expr()?)
                    };
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Return(v)));
                }
                "break" => {
                    self.bump();
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Break));
                }
                "continue" => {
                    self.bump();
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Continue));
                }
                "throw" => {
                    self.bump();
                    let e = self.parse_expr()?;
                    self.expect_semi()?;
                    return Ok(Some(Stmt::Throw(e)));
                }
                "try" => {
                    let body = self.parse_block()?;
                    let mut catches = Vec::new();
                    while self.at_kw("catch") {
                        self.bump();
                        self.expect_punct("(")?;
                        let mut var = None;
                        if self.cur().kind == Tok::Ident {
                            var = Some(self.bump().text);
                            if self.eat_punct(":") {
                                let _ = self.parse_type()?;
                            }
                        }
                        self.expect_punct(")")?;
                        let cbody = self.parse_block()?;
                        catches.push(CatchClause { var, ty: None, body: cbody });
                    }
                    let mut finally = None;
                    if self.eat_kw("finally") {
                        finally = Some(self.parse_block()?);
                    }
                    return Ok(Some(Stmt::Try { body, catches, finally }));
                }
                "object" => {
                    // object expression
                    return Err(self.unsupported("object expressions"));
                }
                _ => {}
            }
        }
        // expression / assignment
        let e = self.parse_expr()?;
        const AUG: &[(&str, BinOp)] = &[
            ("+=", BinOp::Add),
            ("-=", BinOp::Sub),
            ("*=", BinOp::Mul),
            ("/=", BinOp::Div),
            ("%=", BinOp::Mod),
        ];
        for (p, op) in AUG {
            if self.at_punct(p) {
                self.bump();
                let value = self.parse_expr()?;
                self.expect_semi()?;
                return Ok(Some(Stmt::Assign { target: e, op: Some(*op), value }));
            }
        }
        if self.at_punct("=") {
            self.bump();
            let value = self.parse_expr()?;
            self.expect_semi()?;
            return Ok(Some(Stmt::Assign { target: e, op: None, value }));
        }
        self.expect_semi()?;
        Ok(Some(Stmt::Expr(e)))
    }

    /// if [cond] a else b — as an expression
    fn parse_if_expr(&mut self) -> PResult<Expr> {
        self.bump(); // if
        self.expect_punct("(")?;
        let cond = self.parse_expr()?;
        self.expect_punct(")")?;
        let then = if self.at_punct("{") {
            let b = self.parse_block()?;
            block_to_value(b)
        } else {
            self.parse_expr()?
        };
        let els = if self.eat_kw("else") {
            if self.at_kw("if") {
                self.parse_if_expr()?
            } else if self.at_punct("{") {
                let b = self.parse_block()?;
                block_to_value(b)
            } else {
                self.parse_expr()?
            }
        } else {
            Expr::Unit
        };
        Ok(Expr::Ternary {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        })
    }

    /// when [(subject)] { patterns -> body } — returns statement form + optional value
    fn parse_when(&mut self) -> PResult<(Stmt, Option<Expr>)> {
        self.bump(); // when
        let subject = if self.at_punct("(") {
            self.bump();
            let e = self.parse_expr()?;
            self.expect_punct(")")?;
            Some(e)
        } else {
            None
        };
        self.expect_punct("{")?;
        let mut arms: Vec<MatchArm> = Vec::new();
        let mut default: Option<Block> = None;
        while !self.at_punct("}") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of when"));
            }
            if self.eat_punct("else") {
                self.expect_punct("->")?;
                let body = self.parse_when_body()?;
                default = Some(body);
                continue;
            }
            // pattern: expr | in range | is Type | expr, expr
            let mut pats = Vec::new();
            let mut guard = None;
            if self.at_kw("in") {
                self.bump();
                let range = self.parse_expr()?;
                pats.push(range);
            } else if self.at_kw("is") {
                self.bump();
                let ty = self.parse_type()?;
                let _ = ty;
                pats.push(Expr::Bool(true));
            } else {
                pats.push(self.parse_expr()?);
                while self.eat_punct(",") {
                    pats.push(self.parse_expr()?);
                }
                if self.eat_kw("in") {
                    // x in range guard: subject in range
                    let range = self.parse_expr()?;
                    pats.push(range);
                }
            }
            self.expect_punct("->")?;
            let body = self.parse_when_body()?;
            arms.push(MatchArm { pattern: pats, guard, body });
        }
        self.expect_punct("}")?;
        match subject {
            Some(subject) => Ok((Stmt::Match { subject, arms, default }, None)),
            None => {
                // subject-less when: conditions
                // convert each arm: pattern -> cond
                let arms_cond: Vec<(Expr, Block)> = arms
                    .into_iter()
                    .map(|a| (a.pattern.into_iter().next().unwrap_or(Expr::Bool(true)), a.body))
                    .collect();
                let mut result: Option<Expr> = default.and_then(|d| block_to_option_value(d));
                for (cond, body) in arms_cond.into_iter().rev() {
                    let value = block_to_option_value(body);
                    let els = result.unwrap_or(Expr::Unit);
                    let then = value.unwrap_or(Expr::Unit);
                    result = Some(Expr::Ternary {
                        cond: Box::new(cond),
                        then: Box::new(then),
                        els: Box::new(els),
                    });
                }
                match result {
                    Some(e) => Ok((Stmt::Expr(e.clone()), Some(e))),
                    None => Ok((Stmt::Pass, None)),
                }
            }
        }
    }

    fn parse_when_body(&mut self) -> PResult<Block> {
        if self.at_punct("{") {
            self.parse_block()
        } else {
            let e = self.parse_expr()?;
            self.expect_semi()?;
            Ok(vec![Stmt::Return(Some(e))])
        }
    }

    // ---- expressions ------------------------------------------------------------
    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_elvis()
    }

    fn parse_elvis(&mut self) -> PResult<Expr> {
        let l = self.parse_binary(0)?;
        if self.at_punct("?:") {
            self.bump();
            let r = self.parse_elvis()?;
            return Ok(Expr::Ternary {
                cond: Box::new(Expr::binary(BinOp::Eq, l.clone(), Expr::Null)),
                then: Box::new(r),
                els: Box::new(l),
            });
        }
        Ok(l)
    }

    fn parse_binary(&mut self, level: usize) -> PResult<Expr> {
        const LEVELS: &[&[(&str, BinOp)]] = &[
            &[("||", BinOp::Or)],
            &[("&&", BinOp::And)],
            &[("==", BinOp::Eq), ("!=", BinOp::Ne)],
            &[("<", BinOp::Lt), ("<=", BinOp::Le), (">", BinOp::Gt), (">=", BinOp::Ge)],
            &[
                ("..", BinOp::Add),   // range marker: replaced below
                ("..<", BinOp::Add),
                ("in", BinOp::In),
            ],
            &[("+", BinOp::Add), ("-", BinOp::Sub)],
            &[("*", BinOp::Mul), ("/", BinOp::Div), ("%", BinOp::Mod)],
        ];
        if level >= LEVELS.len() {
            return self.parse_unary();
        }
        let mut l = self.parse_binary(level + 1)?;
        loop {
            let mut matched = None;
            for (p, op) in LEVELS[level] {
                if self.at_punct(p) || (self.cur().is_kw(p) && (level == 5)) {
                    matched = Some((*op, self.cur().text.clone()));
                    break;
                }
            }
            let (op, text) = match matched {
                Some(m) => m,
                None => break,
            };
            self.bump();
            let r = self.parse_binary(level + 1)?;
            if text == ".." || text == "..<" {
                l = Expr::Range {
                    lo: Box::new(l),
                    hi: Box::new(r),
                    inclusive: text == "..",
                };
            } else if text == "in" {
                l = Expr::binary(BinOp::In, l, r);
            } else if self.cur().is_kw("in") {
                // x in y handled above
                l = Expr::binary(op, l, r);
            } else {
                l = Expr::binary(op, l, r);
            }
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
        if self.at_punct("++") || self.at_punct("--") {
            let is_inc = self.at_punct("++");
            self.bump();
            let e = self.parse_unary()?;
            return Ok(Expr::Unary {
                op: if is_inc { UnOp::PreInc } else { UnOp::PreDec },
                e: Box::new(e),
            });
        }
        // label@? not supported
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            if self.at_punct(".") {
                self.bump();
                if self.at_kw("class") {
                    self.bump();
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
            if self.at_punct("?.") {
                self.bump();
                let name = if self.cur().kind == Tok::Ident || self.cur().kind == Tok::Keyword {
                    self.bump().text
                } else {
                    return Err(self.err("expected member name"));
                };
                e = Expr::Field { obj: Box::new(e), name, nullsafe: true };
                continue;
            }
            if self.at_punct("!!") {
                self.bump();
                // non-null assertion: runtime check
                e = Expr::call(Expr::ident("__notnull__"), vec![e]);
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
                return Err(self.unsupported("callable references"));
            }
            break;
        }
        Ok(e)
    }

    fn parse_args(&mut self) -> PResult<Vec<Expr>> {
        self.expect_punct("(")?;
        let mut args = Vec::new();
        while !self.at_punct(")") {
            if self.cur().kind == Tok::Eof {
                return Err(self.err("unexpected end of arguments"));
            }
            // trailing lambda arg handled at call site (unsupported)
            if self.cur().kind == Tok::Ident && self.nxt(1).is_punct("=") && !self.nxt(2).is_punct("=") {
                let name = self.bump().text;
                self.bump();
                let v = self.parse_expr()?;
                args.push(Expr::Named { name, e: Box::new(v) });
            } else {
                args.push(self.parse_expr()?);
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct(")")?;
        // trailing lambda
        if self.at_punct("{") {
            let lam = self.parse_lambda()?;
            args.push(lam);
        }
        Ok(args)
    }

    fn parse_lambda(&mut self) -> PResult<Expr> {
        self.expect_punct("{")?;
        // params: a, b -> or implicit it
        let mut params = Vec::new();
        let mut has_arrow = false;
        // detect "x ->" or "a, b ->" at start
        let save = self.pos;
        if self.cur().kind == Tok::Ident {
            let mut names = vec![self.cur().text.clone()];
            let mut i = 1;
            while self.nxt(i).kind == Tok::Ident && self.nxt(i + 1).is_punct(",") {
                names.push(self.nxt(i).text.clone());
                i += 2;
            }
            if self.nxt(i).is_punct("->") {
                // arrow params
                for _ in 0..names.len() + (names.len() - 1) {
                    self.bump();
                }
                self.bump(); // ->
                for n in &names {
                    params.push(Param::new(n, Ty::Any));
                }
                has_arrow = true;
            }
            let _ = save;
        }
        let _ = has_arrow;
        let body = self.parse_block()?;
        if params.is_empty() {
            params.push(Param::new("it", Ty::Any));
        }
        Ok(Expr::Closure { params, body, value: None })
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
            Tok::Template => {
                self.bump();
                self.parse_string_template(&t.text)
            }
            Tok::Ident => {
                self.bump();
                if self.at_punct("<") && self.nxt(1).is_punct(">") {
                    // Foo<Bar> constructor — skip generics
                } else if self.at_punct("<") {
                    // heuristic generic call or comparison; treat as generic when followed by Ident then ( or >
                    let save = self.pos;
                    if self.nxt(1).kind == Tok::Ident
                        && (self.nxt(2).is_punct(">(") || self.nxt(2).is_punct(">") || self.nxt(2).is_punct(","))
                    {
                        if self.nxt(2).is_punct(">(") {
                            self.skip_generics()?;
                            self.bump(); // (
                            let mut args = self.parse_args_rest()?;
                            let _ = &mut args;
                            return Ok(Expr::New { class: t.text, args });
                        }
                    }
                    self.pos = save;
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
                "if" => self.parse_if_expr(),
                "when" => {
                    let (stmt, value) = self.parse_when()?;
                    match value {
                        Some(v) => Ok(v),
                        None => {
                            // wrap statement into IIFE
                            Ok(Expr::Call {
                                callee: Box::new(Expr::Closure {
                                    params: vec![],
                                    body: vec![stmt],
                                    value: None,
                                }),
                                args: vec![],
                            })
                        }
                    }
                }
                "try" => {
                    // try as expression
                    let body = self.parse_block()?;
                    let mut catches = Vec::new();
                    while self.at_kw("catch") {
                        self.bump();
                        self.expect_punct("(")?;
                        let mut var = None;
                        if self.cur().kind == Tok::Ident {
                            var = Some(self.bump().text);
                            if self.eat_punct(":") {
                                let _ = self.parse_type()?;
                            }
                        }
                        self.expect_punct(")")?;
                        let cbody = self.parse_block()?;
                        catches.push(CatchClause { var, ty: None, body: cbody });
                    }
                    let mut finally = None;
                    if self.eat_kw("finally") {
                        finally = Some(self.parse_block()?);
                    }
                    Ok(Expr::Call {
                        callee: Box::new(Expr::Closure {
                            params: vec![],
                            body: vec![Stmt::Try { body, catches, finally }],
                            value: None,
                        }),
                        args: vec![],
                    })
                }
                "object" => Err(self.unsupported("object expressions")),
                "fun" => {
                    // anonymous function
                    self.bump();
                    if self.cur().kind == Tok::Ident {
                        self.bump();
                    }
                    let f = self.parse_fun_rest(None)?;
                    Ok(Expr::Closure {
                        params: f.params,
                        body: f.body,
                        value: None,
                    })
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
                if t.text == "[" {
                    self.bump();
                    let mut items = Vec::new();
                    while !self.at_punct("]") {
                        if self.cur().kind == Tok::Eof {
                            return Err(self.err("unexpected end of array"));
                        }
                        items.push(self.parse_expr()?);
                        if !self.eat_punct(",") {
                            break;
                        }
                    }
                    self.expect_punct("]")?;
                    return Ok(Expr::Array(items));
                }
                if t.text == "{" {
                    return self.parse_lambda();
                }
                Err(self.err(format!("unexpected token '{}'", t.text)))
            }
            _ => Err(self.err(format!("unexpected token '{}'", t.text))),
        }
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

    /// Kotlin string template: parts separated by $ident or ${expr}
    fn parse_string_template(&mut self, raw: &str) -> PResult<Expr> {
        let mut parts: Vec<InterpPart> = Vec::new();
        let chars: Vec<char> = raw.chars().collect();
        let mut lit = String::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' && i + 1 < chars.len() {
                lit.push(c);
                lit.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '$' && i + 1 < chars.len() {
                if chars[i + 1] == '{' {
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
                        return Err(self.err("unterminated string template"));
                    }
                    let inner: String = chars[i + 2..j].iter().collect();
                    let sub = parse_kotlin(&inner, &self.file)?;
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
                        self.err("empty string template expression")
                    })?));
                    i = j + 1;
                    continue;
                }
                // $ident
                if chars[i + 1].is_alphabetic() || chars[i + 1] == '_' {
                    if !lit.is_empty() {
                        parts.push(InterpPart::Lit(transpiler_core::lexer::unescape(&lit)));
                        lit.clear();
                    }
                    let mut j = i + 1;
                    while j < chars.len()
                        && (chars[j].is_alphanumeric() || chars[j] == '_')
                    {
                        j += 1;
                    }
                    let name: String = chars[i + 1..j].iter().collect();
                    parts.push(InterpPart::E(Expr::Ident(name)));
                    i = j;
                    continue;
                }
            }
            lit.push(c);
            i += 1;
        }
        if !lit.is_empty() {
            parts.push(InterpPart::Lit(transpiler_core::lexer::unescape(&lit)));
        }
        Ok(Expr::Interp { parts })
    }
}

/// last expression of a block becomes the block's value (Kotlin blocks are
/// expressions); we approximate by finding a trailing expression.
fn block_to_value(b: Block) -> Expr {
    match block_to_option_value(b) {
        Some(e) => e,
        None => Expr::Unit,
    }
}

fn block_to_option_value(mut b: Block) -> Option<Expr> {
    if let Some(Stmt::Expr(e)) = b.last() {
        let e = e.clone();
        b.pop();
        if b.is_empty() {
            return Some(e);
        }
        // block with trailing expression -> IIFE
        return Some(Expr::Call {
            callee: Box::new(Expr::Closure { params: vec![], body: b, value: None }),
            args: vec![],
        });
    }
    if let Some(Stmt::Return(Some(e))) = b.last() {
        let e = e.clone();
        b.pop();
        if b.is_empty() {
            return Some(e);
        }
        return Some(Expr::Call {
            callee: Box::new(Expr::Closure { params: vec![], body: b, value: None }),
            args: vec![],
        });
    }
    None
}

#![forbid(unsafe_code)]
//! The unified typed intermediate representation (IR).
//!
//! Every front-end (Java, JavaScript, Kotlin, Python, Go, TypeScript) lowers
//! into exactly this representation, and the Rust back-end generates code
//! from it. Dynamic languages (JS/TS/Python) use `Ty::Any` values; statically
//! typed languages carry their types through.

/// Source language of the module currently being compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    JavaScript,
    TypeScript,
    Python,
    Go,
    Java,
    Kotlin,
}

impl Lang {
    pub fn dynamic(self) -> bool {
        matches!(self, Lang::JavaScript | Lang::TypeScript | Lang::Python)
    }
    pub fn name(self) -> &'static str {
        match self {
            Lang::JavaScript => "javascript",
            Lang::TypeScript => "typescript",
            Lang::Python => "python",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::Kotlin => "kotlin",
        }
    }
}

/// A whole translation unit (one source file).
#[derive(Debug, Clone)]
pub struct Module {
    pub lang: Lang,
    pub name: String,
    pub imports: Vec<String>,
    pub decls: Vec<Decl>,
}

/// Top-level declarations.
#[derive(Debug, Clone)]
pub enum Decl {
    Function(FunctionDecl),
    Class(ClassDecl),
    Global(GlobalDecl),
    TypeAlias { name: String, ty: Ty },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Default,
    Public,
    Private,
    Protected,
    Internal,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
    pub default: Option<Expr>,
    pub variadic: bool,
}

impl Param {
    pub fn new(name: &str, ty: Ty) -> Self {
        Param { name: name.to_string(), ty, default: None, variadic: false }
    }
}

#[derive(Debug, Clone)]
pub struct FunctionDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Ty,
    pub body: Block,
    pub is_static: bool,
    pub is_async: bool,
    pub is_ctor: bool,
    pub visibility: Visibility,
    /// Extra names captured for closures that need them (JS `arguments` etc).
    pub lang_name: Option<String>,
}

impl FunctionDecl {
    pub fn new(name: &str, params: Vec<Param>, ret: Ty, body: Block) -> Self {
        FunctionDecl {
            name: name.to_string(),
            params,
            ret,
            body,
            is_static: false,
            is_async: false,
            is_ctor: false,
            visibility: Visibility::Default,
            lang_name: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    Class,
    Struct,
    Interface,
    Object,
    Enum,
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub name: String,
    pub ty: Ty,
    pub init: Option<Expr>,
    pub is_static: bool,
    pub mutable: bool,
    pub visibility: Visibility,
}

#[derive(Debug, Clone)]
pub struct ClassDecl {
    pub name: String,
    pub extends: Option<String>,
    pub fields: Vec<FieldDecl>,
    pub methods: Vec<FunctionDecl>,
    pub ctor: Option<FunctionDecl>,
    pub kind: ClassKind,
    pub is_data: bool,
}

#[derive(Debug, Clone)]
pub struct GlobalDecl {
    pub name: String,
    pub ty: Ty,
    pub init: Option<Expr>,
    pub mutable: bool,
    pub visibility: Visibility,
}

/// Types used by the IR. Deliberately simple: the back-end maps them onto
/// concrete safe-Rust types (`i32/i64/f64/bool/String/Rt<..>/Value`).
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Any,
    Int,
    Long,
    Float,
    Double,
    Bool,
    Char,
    Str,
    Void,
    Array(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Option(Box<Ty>),
    Class(String),
    Func { params: Vec<Ty>, ret: Box<Ty> },
    /// Named type parameter / generic (erased to Any or concrete at use-site).
    Named(String),
}

impl Ty {
    pub fn class(n: &str) -> Ty {
        Ty::Class(n.to_string())
    }
    pub fn array(t: Ty) -> Ty {
        Ty::Array(Box::new(t))
    }
    pub fn is_any(&self) -> bool {
        matches!(self, Ty::Any | Ty::Named(_))
    }
    pub fn is_numeric(&self) -> bool {
        matches!(self, Ty::Int | Ty::Long | Ty::Float | Ty::Double)
    }
}

pub type Block = Vec<Stmt>;

/// Statements.
#[derive(Debug, Clone)]
pub enum Stmt {
    Local {
        name: String,
        ty: Ty,
        init: Option<Expr>,
        mutable: bool,
    },
    /// Assignment; `op` is the compound operator when present (`a += b`).
    Assign {
        target: Expr,
        op: Option<BinOp>,
        value: Expr,
    },
    Expr(Expr),
    If {
        cond: Expr,
        then: Block,
        els: Block,
    },
    While {
        cond: Expr,
        body: Block,
    },
    For {
        init: Vec<Stmt>,
        cond: Option<Expr>,
        step: Vec<Stmt>,
        body: Block,
    },
    ForEach {
        var: String,
        /// second loop variable (`for k, v := range m` in Go)
        var2: Option<String>,
        var_ty: Ty,
        iter: Expr,
        body: Block,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    Block(Block),
    Match {
        subject: Expr,
        arms: Vec<MatchArm>,
        default: Option<Block>,
    },
    Throw(Expr),
    Try {
        body: Block,
        catches: Vec<CatchClause>,
        finally: Option<Block>,
    },
    /// Go `defer` -> scope-guard executed at end of function.
    Defer(Block),
    /// Go `go expr` -> thread spawn.
    Spawn(Expr),
    Comment(String),
    Pass,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Vec<Expr>,
    pub guard: Option<Expr>,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub struct CatchClause {
    pub var: Option<String>,
    pub ty: Option<Ty>,
    pub body: Block,
}

/// Expressions.
#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    Char(char),
    Bool(bool),
    Null,
    Undefined,
    Unit,
    Ident(String),
    This,
    Super,
    Binary {
        op: BinOp,
        l: Box<Expr>,
        r: Box<Expr>,
    },
    Unary {
        op: UnOp,
        e: Box<Expr>,
    },
    /// Postfix increment/decrement: `i++`, `i--`.
    Post {
        op: UnOp,
        e: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Method {
        obj: Box<Expr>,
        name: String,
        args: Vec<Expr>,
        nullsafe: bool,
    },
    Field {
        obj: Box<Expr>,
        name: String,
        nullsafe: bool,
    },
    Index {
        obj: Box<Expr>,
        idx: Box<Expr>,
    },
    Slice {
        obj: Box<Expr>,
        lo: Option<Box<Expr>>,
        hi: Option<Box<Expr>>,
    },
    Array(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    /// Object literal `{ x: 1, m() { ... } }`.
    Object(Vec<(String, Expr)>),
    New {
        class: String,
        args: Vec<Expr>,
    },
    Closure {
        params: Vec<Param>,
        body: Block,
        /// Expression-bodied closure (`=> expr`).
        value: Option<Box<Expr>>,
    },
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    Cast {
        e: Box<Expr>,
        ty: Ty,
    },
    /// Interpolated string: template literals, f-strings, Kotlin "$x".
    Interp {
        parts: Vec<InterpPart>,
    },
    Range {
        lo: Box<Expr>,
        hi: Box<Expr>,
        inclusive: bool,
    },
    /// Keyword argument at call-site (`f(x = 1)` in Python).
    Named {
        name: String,
        e: Box<Expr>,
    },
    /// Assignment used as an expression (JS `for (i = 0; ...)`).
    AssignExpr {
        target: Box<Expr>,
        op: Option<BinOp>,
        value: Box<Expr>,
    },
    Paren(Box<Expr>),
    Spread(Box<Expr>),
    Seq(Vec<Expr>),
    Await(Box<Expr>),
    /// Reference to a function by name (method references, `foo` as value).
    FuncRef(String),
}

#[derive(Debug, Clone)]
pub enum InterpPart {
    Lit(String),
    E(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    In,
    Nullish,
}

impl BinOp {
    pub fn symbol(&self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::FloorDiv => "//",
            BinOp::Mod => "%",
            BinOp::Pow => "**",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::UShr => ">>>",
            BinOp::In => "in",
            BinOp::Nullish => "??",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
    PreInc,
    PreDec,
}

impl Expr {
    pub fn ident(n: &str) -> Expr {
        Expr::Ident(n.to_string())
    }
    pub fn str_lit(s: &str) -> Expr {
        Expr::Str(s.to_string())
    }
    pub fn binary(op: BinOp, l: Expr, r: Expr) -> Expr {
        Expr::Binary { op, l: Box::new(l), r: Box::new(r) }
    }
    pub fn call(callee: Expr, args: Vec<Expr>) -> Expr {
        Expr::Call { callee: Box::new(callee), args }
    }
    pub fn method(obj: Expr, name: &str, args: Vec<Expr>) -> Expr {
        Expr::Method { obj: Box::new(obj), name: name.to_string(), args, nullsafe: false }
    }
    pub fn field(obj: Expr, name: &str) -> Expr {
        Expr::Field { obj: Box::new(obj), name: name.to_string(), nullsafe: false }
    }
    pub fn index(obj: Expr, idx: Expr) -> Expr {
        Expr::Index { obj: Box::new(obj), idx: Box::new(idx) }
    }
}

/// Count of nodes in an expression (used by the verifier to size formulas).
pub fn expr_size(e: &Expr) -> usize {
    1 + match e {
        Expr::Binary { l, r, .. } => expr_size(l) + expr_size(r),
        Expr::Unary { e, .. } | Expr::Post { e, .. } => expr_size(e),
        Expr::Call { callee, args } => expr_size(callee) + args.iter().map(expr_size).sum::<usize>(),
        Expr::Method { obj, args, .. } => expr_size(obj) + args.iter().map(expr_size).sum::<usize>(),
        Expr::Field { obj, .. } | Expr::Index { obj, .. } => expr_size(obj),
        Expr::Slice { obj, lo, hi } => {
            expr_size(obj) + lo.as_ref().map(|b| expr_size(b)).unwrap_or(0)
                + hi.as_ref().map(|b| expr_size(b)).unwrap_or(0)
        }
        Expr::Array(items) | Expr::Seq(items) => items.iter().map(expr_size).sum(),
        Expr::Dict(pairs) => {
            pairs.iter().map(|(k, v)| expr_size(k) + expr_size(v)).sum()
        }
        Expr::Object(fields) => fields.iter().map(|(_, v)| expr_size(v)).sum(),
        Expr::Interp { parts } => parts
            .iter()
            .map(|p| match p {
                InterpPart::Lit(_) => 0,
                InterpPart::E(e) => expr_size(e),
            })
            .sum(),
        Expr::Ternary { cond, then, els } => expr_size(cond) + expr_size(then) + expr_size(els),
        Expr::Cast { e, .. } => expr_size(e),
        Expr::Range { lo, hi, .. } => expr_size(lo) + expr_size(hi),
        Expr::Named { e, .. } | Expr::Paren(e) | Expr::Spread(e) | Expr::Await(e) => expr_size(e),
        Expr::AssignExpr { target, value, .. } => expr_size(target) + expr_size(value),
        Expr::Closure { params, body, value } => {
            params.len()
                + stmts_size(body)
                + value.as_ref().map(|v| expr_size(v)).unwrap_or(0)
        }
        _ => 0,
    }
}

pub fn stmts_size(b: &[Stmt]) -> usize {
    b.iter().map(stmt_size).sum()
}

pub fn stmt_size(s: &Stmt) -> usize {
    1 + match s {
        Stmt::Local { init, .. } => init.as_ref().map(expr_size).unwrap_or(0),
        Stmt::Assign { target, value, .. } => expr_size(target) + expr_size(value),
        Stmt::Expr(e) => expr_size(e),
        Stmt::If { cond, then, els } => expr_size(cond) + stmts_size(then) + stmts_size(els),
        Stmt::While { cond, body } => expr_size(cond) + stmts_size(body),
        Stmt::For { init, cond, step, body } => {
            stmts_size(init)
                + cond.as_ref().map(expr_size).unwrap_or(0)
                + stmts_size(step)
                + stmts_size(body)
        }
        Stmt::ForEach { iter, body, .. } => expr_size(iter) + stmts_size(body),
        Stmt::Return(e) => e.as_ref().map(expr_size).unwrap_or(0),
        Stmt::Block(b) => stmts_size(b),
        Stmt::Match { subject, arms, default } => {
            expr_size(subject)
                + arms.iter().map(|a| stmts_size(&a.body)).sum::<usize>()
                + default.as_ref().map(|d| stmts_size(d)).unwrap_or(0)
        }
        Stmt::Throw(e) => expr_size(e),
        Stmt::Try { body, catches, finally } => {
            stmts_size(body)
                + catches.iter().map(|c| stmts_size(&c.body)).sum::<usize>()
                + finally.as_ref().map(|f| stmts_size(f)).unwrap_or(0)
        }
        Stmt::Defer(b) => stmts_size(b),
        _ => 0,
    }
}

#![forbid(unsafe_code)]
//! Identifier mapping into valid, collision-free Rust identifiers.

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true",
    "try", "type", "unsafe", "use", "where", "while", "yield", "box", "priv", "macro",
    "abstract", "become", "do", "final", "macro_rules", "override", "typeof", "unsized",
    "virtual", "will_check_for_llvm_feature",
];

/// Map a source-language identifier to a valid Rust identifier.
pub fn rust_ident(name: &str) -> String {
    let mut n: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if n.is_empty() {
        return "_anon".to_string();
    }
    if n.chars().next().map_or(false, |c| c.is_ascii_digit()) {
        n = format!("_{}", n);
    }
    if RUST_KEYWORDS.contains(&n.as_str()) {
        n = format!("{}_", n);
    }
    n
}

/// Rust type name for classes (keep simple title case as-is).
pub fn rust_type_name(name: &str) -> String {
    rust_ident(name)
}

/// Escape a string as a Rust string literal body (without quotes).
pub fn rust_string_body(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if (c as u32) < 32 => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

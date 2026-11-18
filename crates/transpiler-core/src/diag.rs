#![forbid(unsafe_code)]
//! Diagnostics produced by lexers, parsers and code generators.

#[derive(Debug, Clone)]
pub struct Diag {
    pub line: usize,
    pub col: usize,
    pub message: String,
    pub file: String,
}

impl Diag {
    pub fn new(line: usize, col: usize, message: impl Into<String>) -> Self {
        Diag { line, col, message: message.into(), file: String::new() }
    }
    pub fn at(self, file: &str) -> Self {
        Diag { file: file.to_string(), ..self }
    }
}

impl std::fmt::Display for Diag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            if self.file.is_empty() { "<input>" } else { &self.file },
            self.line,
            self.col,
            self.message
        )
    }
}

impl std::error::Error for Diag {}

pub type PResult<T> = Result<T, Diag>;

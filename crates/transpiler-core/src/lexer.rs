#![forbid(unsafe_code)]
//! Hand-written lexer framework shared by the C-like front-ends
//! (JavaScript/TypeScript, Java, Kotlin, Go). Python has its own
//! indentation-aware lexer in `frontend-python`.

use crate::diag::{Diag, PResult};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident,
    Keyword,
    Int,
    Float,
    /// Decoded string literal content is in `text`.
    Str,
    Char,
    /// Raw template content (JS backtick / Kotlin "$" strings / Python
    /// f-strings / Go raw strings use `RawStr` instead).
    Template,
    RawStr,
    Punct,
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: Tok,
    pub text: String,
    pub line: usize,
    pub col: usize,
}

impl Token {
    pub fn is_kw(&self, kw: &str) -> bool {
        self.kind == Tok::Keyword && self.text == kw
    }
    pub fn is_punct(&self, p: &str) -> bool {
        self.kind == Tok::Punct && self.text == p
    }
    pub fn ident_name(&self) -> Option<&str> {
        if self.kind == Tok::Ident {
            Some(&self.text)
        } else {
            None
        }
    }
}

/// What a backtick means in this language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backtick {
    Template,
    RawString,
    None,
}

pub struct LexerConfig {
    pub keywords: Vec<&'static str>,
    /// All punctuators, any order (sorted internally longest-first).
    pub puncts: Vec<&'static str>,
    pub backtick: Backtick,
    /// If true, `"..."` strings containing `$` become `Tok::Template` (Kotlin).
    pub dollar_strings: bool,
    /// Character literals present (single quotes are chars, not strings).
    pub char_literals: bool,
}

impl LexerConfig {
    pub fn sorted_puncts(&self) -> Vec<&'static str> {
        let mut p = self.puncts.clone();
        p.sort_by(|a, b| b.len().cmp(&a.len()));
        p
    }
}

pub struct Lexer {
    src: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
    cfg_keywords: Vec<&'static str>,
    cfg_puncts: Vec<&'static str>,
    cfg_backtick: Backtick,
    cfg_dollar: bool,
    cfg_char: bool,
}

impl Lexer {
    pub fn new(src: &str, cfg: &LexerConfig) -> Self {
        Lexer {
            src: src.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
            cfg_keywords: cfg.keywords.clone(),
            cfg_puncts: cfg.sorted_puncts(),
            cfg_backtick: cfg.backtick,
            cfg_dollar: cfg.dollar_strings,
            cfg_char: cfg.char_literals,
        }
    }

    pub fn tokenize(mut self) -> PResult<Vec<Token>> {
        let mut out = Vec::new();
        loop {
            let t = self.next_token()?;
            let done = t.kind == Tok::Eof;
            out.push(t);
            if done {
                break;
            }
        }
        Ok(out)
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }
    fn peek2(&self) -> Option<char> {
        self.src.get(self.pos + 1).copied()
    }
    fn peek3(&self) -> Option<char> {
        self.src.get(self.pos + 2).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.src.get(self.pos).copied();
        if let Some(ch) = c {
            self.pos += 1;
            if ch == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        c
    }

    fn skip_trivia(&mut self) -> PResult<()> {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') if self.peek2() == Some('/') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.peek2() == Some('*') => {
                    let (l, c0) = (self.line, self.col);
                    self.bump();
                    self.bump();
                    loop {
                        match self.peek() {
                            None => {
                                return Err(Diag::new(l, c0, "unterminated block comment"))
                            }
                            Some('*') if self.peek2() == Some('/') => {
                                self.bump();
                                self.bump();
                                break;
                            }
                            _ => {
                                self.bump();
                            }
                        }
                    }
                }
                _ => break,
            }
        }
        Ok(())
    }

    fn next_token(&mut self) -> PResult<Token> {
        self.skip_trivia()?;
        let (line, col) = (self.line, self.col);
        let c = match self.peek() {
            None => {
                return Ok(Token { kind: Tok::Eof, text: String::new(), line, col });
            }
            Some(c) => c,
        };

        // Identifiers / keywords
        if c == '_' || c.is_alphabetic() || (c as u32) > 127 {
            let mut s = String::new();
            while let Some(ch) = self.peek() {
                if ch == '_' || ch.is_alphanumeric() || (ch as u32) > 127 {
                    s.push(ch);
                    self.bump();
                } else {
                    break;
                }
            }
            let kind = if self.cfg_keywords.contains(&s.as_str()) {
                Tok::Keyword
            } else {
                Tok::Ident
            };
            return Ok(Token { kind, text: s, line, col });
        }

        // Numbers
        if c.is_ascii_digit() {
            return self.lex_number(line, col);
        }

        // Strings
        if c == '"' || c == '\'' || (c == '`' && self.cfg_backtick != Backtick::None) {
            return self.lex_string(line, col);
        }

        // Punctuators
        let mut matched: Option<String> = None;
        for p in &self.cfg_puncts {
            let chars: Vec<char> = p.chars().collect();
            if self.src.len() >= chars.len() {
                let all_eq = chars.iter().enumerate().all(|(i, ch)| {
                    self.src.get(self.pos + i).copied() == Some(*ch)
                });
                if all_eq {
                    matched = Some(p.to_string());
                    break;
                }
            }
        }
        if let Some(p) = matched {
            let n = p.chars().count();
            for _ in 0..n {
                self.bump();
            }
            return Ok(Token { kind: Tok::Punct, text: p, line, col });
        }

        let _ = self.bump();
        Err(Diag::new(line, col, format!("unexpected character '{}'", c)))
    }

    fn lex_number(&mut self, line: usize, col: usize) -> PResult<Token> {
        let mut s = String::new();
        let mut is_float = false;
        // hex / binary / octal
        if self.peek() == Some('0')
            && matches!(self.peek2(), Some('x') | Some('X') | Some('b') | Some('B') | Some('o') | Some('O'))
        {
            s.push(self.bump().unwrap());
            s.push(self.bump().unwrap());
            while let Some(ch) = self.peek() {
                if ch.is_ascii_hexdigit() || ch == '_' {
                    if ch != '_' {
                        s.push(ch);
                    }
                    self.bump();
                } else {
                    break;
                }
            }
        } else {
            while let Some(ch) = self.peek() {
                if ch.is_ascii_digit() || ch == '_' {
                    if ch != '_' {
                        s.push(ch);
                    }
                    self.bump();
                } else {
                    break;
                }
            }
            if self.peek() == Some('.') && self.peek2().map_or(false, |c2| c2.is_ascii_digit()) {
                is_float = true;
                s.push(self.bump().unwrap());
                while let Some(ch) = self.peek() {
                    if ch.is_ascii_digit() || ch == '_' {
                        if ch != '_' {
                            s.push(ch);
                        }
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            if matches!(self.peek(), Some('e') | Some('E'))
                && self
                    .peek2()
                    .map_or(false, |c2| c2.is_ascii_digit() || c2 == '+' || c2 == '-')
            {
                is_float = true;
                s.push(self.bump().unwrap());
                if matches!(self.peek(), Some('+') | Some('-')) {
                    s.push(self.bump().unwrap());
                }
                while let Some(ch) = self.peek() {
                    if ch.is_ascii_digit() {
                        s.push(ch);
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
        }
        // numeric suffixes: JS `n`, Java `LlFfDd`, Go `i8..64 u8..64 f32 f64`, Kotlin `L f`
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphabetic() {
                // Go permits unsigned suffix 'u' possibly followed by digits; keep it simple:
                s.push(ch);
                self.bump();
                while let Some(c2) = self.peek() {
                    if c2.is_ascii_digit() {
                        s.push(c2);
                        self.bump();
                    } else {
                        break;
                    }
                }
            } else if ch == '.' && matches!(self.peek2(), Some('.')) {
                // range like 1..2 (Kotlin): do not consume
                break;
            } else {
                break;
            }
        }
        let kind = if is_float || s.contains('.') || s.contains('e') || s.contains('E') {
            Tok::Float
        } else {
            Tok::Int
        };
        Ok(Token { kind, text: s, line, col })
    }

    fn lex_string(&mut self, line: usize, col: usize) -> PResult<Token> {
        let quote = self.bump().unwrap();
        // Go raw string
        if quote == '`' && self.cfg_backtick == Backtick::RawString {
            let mut s = String::new();
            loop {
                match self.bump() {
                    None => return Err(Diag::new(line, col, "unterminated raw string")),
                    Some('`') => break,
                    Some(ch) => s.push(ch),
                }
            }
            return Ok(Token { kind: Tok::RawStr, text: s, line, col });
        }
        // Char literal (single character or escape)
        if quote == '\'' && self.cfg_char {
            let mut s = String::new();
            loop {
                match self.bump() {
                    None => return Err(Diag::new(line, col, "unterminated char literal")),
                    Some('\'') => break,
                    Some('\\') => {
                        let e = self.bump();
                        s.push('\\');
                        if let Some(ec) = e {
                            s.push(ec);
                        }
                    }
                    Some('\n') => {
                        return Err(Diag::new(line, col, "unterminated char literal"))
                    }
                    Some(ch) => s.push(ch),
                }
            }
            if quote == '\'' {
                let decoded = unescape(&s);
                let ch = decoded.chars().next().unwrap_or('\0');
                return Ok(Token { kind: Tok::Char, text: ch.to_string(), line, col });
            }
        }
        // Normal string
        let mut raw = String::new();
        loop {
            match self.bump() {
                None => return Err(Diag::new(line, col, "unterminated string literal")),
                Some(ch) if ch == quote => break,
                Some('\\') => {
                    let e = self.bump();
                    raw.push('\\');
                    if let Some(ec) = e {
                        raw.push(ec);
                    }
                }
                Some('\n') if quote != '`' => {
                    return Err(Diag::new(line, col, "unterminated string literal"))
                }
                Some(ch) => raw.push(ch),
            }
        }
        // Kotlin string templates
        if self.cfg_dollar && quote == '"' && raw.contains('$') {
            return Ok(Token { kind: Tok::Template, text: raw, line, col });
        }
        Ok(Token { kind: Tok::Str, text: unescape(&raw), line, col })
    }
}

/// Decode escape sequences. Template raw content is NOT decoded (kept raw).
pub fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            None => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('\'') => out.push('\''),
            Some('"') => out.push('"'),
            Some('`') => out.push('`'),
            Some('$') => out.push('$'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('v') => out.push('\u{b}'),
            Some('x') => {
                let mut h = String::new();
                for _ in 0..2 {
                    if let Some(&c2) = it.peek() {
                        if c2.is_ascii_hexdigit() {
                            h.push(c2);
                            it.next();
                        }
                    }
                }
                if let Ok(v) = u8::from_str_radix(&h, 16) {
                    out.push(v as char);
                }
            }
            Some('u') => {
                if it.peek() == Some(&'{') {
                    it.next();
                    let mut h = String::new();
                    while let Some(&c2) = it.peek() {
                        if c2 == '}' {
                            it.next();
                            break;
                        }
                        h.push(c2);
                        it.next();
                    }
                    if let Ok(v) = u32::from_str_radix(&h, 16) {
                        if let Some(ch) = char::from_u32(v) {
                            out.push(ch);
                        }
                    }
                } else {
                    let mut h = String::new();
                    for _ in 0..4 {
                        if let Some(&c2) = it.peek() {
                            if c2.is_ascii_hexdigit() {
                                h.push(c2);
                                it.next();
                            }
                        }
                    }
                    if let Ok(v) = u32::from_str_radix(&h, 16) {
                        if let Some(ch) = char::from_u32(v) {
                            out.push(ch);
                        }
                    }
                }
            }
            Some(other) => out.push(other),
        }
    }
    out
}

/// Parse an integer literal text (hex/binary/octal/decimal with `_`).
pub fn parse_int_text(text: &str) -> Option<i64> {
    let t = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let t = t.replace('_', "");
    let t = t.trim_end_matches(|c: char| c.is_ascii_digit() && false);
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return i64::from_str_radix(hex, 16).ok();
    }
    if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        return i64::from_str_radix(b, 2).ok();
    }
    if let Some(o) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        return i64::from_str_radix(o, 8).ok();
    }
    t.parse::<i64>().ok()
}

pub fn parse_float_text(text: &str) -> Option<f64> {
    let t: String = text.chars().filter(|&c| c != '_').collect();
    let t = t.trim_end_matches(|c: char| c.is_ascii_alphabetic()).to_string();
    t.parse::<f64>().ok()
}

#[allow(dead_code)]
fn unused(_: Option<char>, _: Option<char>) {}

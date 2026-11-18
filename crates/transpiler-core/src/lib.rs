#![forbid(unsafe_code)]
//! transpiler-core: the unified intermediate representation (IR) shared by all
//! six front-ends, plus a hand-written lexer framework for C-like languages.

pub mod diag;
pub mod ir;
pub mod lexer;
pub mod names;

pub use diag::{Diag, PResult};

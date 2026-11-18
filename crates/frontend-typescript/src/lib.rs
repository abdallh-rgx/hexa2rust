#![forbid(unsafe_code)]
//! TypeScript front-end: reuses the JavaScript parser with type annotations
//! enabled, plus TS-only declarations (interfaces, enums, type aliases).

use transpiler_core::diag::PResult;
use transpiler_core::ir::Module;
pub use frontend_javascript::parse_js;

/// Parse a TypeScript source file into the unified IR.
pub fn parse_typescript(src: &str, file: &str) -> PResult<Module> {
    parse_js(src, transpiler_core::ir::Lang::TypeScript, file)
}

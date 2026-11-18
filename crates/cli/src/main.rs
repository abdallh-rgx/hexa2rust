#![forbid(unsafe_code)]
//! hexa2rust CLI: transpile / verify / corpus subcommands.

use std::path::{Path, PathBuf};

mod dispatch;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = dispatch::run(&args);
    std::process::exit(code);
}

#[allow(dead_code)]
fn stub() {}

pub fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(1).unwrap().to_path_buf()
}

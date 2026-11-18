#![forbid(unsafe_code)]
#[test]
fn debug_tokens() {
    let src = "import os\nfrom typing import Dict\n\nMAX = 100\ncount = 0\n\ndef add(a, b=2):\n    return a + b\n";
    let toks = frontend_python::py_tokenize(src, "d.py").unwrap();
    for t in &toks {
        println!("{:?} {:?} {}:{}", t.kind, t.text, t.line, t.col);
    }
}

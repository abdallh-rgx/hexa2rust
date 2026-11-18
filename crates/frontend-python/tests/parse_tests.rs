#![forbid(unsafe_code)]
use frontend_python::parse_python;
use transpiler_core::ir::*;

#[test]
fn parses_basic_python() {
    let src = r#"
import os
import sys
from typing import Dict

MAX = 100
count = 0

def add(a, b=2):
    """docstring"""
    return a + b

class Point(Base):
    def __init__(self, x, y):
        self.x = x
        self.y = y
    def norm(self):
        return (self.x ** 2 + self.y ** 2) ** 0.5

def run(items):
    global count
    total = 0
    for it in items:
        total += it
    i = 0
    while i < 10:
        i += 1
    if total > 3 and total < 100:
        return add(total, MAX)
    elif total == 0:
        return None
    else:
        count += 1
    try:
        raise ValueError("boom")
    except ValueError as e:
        print(e)
    finally:
        count = 0
    squares = [x * x for x in items if x > 0]
    lookup = {k: v for k, v in pairs}
    msg = f"total={total}, ok"
    a, b = b, a
    lam = lambda x: x + 1
    t = a if a > b else b
    x = 7 // 2
    y = -7 // 2
    z = 7 % 3
    sl = items[1:3]
    del items[0]
    assert total >= 0, "negative"
    return len(squares)
"#;
    let m = parse_python(src, "t.py").expect("parse");
    assert!(m.decls.iter().any(|d| matches!(d, Decl::Function(f) if f.name == "add")));
    assert!(m.decls.iter().any(|d| matches!(d, Decl::Class(c) if c.name == "Point" && c.extends.as_deref() == Some("Base"))));
    assert!(m.decls.iter().any(|d| matches!(d, Decl::Global(g) if g.name == "MAX")));
}

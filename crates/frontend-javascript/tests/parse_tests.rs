#![forbid(unsafe_code)]
use frontend_javascript::parse_js;
use transpiler_core::ir::*;

#[test]
fn parses_basic_js() {
    let src = r#"
        import { x } from './y.js';
        const MAX = 100;
        let count = 0;
        function add(a, b = 2) { return a + b; }
        class Point extends Base {
            constructor(x, y) { super(x); this.x = x; this.y = y; }
            norm() { return Math.sqrt(this.x * this.x + this.y * this.y); }
            static origin() { return new Point(0, 0); }
        }
        export function run(items) {
            let total = 0;
            for (const it of items) { total += it; }
            for (let i = 0; i < 10; i++) { count++; }
            while (count > 0) { count--; }
            const p = new Point(1, 2);
            if (p.norm() > 0 && true || false) { return add(total, MAX); }
            try { throw new Error("boom"); } catch (e) { console.log(e); } finally { count = 0; }
            switch (count) { case 1: return 1; case 2: return 2; default: return 0; }
        }
    "#;
    let m = parse_js(src, Lang::JavaScript, "test.js").expect("parse");
    assert_eq!(m.imports.len(), 1);
    assert!(m.decls.iter().any(|d| matches!(d, Decl::Function(f) if f.name == "add")));
    assert!(m.decls.iter().any(|d| matches!(d, Decl::Class(c) if c.name == "Point" && c.extends.as_deref() == Some("Base"))));
    assert!(m.decls.iter().any(|d| matches!(d, Decl::Global(g) if g.name == "MAX")));
}

#[test]
fn parses_arrows_templates_destructuring() {
    let src = r#"
        const f = (a, b) => a * b;
        const g = x => { return x + 1; };
        const name = "world";
        const msg = `hello ${name}, ${1 + 2} times`;
        const [q, r] = [1, 2];
        const { a, b: c } = obj;
        const nums = [1, ...rest, 5];
        export default f;
    "#;
    let m = parse_js(src, Lang::JavaScript, "t.js").expect("parse2");
    assert!(m.decls.iter().any(|d| matches!(d, Decl::Global(g) if g.name == "f")));
}

#[test]
fn parses_typescript() {
    let src = r#"
        interface Shape { area(): number; name: string; }
        type ID = string | number;
        enum Color { Red, Green = 5, Blue }
        class Circle implements Shape {
            private readonly r: number;
            constructor(r: number) { this.r = r; }
            area(): number { return 3.14159 * this.r ** 2; }
        }
        function size<T extends Shape>(s: T): number { return s.area(); }
        const v = x as unknown as string;
    "#;
    let m = parse_js(src, Lang::TypeScript, "t.ts").expect("parse ts");
    assert!(m.decls.iter().any(|d| matches!(d, Decl::Class(c) if c.kind == ClassKind::Interface)));
    assert!(m.decls.iter().any(|d| matches!(d, Decl::Class(c) if c.kind == ClassKind::Enum)));
}

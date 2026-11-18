# hexa2rust

**Source-to-source transpiler: Java, JavaScript, Kotlin, Python, Go, TypeScript → SAFE Rust.**

- Output is **100% safe Rust**: every generated file starts with `#![forbid(unsafe_code)]`.
- The transpiler itself is written in **100% safe Rust** (`#![forbid(unsafe_code)]` in every crate).
- **Real parsing** — hand-written lexers and recursive-descent parsers produce an AST for each language. No regex/text substitution anywhere.
- **Unified IR** — all six front-ends lower to one typed intermediate representation; a single Rust back-end generates code from it.
- **Formal verification** — a Z3-based verifier proves semantic equivalence of arithmetic/logic fragments between source semantics and generated-Rust semantics (e.g. it *disproves* the naive translation of Python floor division and *proves* the corrected one).

## Pipeline

```
 source (.java/.js/.kt/.py/.go/.ts)
    │
    ▼
 front-end (hand-written lexer + recursive-descent parser)
    │
    ▼
 unified typed IR  (transpiler-core: Module/Decl/Stmt/Expr/Ty)
    │                          │
    ▼                          ▼
 backend-rust            verifier-z3
 (IR -> safe Rust)       (SMT-LIB2 + Z3 equivalence proofs)
    │
    ▼
 safe Rust + transpiler-rt   (#![forbid(unsafe_code)])
```

## Crates

| Crate | Purpose |
|---|---|
| `transpiler-core` | Shared IR (types, expressions, statements, declarations) + lexer framework |
| `transpiler-rt` | Runtime support library for generated code (dynamic `Value`, objects, arrays, maps) — safe Rust |
| `frontend-javascript` | JS2015+ lexer/parser → IR |
| `frontend-typescript` | TS lexer/parser → IR (typed superset of the JS front-end) |
| `frontend-python` | Python 3 lexer (offside-rule) + parser → IR |
| `frontend-go` | Go lexer/parser → IR |
| `frontend-java` | Java lexer/parser → IR |
| `frontend-kotlin` | Kotlin lexer/parser → IR |
| `backend-rust` | IR → safe Rust code generation |
| `verifier-z3` | SMT-LIB2 emission + Z3 equivalence proofs |
| `cli` | Command line driver |

## Usage

```bash
cargo run --release -p cli -- transpile tests/corpus/react_invariant.js -o out/invariant.rs
cargo run --release -p cli -- verify   tests/semantics/floordiv.py
cargo run --release -p cli -- corpus   tests/corpus --report reports/corpus.json
```

## Status / honest scope

Six hand-written parsers covering a large practical subset of each language
(classes, closures, generics-lite, collections, control flow, …) feeding one IR
and one Rust back-end, with Z3 proofs for the semantics that actually differ
(floor division, truthiness, …). The corpus harness runs the tool on real files
taken from large open-source projects (React/Facebook web, Google Guava/Gson,
Kubernetes, CPython ecosystem, VS Code) and reports per-file results.

## License

MIT

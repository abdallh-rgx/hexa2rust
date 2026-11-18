#!/bin/bash
# check_modules.sh — cargo check every generated module (tests_output/src/*.rs)
# as a standalone crate with #![forbid(unsafe_code)] + transpiler-rt.
# Reports per-module pass/fail and an overall compile-clean count.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
BASE=/tmp/modcheck
mkdir -p "$BASE/target"
pass=0; fail=0; failed=""
for f in tests_output/src/*.rs; do
  name=$(basename "$f" .rs)
  dir="$BASE/$name"
  rm -rf "$dir"; mkdir -p "$dir/src"
  cat > "$dir/Cargo.toml" <<EOF
[package]
name = "mod_$name"
version = "0.1.0"
edition = "2021"

[dependencies]
transpiler-rt = { path = "$ROOT/crates/transpiler-rt" }

[workspace]
EOF
  # generated output already starts with #![forbid(unsafe_code)] — keep exactly one
  if head -1 "$f" | grep -q "forbid(unsafe_code)"; then
    cp "$f" "$dir/src/lib.rs"
  else
    { echo "#![forbid(unsafe_code)]"; cat "$f"; } > "$dir/src/lib.rs"
  fi
  if (cd "$dir" && CARGO_TARGET_DIR="$BASE/target" cargo check --quiet > "$BASE/$name.log" 2>&1); then
    printf "[OK]   %s\n" "$name"; pass=$((pass+1))
  else
    printf "[FAIL] %s (%s errors)  log: %s/%s.log\n" "$name" "$(grep -c '^error' "$BASE/$name.log")" "$BASE" "$name"
    fail=$((fail+1)); failed="$failed $name"
  fi
done
echo "=== compile-clean: $pass/$((pass+fail)) ==="
[ -n "$failed" ] && echo "failed:$failed"
exit 0

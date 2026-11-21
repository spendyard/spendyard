#!/usr/bin/env bash
# Builds the core for JavaScript into crates/wasm/pkg: the npm package
# @spendyard/core, which Spendyard Cloud's Worker reads documents with.
#
#   scripts/build-wasm.sh
#
# Needs the wasm32-unknown-unknown target (rust-toolchain.toml installs it),
# wasm-bindgen-cli at the version crates/wasm pins
# (cargo install wasm-bindgen-cli --version <it> --locked), and wasm-opt
# (binaryen) if it's to be optimised: without it, the package is just bigger.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
pkg="$root/crates/wasm/pkg"

cd "$root"

cargo build -p spendyard-wasm --profile wasm --target wasm32-unknown-unknown --locked

wasm-bindgen --target web --out-dir "$pkg" --out-name spendyard_core \
  "target/wasm32-unknown-unknown/wasm/spendyard_wasm.wasm"

if command -v wasm-opt >/dev/null; then
  # The features named, not --all-features: newer encodings don't load in every runtime yet.
  wasm-opt -Oz --strip-debug \
    --enable-bulk-memory --enable-sign-ext --enable-nontrapping-float-to-int \
    --enable-mutable-globals --enable-reference-types --enable-multivalue \
    "$pkg/spendyard_core_bg.wasm" -o "$pkg/spendyard_core_bg.wasm"
else
  echo "no wasm-opt: the package isn't optimised" >&2
fi

version="$(cargo metadata --format-version 1 --no-deps | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>console.log(JSON.parse(s).packages.find(p=>p.name==="spendyard-wasm").version))')"

cat > "$pkg/package.json" <<JSON
{
  "name": "@spendyard/core",
  "version": "$version",
  "description": "The Spendyard core for JavaScript: reads receipts and invoices in a Worker or a browser.",
  "license": "Apache-2.0",
  "repository": "github:spendyard/spendyard",
  "type": "module",
  "main": "spendyard_core.js",
  "types": "spendyard_core.d.ts",
  "files": ["spendyard_core.js", "spendyard_core.d.ts", "spendyard_core_bg.wasm", "spendyard_core_bg.wasm.d.ts"],
  "sideEffects": false
}
JSON

rm -f "$pkg/.gitignore"

echo "built $pkg: $(wc -c < "$pkg/spendyard_core_bg.wasm" | tr -d ' ') bytes, $(gzip -9 -c "$pkg/spendyard_core_bg.wasm" | wc -c | tr -d ' ') gzipped"

// Every fixture read as WebAssembly (pkg/, scripts/build-wasm.sh) and natively
// (examples/read.rs): the answers must be the same, byte for byte. Spendyard
// Cloud reads with the one and the CLI with the other, and a push merges their
// rows. Run from the repository's root:
//
//   scripts/build-wasm.sh && node crates/wasm/tests/compare.mjs

import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import { initSync, read, version } from "../pkg/spendyard_core.js";

const fixtures = "crates/core/tests/fixtures";

initSync({ module: readFileSync("crates/wasm/pkg/spendyard_core_bg.wasm") });

execFileSync("cargo", ["build", "--quiet", "-p", "spendyard-wasm", "--example", "read"], { stdio: "inherit" });

let failed = 0;

for (const name of readdirSync(fixtures).filter((file) => !file.endsWith(".md"))) {
  const bytes = readFileSync(`${fixtures}/${name}`);
  const wasm = read(bytes, name);
  const native = execFileSync("target/debug/examples/read", [`${fixtures}/${name}`]).toString();

  if (wasm === native) {
    console.log(`same   ${name}`);
  } else {
    failed++;
    console.log(`DIFFER ${name}\n  native: ${native.slice(0, 200)}\n  wasm:   ${wasm.slice(0, 200)}`);
  }
}

console.log(`\nspendyard-core ${version()}: ${failed === 0 ? "WebAssembly reads every fixture as native does" : `${failed} differ`}`);
process.exit(failed === 0 ? 0 : 1);

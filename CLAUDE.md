# Spendyard

An open-source Rust binary that reads receipts and invoices, plus (later) a
SaaS spend dashboard. The design is in [docs/design.md](docs/design.md);
milestones are in §12 there.

## Layout

A Cargo workspace:

- `crates/core` (`spendyard-core`): everything about a single document:
  reading it, the vendor catalog (`vendors/`, compiled in by its `build.rs`),
  money, statements, prompts and parsing models' answers, the result and
  checking it, the rows a store keeps. No file, network, clock or OS: it
  builds for `wasm32-unknown-unknown`, because Spendyard Cloud runs it in a
  Worker. The test fixtures are its, in `crates/core/tests/fixtures`.
- `crates/models` (`spendyard-models`): the model providers over HTTP.
- `crates/wasm` (`spendyard-wasm`): the core for JavaScript, built by
  `scripts/build-wasm.sh` into the npm package `@spendyard/core`.
- `src/` (`spendyard`): the CLI: commands, the local store, mailboxes, the
  keychain, MCP.

Keep filesystem, keychain, terminal and network code out of the core. If
something in it won't build for `wasm32-unknown-unknown`, CI says so.

## Code style

Readable over clever. Give the code room:

- A blank line between steps in a function, after an `if`/`match`/`for`
  block, and before the final expression or `return`.
- Name intermediate values instead of chaining long expressions or nesting
  closures.
- Break long calls and struct literals over several lines.
- Small functions with names that say what they do; `main` dispatches.
- Doc comments on everything public.

## Checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p spendyard-core --target wasm32-unknown-unknown
scripts/build-wasm.sh && node crates/wasm/tests/compare.mjs   # the core as WebAssembly reads as native does
```

Commit straight to `main`.

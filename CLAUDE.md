# Spendyard

An open-source Rust binary that reads receipts and invoices, plus (later) a
SaaS spend dashboard. The design is in [docs/design.md](docs/design.md);
milestones are in §12 there.

## Layout

One crate for now: `src/lib.rs` is the library (everything about a single
document), `src/main.rs` the CLI. Keep filesystem, keychain and terminal
code out of the library. It moves into its own WebAssembly-ready crate when
the cloud work starts.

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
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Commit straight to `main`.

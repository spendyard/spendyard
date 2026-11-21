//! Reads a file natively, exactly as the WebAssembly `read` does, and prints
//! the answer: for tests/compare.mjs to check the two agree.

fn main() {
    let path = std::env::args().nth(1).expect("a file to read");

    let bytes = std::fs::read(&path).expect("a readable file");

    print!("{}", spendyard_wasm::read_json(&bytes));
}

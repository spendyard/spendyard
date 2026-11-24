//! Reads a file natively, exactly as the WebAssembly `read` does, and prints
//! the answer: for tests/compare.mjs to check the two agree.

fn main() {
    let path = std::env::args().nth(1).expect("a file to read");

    let bytes = std::fs::read(&path).expect("a readable file");

    // Named as compare.mjs names it to the WebAssembly: by its file name.
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());

    print!("{}", spendyard_wasm::read_json(&bytes, name.as_deref()));
}

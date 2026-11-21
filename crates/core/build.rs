//! Compiles the vendor catalog into the binary: every `vendors/*.toml`
//! file becomes one `[[vendor]]` entry, and `vendors/_processors.toml` is
//! added as it is.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

fn main() {
    // The repository's vendors/, two levels up from this crate.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");

    let dir = Path::new(&manifest).join("../../vendors");

    println!("cargo:rerun-if-changed={}", dir.display());

    let mut files: Vec<_> = fs::read_dir(&dir)
        .expect("a vendors/ folder")
        .map(|entry| entry.expect("a readable entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect();

    files.sort();

    let mut catalog = String::new();

    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();

        let text = fs::read_to_string(&path).expect("a readable vendor file");

        println!("cargo:rerun-if-changed={}", path.display());

        if name.starts_with('_') {
            catalog.push_str(&text);
            catalog.push('\n');

            continue;
        }

        let id = name.trim_end_matches(".toml");

        writeln!(catalog, "[[vendor]]\nfile = {id:?}\n{text}").unwrap();
    }

    let out = std::env::var("OUT_DIR").unwrap();

    fs::write(Path::new(&out).join("catalog.toml"), catalog).unwrap();
}

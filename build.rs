// Embeds icons/*.svg as `ICONS: &[(name, svg)]`.
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=icons");
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("icons");
    let mut icons: Vec<(String, PathBuf)> = fs::read_dir(&dir)
        .expect("icons directory")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "svg"))
        .map(|path| (path.file_stem().unwrap().to_string_lossy().into_owned(), path))
        .collect();
    icons.sort();

    let mut code = String::from("pub static ICONS: &[(&str, &str)] = &[\n");
    for (name, path) in icons {
        code += &format!("    ({name:?}, include_str!({:?})),\n", path.display().to_string());
    }
    code += "];\n";
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("icons.rs");
    fs::write(out, code).unwrap();
}

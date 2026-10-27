//! Embeds the built frontend (`../dist`) into the executable so WorkDesk Chrome is a single file.
use std::{env, fs, path::Path};

fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            walk(&p, base, out);
        } else {
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
            out.push((rel, p.canonicalize().unwrap().to_string_lossy().into_owned()));
        }
    }
}

fn main() {
    let dist = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("../dist");
    println!("cargo:rerun-if-changed=../dist");
    println!("cargo:rerun-if-changed=assets");
    let mut files = vec![];
    walk(&dist, &dist, &mut files);
    if files.is_empty() {
        println!("cargo:warning=../dist is empty - run `npm run build` first (the app would show a blank page)");
    }
    let mut code = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for (rel, abs) in &files {
        code.push_str(&format!("    ({rel:?}, include_bytes!({abs:?})),\n"));
    }
    code.push_str("];\n");
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("assets.rs"), code).unwrap();
}

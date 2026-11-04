//! Embeds every file of $BC_PAYLOAD_DIR (relative paths preserved) into the setup executable.
use std::io::Write;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).expect("payload dir").flatten().collect();
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            walk(&p, base, out);
        } else {
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().replace('/', "\\");
            out.push((rel, p));
        }
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=BC_PAYLOAD_DIR");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut f = std::fs::File::create(out_dir.join("payload.rs")).unwrap();
    let mut files = Vec::new();
    if let Ok(dir) = std::env::var("BC_PAYLOAD_DIR") {
        let dir = PathBuf::from(dir);
        println!("cargo:rerun-if-changed={}", dir.display());
        walk(&dir, &dir, &mut files);
    }
    writeln!(f, "pub static FILES: &[(&str, &[u8])] = &[").unwrap();
    for (rel, p) in &files {
        println!("cargo:rerun-if-changed={}", p.display());
        writeln!(f, "    ({:?}, include_bytes!({:?})),", rel, p.display().to_string()).unwrap();
    }
    writeln!(f, "];").unwrap();
    // Windows manifest: run as invoker (per-user install, never asks for administrator).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:level='asInvoker' uiAccess='false'");
    }
}

use std::{env, fs, path::{Path, PathBuf}, process::Command};
fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("source directory") {
        let path = entry.unwrap().path();
        if path.is_dir() { collect(&path, files); }
        else { files.push(path); }
    }
}
fn main() {
    let app = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = app.join("../..").canonicalize().unwrap();
    let mut files = vec![root.join("Cargo.lock"), root.join("Cargo.toml"), app.join("build.rs")];
    for package in ["crates/libmtg-engine", "crates/libmtg-decklist", "apps/libmtg-forge"] {
        let dir = root.join(package);
        files.push(dir.join("Cargo.toml"));
        println!("cargo:rerun-if-changed={}", dir.join("src").display());
        collect(&dir.join("src"), &mut files);
    }
    files.sort_by_key(|p| p.strip_prefix(&root).unwrap_or(p).to_string_lossy().replace('\\', "/"));
    let mut hash = 0xcbf29ce484222325u64;
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        for b in name.bytes().chain([0]).chain(fs::read(&path).expect("read source")).chain([0]) {
            hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }
    println!("cargo:rustc-env=FORGE_SOURCE_FINGERPRINT=fnv1a64:{hash:016x}");
    let rev = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&app).output().ok()
        .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=FORGE_GIT_REVISION={rev}");
    // Source hash above identifies uncommitted edits too; revision is supplementary.
    println!("cargo:rerun-if-changed={}", root.parent().unwrap().join(".git/HEAD").display());
}

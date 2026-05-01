use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

// Pinned commit of szechyjs/mbelib.  Master HEAD has been stable since
// 2019.  Pinning keeps the committed ambe/tests/fixtures/mbelib_golden.bin
// valid; bump this only when intentionally regenerating the golden.
const MBELIB_REPO: &str = "https://github.com/szechyjs/mbelib.git";
const MBELIB_COMMIT: &str = "9a04ed5c78176a9965f3d43f7aa1b1f5330e771f";

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("failed to run `git {}`: {e}", args.join(" ")));
    assert!(status.success(), "`git {}` failed", args.join(" "));
}

fn fetch_mbelib(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create mbelib dir");
    git(dir, &["init", "-q"]);
    git(dir, &["remote", "add", "origin", MBELIB_REPO]);
    git(dir, &["fetch", "--depth=1", "-q", "origin", MBELIB_COMMIT]);
    git(dir, &["checkout", "-q", "FETCH_HEAD"]);
}

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let mbelib_dir = out_dir.join("mbelib");

    // Check for a source file, not just .git -- handles partial fetches.
    if !mbelib_dir.join("mbelib.c").exists() {
        if mbelib_dir.exists() {
            std::fs::remove_dir_all(&mbelib_dir).expect("clean partial mbelib dir");
        }
        fetch_mbelib(&mbelib_dir);
    }

    println!("cargo:rerun-if-changed=build.rs");

    cc::Build::new()
        .file(mbelib_dir.join("ambe3600x2450.c"))
        .file(mbelib_dir.join("ambe3600x2400.c"))
        .file(mbelib_dir.join("ecc.c"))
        .file(mbelib_dir.join("mbelib.c"))
        .include(&mbelib_dir)
        .warnings(false)
        .opt_level(2)
        .compile("mbe");
}

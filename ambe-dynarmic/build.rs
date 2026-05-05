use std::path::PathBuf;

fn main() {
    check_prerequisites();

    let vendor = PathBuf::from("vendor/md380_vocoder_dynarmic");

    let dst = cmake::Config::new(&vendor)
        .build_target("md380_vocoder")
        .build();

    let build_dir = dst.join("build");
    let dynarmic_dir = build_dir.join("_deps/dynarmic-build");

    // md380_vocoder and dynarmic transitive deps, in link order.
    link_search(&build_dir);
    println!("cargo:rustc-link-lib=static=md380_vocoder");

    link_search(dynarmic_dir.join("src/dynarmic"));
    println!("cargo:rustc-link-lib=static=dynarmic");

    link_search(dynarmic_dir.join("externals/zydis/zycore"));
    println!("cargo:rustc-link-lib=static=Zycore");

    link_search(dynarmic_dir.join("externals/zydis"));
    println!("cargo:rustc-link-lib=static=Zydis");

    // fmt has a debug-suffix variant; find whichever name was produced.
    let fmt_dir = dynarmic_dir.join("externals/fmt");
    link_search(&fmt_dir);
    let fmt_name = find_lib(&fmt_dir, "fmt").expect("libfmt*.a not found in dynarmic build");
    println!("cargo:rustc-link-lib=static={fmt_name}");

    link_search(dynarmic_dir.join("externals/mcl/src"));
    println!("cargo:rustc-link-lib=static=mcl");

    println!("cargo:rustc-link-lib=stdc++");

    // Compile the C++ shim that re-exports md380_vocoder symbols with extern "C" linkage.
    cc::Build::new()
        .cpp(true)
        .include(&vendor)
        .file("src/shim.cpp")
        .compile("md380shim");

    println!("cargo:rerun-if-changed=src/shim.cpp");
    println!("cargo:rerun-if-changed=vendor/md380_vocoder_dynarmic/md380_vocoder.h");
    println!("cargo:rerun-if-changed=vendor/md380_vocoder_dynarmic/CMakeLists.txt");
}

/// Verify all system prerequisites before invoking cmake.
fn check_prerequisites() {
    // Boost doesn't always ship a .pc file; search for the header directly.
    let boost_found = ["/usr/include", "/usr/local/include", "/opt/homebrew/include"]
        .iter()
        .any(|dir| std::path::Path::new(dir).join("boost/version.hpp").exists());
    if !boost_found {
        panic!(
            "Boost headers not found (required by dynarmic).\n\
             Install with: apt install libboost-dev  \
             OR  brew install boost"
        );
    }

    for tool in &["cmake", "git", "python3", "unzip", "xxd"] {
        which(tool).unwrap_or_else(|| {
            panic!(
                "Required build tool not found: {tool}\n\
                 Install with: apt install {tool}  \
                 OR  brew install {tool}"
            )
        });
    }
}

/// Return the path of `tool` if it exists on PATH, else None.
fn which(tool: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let candidate = dir.join(tool);
            candidate.is_file().then_some(candidate)
        })
    })
}

fn link_search(dir: impl AsRef<std::path::Path>) {
    println!("cargo:rustc-link-search=native={}", dir.as_ref().display());
}

/// Find the stem of a `lib<stem>.a` (or `lib<stem>d.a`) in `dir`.
fn find_lib(dir: impl AsRef<std::path::Path>, base: &str) -> Option<String> {
    let dir = dir.as_ref();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.starts_with("lib") && s.ends_with(".a") {
            let stem = &s[3..s.len() - 2]; // strip "lib" prefix and ".a" suffix
            if stem == base || stem == format!("{base}d") {
                return Some(stem.to_string());
            }
        }
    }
    None
}

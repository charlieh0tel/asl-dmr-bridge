use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

fn main() {
    check_prerequisites();

    let vendor = PathBuf::from("vendor/md380_vocoder_dynarmic");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));

    // ambe/dynarmic-sys -> ambe -> workspace root
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    // Pre-clone dynarmic into target/dynarmic-src from outside the workspace.
    // cmake's FetchContent git clone runs with the cmake source dir as CWD,
    // which is inside the workspace; git then discovers the workspace .git and
    // uses its object store as an alternate, producing "invalid object" errors.
    // By pre-cloning from the workspace parent we avoid that contamination, and
    // FETCHCONTENT_SOURCE_DIR_DYNARMIC tells cmake to use our clone directly,
    // skipping its own git operations for dynarmic entirely.
    let dynarmic_src = ensure_dynarmic_src(&workspace_root);

    let mut config = cmake::Config::new(&vendor);
    config
        // dynarmic's robin-map external declares cmake_minimum_required(VERSION 2.x),
        // which cmake 4.x rejects.  Override the policy floor to keep cmake 4 happy
        // without touching upstream.
        .define("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
        // Skip FetchContent git-fetch update steps for dynarmic's sub-deps
        // (mcl, fmt, etc.).  The sub-dep sources are already cloned on first
        // build; updates are not needed at incremental-build time.
        .define("FETCHCONTENT_UPDATES_DISCONNECTED", "ON")
        // Use the pre-cloned dynarmic source; cmake skips its own git operations
        // for dynarmic (no clone, no checkout, no fetch).
        .define("FETCHCONTENT_SOURCE_DIR_DYNARMIC", &dynarmic_src)
        .build_target("md380_vocoder");

    // Prevent cmake's git clones of dynarmic's sub-dependencies (mcl, fmt,
    // etc.) from discovering the workspace .git.  Those clones run from within
    // the cmake build directory, which sits inside the workspace; without this
    // ceiling, git traverses up to the workspace root, finds .git, and uses
    // the workspace object store as an alternate -- causing "invalid object"
    // errors.  Setting the ceiling to the workspace root stops git before it
    // reaches .git.
    config.env("GIT_CEILING_DIRECTORIES", &workspace_root);

    // Ubuntu 22.04 ships xxd 8.2 (vim-common), which lacks `-n NAME`.
    // The flag arrived in vim 9.0.  Install a shim that emulates it.
    if !xxd_supports_n() {
        let shim_dir = install_xxd_shim(&out_dir);
        let path = std::env::var("PATH").unwrap_or_default();
        config.env("PATH", format!("{}:{path}", shim_dir.display()));
    }

    let dst = config.build();

    let build_dir = dst.join("build");
    let dynarmic_dir = build_dir.join("_deps/dynarmic-build");

    // md380_vocoder and dynarmic transitive deps, in link order.
    link_search(&build_dir);
    println!("cargo:rustc-link-lib=static=md380_vocoder");

    link_search(dynarmic_dir.join("src/dynarmic"));
    println!("cargo:rustc-link-lib=static=dynarmic");

    // Zydis is dynarmic's x86 disassembler; not built on non-x86
    // hosts.  Detect presence rather than gating on target arch.
    let zycore_dir = dynarmic_dir.join("externals/zydis/zycore");
    if find_lib(&zycore_dir, "Zycore").is_some() {
        link_search(&zycore_dir);
        println!("cargo:rustc-link-lib=static=Zycore");
    }
    let zydis_dir = dynarmic_dir.join("externals/zydis");
    if find_lib(&zydis_dir, "Zydis").is_some() {
        link_search(&zydis_dir);
        println!("cargo:rustc-link-lib=static=Zydis");
    }

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
    println!("cargo:rerun-if-changed=vendor/md380_vocoder_dynarmic/md380_vocoder.cpp");
    println!("cargo:rerun-if-changed=vendor/md380_vocoder_dynarmic/md380_vocoder.h");
    println!("cargo:rerun-if-changed=vendor/md380_vocoder_dynarmic/CMakeLists.txt");
}

/// Clone dynarmic into `workspace_root/target/dynarmic-src` if not already
/// present.  The clone runs with CWD set to the workspace parent so git
/// starts outside the workspace and cannot discover the workspace .git.
/// Returns the path to the cloned source tree.
fn ensure_dynarmic_src(workspace_root: &Path) -> PathBuf {
    let src_dir = workspace_root.join("target/dynarmic-src");
    if src_dir.join(".git").exists() {
        return src_dir;
    }
    // A partial clone (no .git) would cause git clone to fail; remove it.
    if src_dir.exists() {
        std::fs::remove_dir_all(&src_dir).expect("remove partial dynarmic-src");
    }
    let workspace_parent = workspace_root
        .parent()
        .expect("workspace root has a parent directory");
    let status = Command::new("git")
        .current_dir(workspace_parent)
        .args([
            "clone",
            "--depth=1",
            "https://github.com/yuzu-mirror/dynarmic.git",
            src_dir.to_str().expect("valid UTF-8 path"),
        ])
        .status()
        .expect("failed to spawn git clone for dynarmic");
    assert!(
        status.success(),
        "git clone of dynarmic into target/dynarmic-src failed"
    );
    src_dir
}

/// Verify all system prerequisites before invoking cmake.
fn check_prerequisites() {
    // Boost doesn't always ship a .pc file; search for the header directly.
    let boost_found = [
        "/usr/include",
        "/usr/local/include",
        "/opt/homebrew/include",
    ]
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

/// True if xxd accepts `-n NAME` (vim 9.0+).  Ubuntu 22.04 ships vim 8.2, which doesn't.
fn xxd_supports_n() -> bool {
    Command::new("xxd")
        .args(["-i", "-n", "probe"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Write an `xxd` shim that emulates `-n NAME FILE` and return its directory.
/// Real xxd's auto-generated identifier maps non-alnum to _ (and prepends _ if leading digit);
/// the shim runs xxd without -n then sed-renames that identifier to NAME.
fn install_xxd_shim(out_dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = out_dir.join("xxd-shim");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("xxd");
    std::fs::write(
        &path,
        concat!(
            "#!/bin/sh\n",
            "if [ \"$1\" = \"-i\" ] && [ \"$2\" = \"-n\" ]; then\n",
            "    auto=$(printf %s \"$4\" | tr -c 'a-zA-Z0-9_' '_' | sed 's/^[0-9]/_&/')\n",
            "    /usr/bin/xxd -i \"$4\" | sed \"s/${auto}/$3/g\"\n",
            "else\n",
            "    exec /usr/bin/xxd \"$@\"\n",
            "fi\n",
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    dir
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

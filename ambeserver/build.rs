use std::env;
use std::fs;
use std::io;

use clap::CommandFactory;

include!("src/cli.rs");

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=src/cli.rs");

    let out_dir = std::path::PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let profile_dir = out_dir.ancestors().nth(3).expect("OUT_DIR layout");
    let man_dir = profile_dir.join("man");
    fs::create_dir_all(&man_dir)?;

    let cmd = Args::command();
    let mut buf = Vec::new();
    clap_mangen::Man::new(cmd).render(&mut buf)?;
    fs::write(man_dir.join("ambeserver.1"), &buf)?;
    Ok(())
}

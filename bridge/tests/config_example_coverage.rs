//! Drift guard for `config.example.toml`.
//!
//! Parses `bridge/src/config.rs` with `syn`, walks the user-facing
//! Config structs (anything that isn't `RuntimeConfig` or starts with
//! `Resolved`), and asserts every field name appears somewhere in
//! `config.example.toml`.  The example may have a field commented
//! out -- coverage is by name, not by activation.

use std::collections::BTreeSet;

const CONFIG_RS: &str = include_str!("../src/config.rs");
const EXAMPLE_TOML: &str = include_str!("../../config.example.toml");

fn is_user_facing_config(name: &str) -> bool {
    !name.starts_with("Resolved") && name != "RuntimeConfig"
}

#[test]
fn config_example_covers_every_struct_field() {
    let file: syn::File = syn::parse_str(CONFIG_RS).expect("parse config.rs");
    let mut fields: BTreeSet<String> = BTreeSet::new();
    for item in &file.items {
        let syn::Item::Struct(s) = item else { continue };
        let name = s.ident.to_string();
        if !name.ends_with("Config") || !is_user_facing_config(&name) {
            continue;
        }
        let syn::Fields::Named(named) = &s.fields else {
            continue;
        };
        for f in &named.named {
            if let Some(ident) = &f.ident {
                fields.insert(ident.to_string());
            }
        }
    }

    assert!(
        !fields.is_empty(),
        "no Config fields found -- syn parse drift?"
    );

    let missing: Vec<&String> = fields
        .iter()
        .filter(|f| !EXAMPLE_TOML.contains(f.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "config.example.toml missing field(s) from config.rs: {:?}",
        missing,
    );
}

//! `main.rs` embeds two optional local files via `include_bytes!`/`include_str!`
//! so the binary can carry a default OpenRouter API key and model without
//! passing flags every run. Both live in `local/`, which is gitignored.
//!
//! `include_*!` requires the file to exist at compile time, so this script
//! creates empty placeholders on first build. An empty file means "unset" --
//! see `embedded_api_key`/`embedded_default_model` in `main.rs`. Fill them
//! in locally with your real values; they're never read by anything except
//! this binary at compile time.

use std::path::Path;

fn main() {
    for path in ["local/openrouter_api_key.txt", "local/default_model.txt"] {
        let path = Path::new(path);
        if !path.exists() {
            std::fs::create_dir_all(path.parent().unwrap()).expect("failed to create local/");
            std::fs::write(path, "").expect("failed to create placeholder file");
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

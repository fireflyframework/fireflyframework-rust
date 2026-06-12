//! Captures the active `rustc` version at compile time so the startup
//! banner can print the toolchain that shipped the binary — the Rust
//! analog of the Go port reading `runtime.Version()` at runtime.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=banner.txt");
    println!("cargo:rerun-if-env-changed=RUSTC");

    // `rustc --version` prints e.g. "rustc 1.87.0 (17067e9ac 2025-05-09)";
    // the second word is the bare version, mirroring Go's
    // strings.TrimPrefix(runtime.Version(), "go").
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = Command::new(&rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|s| s.split_whitespace().nth(1).map(str::to_owned))
        // Fall back to the crate's MSRV when rustc cannot be invoked.
        .unwrap_or_else(|| {
            std::env::var("CARGO_PKG_RUST_VERSION").unwrap_or_else(|_| "unknown".to_string())
        });
    println!("cargo:rustc-env=FIREFLY_RUSTC_VERSION={version}");
}

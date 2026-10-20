//! Captures the identity a release helper must carry: Cargo version, target
//! triple, source commit and updater protocol. `main.rs` embeds them in the
//! binary and consults them at startup, and `scripts/release_catalog.py` reads
//! them back from the built executable.
//!
//! `HANE_SOURCE_SHA` is set by the release workflows to the 40-digit commit that
//! is built. Local builds without it carry the literal `unknown`, which the
//! release verification rejects; a value that is set but malformed fails the
//! build here instead of producing a mislabeled helper.

use std::env;

/// Helper protocol this build speaks. `main.rs` asserts it equals
/// `hane_update::model::UPDATER_PROTOCOL`.
const UPDATER_PROTOCOL: u64 = 1;

fn main() {
    println!("cargo:rerun-if-env-changed=HANE_SOURCE_SHA");
    println!("cargo:rerun-if-changed=build.rs");

    let source = match env::var("HANE_SOURCE_SHA") {
        Ok(value) if !value.is_empty() => {
            let valid = value.len() == 40
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !valid {
                panic!("HANE_SOURCE_SHA must be 40 lowercase hex digits");
            }
            value
        }
        _ => "unknown".to_string(),
    };
    let target = env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=HANE_UPDATER_SOURCE_SHA={source}");
    println!("cargo:rustc-env=HANE_UPDATER_TARGET={target}");
    println!("cargo:rustc-env=HANE_UPDATER_PROTOCOL={UPDATER_PROTOCOL}");
}

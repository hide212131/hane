#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! `hane-updater`: the update helper. It accepts no arguments and no paths;
//! see `hane_update::helper` for the session it runs.

use std::hint::black_box;

/// Identity record that `scripts/release_catalog.py` reads back from the built
/// executable. It is consulted by [`identity_intact`] on every start, so the
/// bytes stay in the final binary because they are used, not because of a
/// linker attribute.
const IDENTITY: &str = concat!(
    "hane-updater-identity-v1;version=",
    env!("CARGO_PKG_VERSION"),
    ";target=",
    env!("HANE_UPDATER_TARGET"),
    ";source=",
    env!("HANE_UPDATER_SOURCE_SHA"),
    ";protocol=",
    env!("HANE_UPDATER_PROTOCOL"),
    ";end"
);

// The record and the library must describe the same protocol.
const _: () = assert!(
    hane_update::model::UPDATER_PROTOCOL == parse_protocol(env!("HANE_UPDATER_PROTOCOL")),
    "build script protocol differs from hane_update::model::UPDATER_PROTOCOL"
);

const fn parse_protocol(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let mut value = 0u64;
    let mut i = 0;
    while i < bytes.len() {
        value = value * 10 + (bytes[i] - b'0') as u64;
        i += 1;
    }
    value
}

const fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

const IDENTITY_HASH: u64 = fnv1a(IDENTITY.as_bytes());

/// Re-reads the embedded record from memory and checks it against its
/// build-time hash. `black_box` hides the source from the optimizer and its
/// result is the data being hashed, so the record cannot be dropped.
fn identity_intact() -> bool {
    let record = black_box(IDENTITY.as_bytes());
    fnv1a(record) == IDENTITY_HASH && record.starts_with(b"hane-updater-identity-v1;")
}

fn main() {
    if !identity_intact() {
        std::process::exit(70);
    }
    std::process::exit(hane_update::helper::helper_main());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_record_names_version_target_source_and_protocol() {
        assert!(identity_intact());
        assert!(IDENTITY.contains(concat!(";version=", env!("CARGO_PKG_VERSION"), ";")));
        assert!(IDENTITY.contains(";target="));
        assert!(IDENTITY.contains(";source="));
        assert!(IDENTITY.ends_with(&format!(
            ";protocol={};end",
            hane_update::model::UPDATER_PROTOCOL
        )));
    }

    #[test]
    fn protocol_parser_reads_decimal() {
        assert_eq!(parse_protocol("1"), 1);
        assert_eq!(parse_protocol("12"), 12);
    }
}

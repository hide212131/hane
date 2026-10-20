//! Unit tests for the U0 core. They use only the test key and in-memory or
//! temporary-directory fakes; nothing here touches the network or an install.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Cursor};
use std::path::Path;
use std::sync::Arc;

use semver::Version;

use crate::archive::{
    ArchiveLimits, InspectedArchive, check_layout, scan_archive, stage_archive,
};
use crate::catalog::{VerifiedCandidate, VerifyContext, confirm_selection, record_seen, verify_catalog};
use crate::download::{
    ArchiveDownload, BodyReader, CancelToken, Fetcher, PartStore, Response, TransportError,
    TransportFailure, asset_url, latest_release_url,
};
use crate::error::{ErrorCode, Result};
use crate::fsutil::{DirPartStore, FileSeenStore};
use crate::identity::{ApplyPlan, FileIdentity, InstallFacts, InstallId, SignedInventory, TransactionId};
use crate::journal::{
    DiskView, DurableJournal, FileJournalStore, FsDiskView, JournalStore, Observed, Phase,
    RecoveryDecision, RollbackAction, StepState, UpdateJournal, check_plan_preconditions,
    decide_recovery,
};
use crate::limits::{CONNECT_TIMEOUT_MS, DOWNLOAD_TOTAL_MS, METADATA_TOTAL_MS, READ_STALL_MS};
use crate::model::{
    CATALOG_ASSET_NAME, CATALOG_DOMAIN, INVENTORY_DOMAIN, INVENTORY_FILE_NAME,
    INVENTORY_SIGNATURE_FILE_NAME, SIGNATURE_ASSET_NAME, Target,
};
use crate::release::select_release;
use crate::testing::{
    Fault, FaultyJournalStore, FixedClock, MemoryDisk, MemoryPartStore, MemorySeenStore,
    Scripted, ScriptedTransport, SlowBody, TempDir, WindowsRelease, ZipEntry, build_zip, catalog_doc,
    default_payload, macos_entry, release_json, sign, sign_catalog, sign_with, test_now,
    test_policy, windows_release, windows_release_with,
};
use crate::trust::{SeenCatalog, SeenStore, TrustPolicy};
use crate::util::{parse_rfc3339_utc, sha256_hex};
use crate::version::{
    VersionRelation, parse_current_version, parse_release_tag, parse_stable_version, relation,
};

const CURRENT: &str = "0.26.0";
const NEW_VERSION: &str = "0.27.0";

fn code<T>(result: Result<T>) -> ErrorCode {
    result.err().expect("expected an error").code
}

fn verify_with(
    release_catalog: &[u8],
    release_sig: &[u8],
    policy: &TrustPolicy,
    clock: &FixedClock,
    target: Target,
    current: &str,
    seen: Option<&SeenCatalog>,
) -> Result<VerifiedCandidate> {
    verify_catalog(
        release_catalog,
        release_sig,
        &VerifyContext {
            policy,
            clock,
            current_version: current,
            target,
            seen,
            expected_tag: None,
        },
    )
}

fn verify_windows(release: &WindowsRelease) -> Result<VerifiedCandidate> {
    verify_with(
        &release.catalog,
        &release.catalog_sig,
        &test_policy(),
        &FixedClock::new(test_now()),
        Target::WindowsX64,
        CURRENT,
        None,
    )
}

fn candidate(release: &WindowsRelease) -> VerifiedCandidate {
    verify_windows(release).expect("release verifies")
}

// ------------------------------------------------------------ U-01 versions

#[test]
fn semver_order_is_not_lexicographic() {
    let current = parse_current_version("0.9.0").unwrap();
    let candidate = parse_stable_version("0.10.0").unwrap();
    assert_eq!(relation(&current, &candidate), VersionRelation::Newer);
    assert_eq!(relation(&candidate, &current), VersionRelation::Older);
    assert_eq!(relation(&current, &current), VersionRelation::Same);
}

#[test]
fn only_plain_stable_versions_are_accepted() {
    assert_eq!(code(parse_stable_version("1.2.3-rc.1")), ErrorCode::NotStable);
    assert_eq!(code(parse_stable_version("1.2.3+build.5")), ErrorCode::NotStable);
    assert_eq!(code(parse_stable_version("1.2")), ErrorCode::NotStable);
    assert_eq!(code(parse_release_tag("1.2.3")), ErrorCode::NotStable);
    assert_eq!(code(parse_release_tag("v1.2")), ErrorCode::NotStable);
    assert!(parse_release_tag("v1.2.3").is_ok());
    assert_eq!(
        code(parse_current_version("0.26.0-dev")),
        ErrorCode::UnsupportedInstall
    );
    assert_eq!(
        code(parse_current_version("0.26.0+local")),
        ErrorCode::UnsupportedInstall
    );
}

fn full_assets() -> Vec<&'static str> {
    vec![
        CATALOG_ASSET_NAME,
        SIGNATURE_ASSET_NAME,
        "hane-windows-x64.zip",
        "hane-macos-arm64.zip",
    ]
}

#[test]
fn release_selection_reports_the_version_relation() {
    let json = release_json("v0.27.0", &full_assets(), false, false);
    let pick = |current: &str| {
        select_release(&json, &Version::parse(current).unwrap(), Target::WindowsX64).unwrap()
    };
    let newer = pick("0.26.0");
    assert_eq!(newer.relation, VersionRelation::Newer);
    assert_eq!(newer.tag, "v0.27.0");
    assert_eq!(newer.archive_url, asset_url("v0.27.0", "hane-windows-x64.zip"));
    assert_eq!(pick("0.27.0").relation, VersionRelation::Same);
    assert_eq!(pick("0.28.0").relation, VersionRelation::Older);
    // The other CPU's asset is never substituted.
    let mac = select_release(&json, &Version::new(0, 26, 0), Target::MacosArm64).unwrap();
    assert_eq!(mac.archive_name, "hane-macos-arm64.zip");
}

#[test]
fn release_selection_rejects_unsuitable_releases() {
    let current = Version::new(0, 26, 0);
    let pick = |json: Vec<u8>| select_release(&json, &current, Target::WindowsX64);

    assert_eq!(
        code(pick(release_json("v0.27.0", &full_assets(), false, true))),
        ErrorCode::NotStable
    );
    assert_eq!(
        code(pick(release_json("v0.27.0", &full_assets(), true, false))),
        ErrorCode::NotStable
    );
    assert_eq!(
        code(pick(release_json("v0.27.0-rc.1", &full_assets(), false, false))),
        ErrorCode::NotStable
    );
    assert_eq!(
        code(pick(release_json("0.27.0", &full_assets(), false, false))),
        ErrorCode::NotStable
    );

    let mut duplicated = full_assets();
    duplicated.push("hane-windows-x64.zip");
    assert_eq!(
        code(pick(release_json("v0.27.0", &duplicated, false, false))),
        ErrorCode::DuplicateAsset
    );

    let no_catalog = ["hane-windows-x64.zip", SIGNATURE_ASSET_NAME];
    assert_eq!(
        code(pick(release_json("v0.27.0", &no_catalog, false, false))),
        ErrorCode::MetadataMissing
    );
    let no_archive = [CATALOG_ASSET_NAME, SIGNATURE_ASSET_NAME, "hane-macos-arm64.zip"];
    assert_eq!(
        code(pick(release_json("v0.27.0", &no_archive, false, false))),
        ErrorCode::AssetMissing
    );
    assert_eq!(code(pick(b"not json".to_vec())), ErrorCode::MalformedMetadata);
}

#[test]
fn release_selection_rejects_assets_outside_the_pinned_release() {
    let json = serde_json::to_vec(&serde_json::json!({
        "id": 1, "tag_name": "v0.27.0", "draft": false, "prerelease": false,
        "assets": [
            {"name": CATALOG_ASSET_NAME, "size": 1, "state": "uploaded",
             "browser_download_url": asset_url("v0.27.0", CATALOG_ASSET_NAME)},
            {"name": SIGNATURE_ASSET_NAME, "size": 1, "state": "uploaded",
             "browser_download_url": asset_url("v0.27.0", SIGNATURE_ASSET_NAME)},
            {"name": "hane-windows-x64.zip", "size": 1, "state": "uploaded",
             "browser_download_url": "https://github.com/other/repo/releases/download/v0.27.0/hane-windows-x64.zip"},
        ]
    }))
    .unwrap();
    assert_eq!(
        code(select_release(&json, &Version::new(0, 26, 0), Target::WindowsX64)),
        ErrorCode::AssetMismatch
    );
    assert_eq!(
        code(Target::parse("x86_64-apple-darwin")),
        ErrorCode::TargetUnsupported
    );
    assert_eq!(Target::parse("aarch64-apple-darwin").unwrap(), Target::MacosArm64);
}

#[test]
fn http_statuses_are_not_reported_as_up_to_date() {
    let clock = FixedClock::new(test_now());
    let check = |transport: &ScriptedTransport| {
        Fetcher {
            transport,
            clock: &clock,
        }
        .check_latest(&Version::new(0, 26, 0), Target::WindowsX64, &CancelToken::new())
    };
    let cases = [
        (404, ErrorCode::NotFound),
        (403, ErrorCode::RateLimited),
        (429, ErrorCode::RateLimited),
        (503, ErrorCode::NetworkUnavailable),
    ];
    for (status, expected) in cases {
        let transport = ScriptedTransport::new();
        transport.push(&latest_release_url(), Scripted::status(status));
        assert_eq!(code(check(&transport)), expected, "HTTP {status}");
    }
    let transport = ScriptedTransport::new();
    transport.push(&latest_release_url(), Scripted::fail(TransportFailure::Timeout));
    assert_eq!(code(check(&transport)), ErrorCode::Timeout);

    let transport = ScriptedTransport::new();
    transport.push(
        &latest_release_url(),
        Scripted::body(200, release_json("v0.27.0", &full_assets(), false, false)),
    );
    let selection = check(&transport).unwrap();
    assert_eq!(selection.version, Version::new(0, 27, 0));
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, latest_release_url());
    assert!(requests[0].user_agent.starts_with("hane-update/"));
    assert!(requests[0].api_version.is_some());
}

#[test]
fn missing_metadata_asset_is_metadata_missing() {
    let clock = FixedClock::new(test_now());
    let selection = select_release(
        &release_json("v0.27.0", &full_assets(), false, false),
        &Version::new(0, 26, 0),
        Target::WindowsX64,
    )
    .unwrap();
    let transport = ScriptedTransport::new();
    transport.push(&selection.catalog_url, Scripted::status(404));
    let fetcher = Fetcher {
        transport: &transport,
        clock: &clock,
    };
    assert_eq!(
        code(fetcher.fetch_catalog(&selection, &CancelToken::new())),
        ErrorCode::MetadataMissing
    );
}

// ------------------------------------------------------------ U-02 catalog

#[test]
fn rfc3339_parser_is_strict() {
    assert_eq!(parse_rfc3339_utc("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_rfc3339_utc("2000-03-01T00:00:00Z"), Some(951_868_800));
    assert_eq!(parse_rfc3339_utc("2024-02-29T23:59:59Z"), Some(1_709_251_199));
    assert_eq!(parse_rfc3339_utc("2023-02-29T00:00:00Z"), None);
    assert_eq!(parse_rfc3339_utc("2026-10-17T00:00:00+09:00"), None);
    assert_eq!(parse_rfc3339_utc("2026-10-17 00:00:00Z"), None);
    assert_eq!(parse_rfc3339_utc("2026-13-01T00:00:00Z"), None);
    assert_eq!(parse_rfc3339_utc("２０２６-10-17T00:00:00Z"), None);
}

#[test]
fn a_correctly_signed_catalog_is_verified() {
    let release = windows_release(NEW_VERSION, 7, default_payload(NEW_VERSION));
    let candidate = candidate(&release);
    assert_eq!(candidate.relation(), VersionRelation::Newer);
    assert!(candidate.is_applicable());
    assert_eq!(candidate.sequence(), 7);
    assert_eq!(candidate.catalog_sha256(), sha256_hex(&release.catalog));
    assert_eq!(candidate.candidate_id().as_str().len(), 64);
}

#[test]
fn same_or_older_versions_are_never_applicable() {
    for version in ["0.26.0", "0.25.0"] {
        let release = windows_release(version, 7, default_payload(version));
        let candidate = candidate(&release);
        assert!(!candidate.is_applicable(), "{version}");
    }
}

#[test]
fn production_trust_policy_makes_nothing_applicable() {
    let release = windows_release(NEW_VERSION, 7, default_payload(NEW_VERSION));
    let policy = TrustPolicy::production();
    assert!(!policy.is_configured());
    let result = verify_with(
        &release.catalog,
        &release.catalog_sig,
        &policy,
        &FixedClock::new(test_now()),
        Target::WindowsX64,
        CURRENT,
        None,
    );
    assert_eq!(code(result), ErrorCode::TrustUnconfigured);
}

#[test]
fn tampering_and_wrong_signatures_are_rejected() {
    let release = windows_release(NEW_VERSION, 7, default_payload(NEW_VERSION));
    let mut tampered = release.catalog.clone();
    let last = tampered.len() - 2;
    tampered[last] ^= 0x01;
    let verify = |catalog: &[u8], sig: &[u8]| {
        verify_with(
            catalog,
            sig,
            &test_policy(),
            &FixedClock::new(test_now()),
            Target::WindowsX64,
            CURRENT,
            None,
        )
    };
    assert_eq!(code(verify(&tampered, &release.catalog_sig)), ErrorCode::SignatureInvalid);
    // Signed under another domain.
    let wrong_domain = sign(INVENTORY_DOMAIN, &release.catalog);
    assert_eq!(code(verify(&release.catalog, &wrong_domain)), ErrorCode::SignatureInvalid);
    // Signed without any domain prefix.
    let no_domain = sign(b"", &release.catalog);
    assert_eq!(code(verify(&release.catalog, &no_domain)), ErrorCode::SignatureInvalid);
    // Unknown key id, and a trusted id signed by a different key.
    let unknown = sign_with(1, "other-key", CATALOG_DOMAIN, &release.catalog);
    assert_eq!(code(verify(&release.catalog, &unknown)), ErrorCode::SignatureInvalid);
    let wrong_key = sign_with(2, crate::testing::TEST_KEY_ID, CATALOG_DOMAIN, &release.catalog);
    assert_eq!(code(verify(&release.catalog, &wrong_key)), ErrorCode::SignatureInvalid);
    assert_eq!(code(verify(&release.catalog, b"{}")), ErrorCode::SchemaUnsupported);
    assert_eq!(code(verify(&release.catalog, b"garbage")), ErrorCode::MalformedMetadata);
}

/// Re-signs hand-edited catalog bytes so the content, not the signature, is
/// what the verifier has to reject.
fn verify_signed_bytes(bytes: &[u8]) -> Result<VerifiedCandidate> {
    let signature = sign(CATALOG_DOMAIN, bytes);
    verify_with(
        bytes,
        &signature,
        &test_policy(),
        &FixedClock::new(test_now()),
        Target::WindowsX64,
        CURRENT,
        None,
    )
}

fn mutated(release: &WindowsRelease, edit: impl FnOnce(&mut crate::model::CatalogDocument)) -> Vec<u8> {
    let mut doc = release.doc.clone();
    edit(&mut doc);
    sign_catalog(&doc).0
}

/// Inserts `field` (with its trailing comma) right after the opening brace.
fn with_leading_field(json: &[u8], field: &str) -> Vec<u8> {
    let mut out = vec![b'{'];
    out.extend_from_slice(field.as_bytes());
    out.extend_from_slice(&json[1..]);
    out
}

#[test]
fn duplicate_keys_unknown_fields_and_schemas_are_rejected() {
    let release = windows_release(NEW_VERSION, 7, default_payload(NEW_VERSION));

    let duplicate = with_leading_field(&release.catalog, r#""schema":1,"#);
    assert_eq!(code(verify_signed_bytes(&duplicate)), ErrorCode::MalformedMetadata);

    let extra = with_leading_field(&release.catalog, r#""surprise":true,"#);
    assert_eq!(code(verify_signed_bytes(&extra)), ErrorCode::MalformedMetadata);

    let schema2 = mutated(&release, |d| d.schema = 2);
    assert_eq!(code(verify_signed_bytes(&schema2)), ErrorCode::SchemaUnsupported);

    let channel = mutated(&release, |d| d.channel = "beta".into());
    assert_eq!(code(verify_signed_bytes(&channel)), ErrorCode::MalformedMetadata);
    let repo = mutated(&release, |d| d.repository = "someone/else".into());
    assert_eq!(code(verify_signed_bytes(&repo)), ErrorCode::MalformedMetadata);
    let tag = mutated(&release, |d| d.tag = "v9.9.9".into());
    assert_eq!(code(verify_signed_bytes(&tag)), ErrorCode::MalformedMetadata);
    let sha = mutated(&release, |d| d.source_sha = "abc".into());
    assert_eq!(code(verify_signed_bytes(&sha)), ErrorCode::MalformedMetadata);
    let version = mutated(&release, |d| {
        d.version = "0.27.0-rc.1".into();
        d.tag = "v0.27.0-rc.1".into();
    });
    assert_eq!(code(verify_signed_bytes(&version)), ErrorCode::NotStable);
}

#[test]
fn time_window_is_enforced() {
    let release = windows_release(NEW_VERSION, 7, default_payload(NEW_VERSION));
    let expired = FixedClock::new(parse_rfc3339_utc("2027-01-02T00:00:00Z").unwrap());
    let result = verify_with(
        &release.catalog,
        &release.catalog_sig,
        &test_policy(),
        &expired,
        Target::WindowsX64,
        CURRENT,
        None,
    );
    assert_eq!(code(result), ErrorCode::CatalogExpired);

    let future = mutated(&release, |d| {
        d.published_at = "2026-10-20T00:00:00Z".into();
        d.expires_at = "2026-12-01T00:00:00Z".into();
    });
    assert_eq!(code(verify_signed_bytes(&future)), ErrorCode::ClockInvalid);

    let long_lived = mutated(&release, |d| d.expires_at = "2027-12-01T00:00:00Z".into());
    assert_eq!(code(verify_signed_bytes(&long_lived)), ErrorCode::MalformedMetadata);
    let reversed = mutated(&release, |d| d.expires_at = d.published_at.clone());
    assert_eq!(code(verify_signed_bytes(&reversed)), ErrorCode::MalformedMetadata);
    let bad_format = mutated(&release, |d| d.expires_at = "2027-01-01".into());
    assert_eq!(code(verify_signed_bytes(&bad_format)), ErrorCode::MalformedMetadata);
}

#[test]
fn protocol_state_and_target_constraints_are_enforced() {
    let release = windows_release(NEW_VERSION, 7, default_payload(NEW_VERSION));
    let needs_v2 = mutated(&release, |d| {
        d.updater_protocol = 2;
        d.min_updater_protocol = 2;
    });
    assert_eq!(code(verify_signed_bytes(&needs_v2)), ErrorCode::ProtocolUnsupported);
    let no_v1 = mutated(&release, |d| d.updater_protocol = 0);
    assert_eq!(code(verify_signed_bytes(&no_v1)), ErrorCode::ProtocolUnsupported);
    let state = mutated(&release, |d| d.state_compatibility = "breaking".into());
    assert_eq!(code(verify_signed_bytes(&state)), ErrorCode::StateIncompatible);
    let no_target = mutated(&release, |d| d.targets.clear());
    assert_eq!(code(verify_signed_bytes(&no_target)), ErrorCode::TargetUnsupported);
    let layout = mutated(&release, |d| {
        d.targets.values_mut().for_each(|t| t.layout_revision = 1);
    });
    assert_eq!(code(verify_signed_bytes(&layout)), ErrorCode::ProtocolUnsupported);
    let name = mutated(&release, |d| {
        d.targets.values_mut().for_each(|t| t.asset_name = "hane-other.zip".into());
    });
    assert_eq!(code(verify_signed_bytes(&name)), ErrorCode::AssetMismatch);
    let no_inventory = mutated(&release, |d| {
        d.targets.values_mut().for_each(|t| t.inventory_sha256 = None);
    });
    assert_eq!(code(verify_signed_bytes(&no_inventory)), ErrorCode::MalformedMetadata);
    let huge = mutated(&release, |d| {
        d.targets.values_mut().for_each(|t| t.archive_size = u64::MAX);
    });
    assert_eq!(code(verify_signed_bytes(&huge)), ErrorCode::LimitExceeded);

    // A development build is not eligible to update itself.
    let result = verify_with(
        &release.catalog,
        &release.catalog_sig,
        &test_policy(),
        &FixedClock::new(test_now()),
        Target::WindowsX64,
        "0.26.0-dev",
        None,
    );
    assert_eq!(code(result), ErrorCode::UnsupportedInstall);
    // The macOS CPU has no entry in this catalog.
    let result = verify_with(
        &release.catalog,
        &release.catalog_sig,
        &test_policy(),
        &FixedClock::new(test_now()),
        Target::MacosArm64,
        CURRENT,
        None,
    );
    assert_eq!(code(result), ErrorCode::TargetUnsupported);
}

#[test]
fn sequence_regression_and_same_sequence_with_other_bytes_are_rejected() {
    let release = windows_release(NEW_VERSION, 10, default_payload(NEW_VERSION));
    let verify = |seen: &SeenCatalog| {
        verify_with(
            &release.catalog,
            &release.catalog_sig,
            &test_policy(),
            &FixedClock::new(test_now()),
            Target::WindowsX64,
            CURRENT,
            Some(seen),
        )
    };
    let higher = SeenCatalog::new(11, sha256_hex(b"other"));
    assert_eq!(code(verify(&higher)), ErrorCode::SequenceRegression);
    let same_other_bytes = SeenCatalog::new(10, sha256_hex(b"other"));
    assert_eq!(code(verify(&same_other_bytes)), ErrorCode::SequenceConflict);
    let same_bytes = SeenCatalog::new(10, sha256_hex(&release.catalog));
    assert!(verify(&same_bytes).is_ok());
    let lower = SeenCatalog::new(9, sha256_hex(b"other"));
    assert!(verify(&lower).is_ok());
    let broken = SeenCatalog::new(1, "not-a-hash".into());
    assert_eq!(code(verify(&broken)), ErrorCode::ReceiptUnwritable);
}

#[test]
fn candidate_is_not_ready_unless_the_sequence_floor_is_stored() {
    let release = windows_release(NEW_VERSION, 10, default_payload(NEW_VERSION));
    let candidate = candidate(&release);

    let mut store = MemorySeenStore::default();
    record_seen(&mut store, &candidate).unwrap();
    assert_eq!(store.current().unwrap().sequence, 10);
    // Repeating the same catalog is fine; a lower one never lowers the floor.
    record_seen(&mut store, &candidate).unwrap();

    let failing = MemorySeenStore::default();
    failing.fail_stores();
    let mut failing_store = failing.clone();
    assert_eq!(
        code(record_seen(&mut failing_store, &candidate)),
        ErrorCode::ReceiptUnwritable
    );
    assert!(failing.current().is_none());

    let mut ahead = MemorySeenStore::with(SeenCatalog::new(11, sha256_hex(b"newer")));
    assert_eq!(
        code(record_seen(&mut ahead, &candidate)),
        ErrorCode::SequenceRegression
    );
    let mut conflicting = MemorySeenStore::with(SeenCatalog::new(10, sha256_hex(b"different")));
    assert_eq!(
        code(record_seen(&mut conflicting, &candidate)),
        ErrorCode::SequenceConflict
    );
}

#[test]
fn concurrent_sequence_updates_never_lower_the_stored_floor() {
    let dir = TempDir::new("seen-race");
    let path = dir.path().join("receipts").join("seen-catalog.json");
    let old = candidate(&windows_release(NEW_VERSION, 10, default_payload(NEW_VERSION)));
    let new = candidate(&windows_release(NEW_VERSION, 11, default_payload(NEW_VERSION)));

    // Process A holds the placement lock while it stores sequence 11; process B
    // (a separate store on the same path) tries 10 and must wait for it.
    let holder = FileSeenStore::new(path.clone());
    let guard = holder.lock().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let old = &old;
        let other_path = path.clone();
        scope.spawn(move || {
            let mut other = FileSeenStore::new(other_path);
            tx.send(record_seen(&mut other, old)).unwrap();
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
            "the stale writer must wait for the lock"
        );
        let mut writer = FileSeenStore::new(path.clone());
        writer
            .store(&SeenCatalog::new(11, new.catalog_sha256().to_owned()))
            .unwrap();
        drop(guard);
        let result = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert_eq!(code(result), ErrorCode::SequenceRegression);
    });
    let stored = FileSeenStore::new(path).load().unwrap().unwrap();
    assert_eq!(stored.sequence, 11);
}

#[test]
fn sequence_locks_of_different_installs_do_not_block_each_other() {
    let dir = TempDir::new("seen-installs");
    let first = FileSeenStore::new(dir.path().join("a").join("seen-catalog.json"));
    let _held = first.lock().unwrap();
    let mut second = FileSeenStore::new(dir.path().join("b").join("seen-catalog.json"));
    let release = windows_release(NEW_VERSION, 10, default_payload(NEW_VERSION));
    record_seen(&mut second, &candidate(&release)).unwrap();
}

#[test]
fn file_seen_store_round_trips_and_rejects_corruption() {
    let dir = TempDir::new("seen");
    let path = dir.path().join("seen-catalog.json");
    let mut store = FileSeenStore::new(path.clone());
    assert!(store.load().unwrap().is_none());
    let seen = SeenCatalog::new(5, sha256_hex(b"x"));
    store.store(&seen).unwrap();
    assert_eq!(store.load().unwrap(), Some(seen));
    fs::write(path.as_path(), b"{\"schema\":1}").unwrap();
    assert!(store.load().is_err());
}

#[test]
fn macos_candidates_need_the_update_key_and_bundle_identity_but_no_apple_identity() {
    let zip = build_zip(&[ZipEntry::file("hane-macos-arm64/README.md", b"x")]);
    let verify = |entry: crate::model::TargetEntry, policy: &TrustPolicy| {
        let doc = catalog_doc(NEW_VERSION, 3, Target::MacosArm64, entry);
        let (bytes, sig) = sign_catalog(&doc);
        verify_with(
            &bytes,
            &sig,
            policy,
            &FixedClock::new(test_now()),
            Target::MacosArm64,
            CURRENT,
            None,
        )
    };
    let good = macos_entry(NEW_VERSION, &zip, 1, b"inventory");
    // No Apple Developer ID, Team ID or notarization is needed.
    assert!(verify(good.clone(), &test_policy()).unwrap().is_applicable());
    assert_eq!(
        code(verify(good.clone(), &TrustPolicy::production())),
        ErrorCode::TrustUnconfigured
    );

    let mut with_team = good.clone();
    with_team.team_id = Some("EVILTEAM99".into());
    assert_eq!(code(verify(with_team, &test_policy())), ErrorCode::MalformedMetadata);
    let mut other_bundle = good.clone();
    other_bundle.bundle_id = Some("com.example.fake".into());
    assert_eq!(code(verify(other_bundle, &test_policy())), ErrorCode::IdentityMismatch);
    let mut other_version = good.clone();
    other_version.bundle_version = Some("9.9.9".into());
    assert_eq!(code(verify(other_version, &test_policy())), ErrorCode::IdentityMismatch);
    let mut no_inventory = good;
    no_inventory.inventory_sha256 = None;
    assert_eq!(code(verify(no_inventory, &test_policy())), ErrorCode::MalformedMetadata);
}

#[test]
fn listing_and_catalog_must_describe_the_same_archive() {
    let release = windows_release(NEW_VERSION, 7, default_payload(NEW_VERSION));
    let candidate = candidate(&release);
    let json = release_json("v0.27.0", &full_assets(), false, false);
    let mut selection = select_release(&json, &Version::new(0, 26, 0), Target::WindowsX64).unwrap();
    // release_json lists every asset with a placeholder size.
    assert_eq!(code(confirm_selection(&selection, &candidate)), ErrorCode::AssetMismatch);
    selection.archive_size = candidate.target_entry().archive_size;
    assert!(confirm_selection(&selection, &candidate).is_ok());
    selection.tag = "v0.28.0".into();
    assert_eq!(code(confirm_selection(&selection, &candidate)), ErrorCode::AssetMismatch);
}

// ------------------------------------------------------- U-03 download

const ARCHIVE_NAME: &str = "hane-windows-x64.zip";
const TAG: &str = "v0.27.0";

fn payload_bytes() -> Vec<u8> {
    (0..5000u32).map(|i| (i % 251) as u8).collect()
}

fn download_request<'a>(data: &[u8], hash: &'a str) -> ArchiveDownload<'a> {
    ArchiveDownload {
        tag: TAG,
        name: ARCHIVE_NAME,
        size: data.len() as u64,
        sha256: hash,
    }
}

fn run_download(
    transport: &ScriptedTransport,
    clock: &FixedClock,
    store: &mut MemoryPartStore,
    cancel: &CancelToken,
) -> Result<crate::download::DownloadedArchive> {
    let data = payload_bytes();
    let hash = sha256_hex(&data);
    Fetcher { transport, clock }.download_archive(&download_request(&data, &hash), store, cancel)
}

#[test]
fn download_commits_only_a_verified_complete_body() {
    let data = payload_bytes();
    let transport = ScriptedTransport::new();
    transport.push(&asset_url(TAG, ARCHIVE_NAME), Scripted::body(200, data.clone()));
    let mut store = MemoryPartStore::default();
    let done = run_download(&transport, &FixedClock::new(0), &mut store, &CancelToken::new()).unwrap();
    assert_eq!(done.size, data.len() as u64);
    assert_eq!(done.sha256, sha256_hex(&data));
    assert_eq!(store.committed().unwrap(), data);
}

#[test]
fn download_follows_only_allowed_release_hosts() {
    let entry = asset_url(TAG, ARCHIVE_NAME);
    let data = payload_bytes();

    // Allowed: github.com entry -> release-assets host (with a signed query).
    let target = "https://release-assets.githubusercontent.com/blob/1?sig=abc";
    let transport = ScriptedTransport::new();
    transport.push(&entry, Scripted::redirect(302, target));
    transport.push(target, Scripted::body(200, data.clone()));
    let mut store = MemoryPartStore::default();
    run_download(&transport, &FixedClock::new(0), &mut store, &CancelToken::new()).unwrap();
    let urls: Vec<String> = transport.requests().into_iter().map(|r| r.url).collect();
    assert_eq!(urls, vec![entry.clone(), target.to_string()]);
    assert!(transport.requests()[1].api_version.is_none());

    let rejected = [
        "https://evil.example.com/file.zip",
        "http://objects.githubusercontent.com/file.zip",
        "https://raw.githubusercontent.com/file.zip",
        "https://objects.githubusercontent.com.evil.example/file.zip",
        "https://user:pw@objects.githubusercontent.com/file.zip",
        "https://objects.githubusercontent.com:8443/file.zip",
        "https://github.com/other/repo/releases/download/v0.27.0/hane-windows-x64.zip",
        "ftp://objects.githubusercontent.com/file.zip",
    ];
    for location in rejected {
        let transport = ScriptedTransport::new();
        transport.push(&entry, Scripted::redirect(302, location));
        transport.push(location, Scripted::body(200, data.clone()));
        let mut store = MemoryPartStore::default();
        let result = run_download(&transport, &FixedClock::new(0), &mut store, &CancelToken::new());
        assert_eq!(code(result), ErrorCode::RedirectRejected, "{location}");
        assert_eq!(transport.requests().len(), 1, "{location} must not be requested");
        assert!(store.committed().is_none());
        assert!(store.was_discarded());
    }

    // More than five hops.
    let transport = ScriptedTransport::new();
    transport.push(&entry, Scripted::redirect(302, "https://objects.githubusercontent.com/0"));
    for hop in 0..6 {
        transport.push(
            &format!("https://objects.githubusercontent.com/{hop}"),
            Scripted::redirect(302, &format!("https://objects.githubusercontent.com/{}", hop + 1)),
        );
    }
    let mut store = MemoryPartStore::default();
    let result = run_download(&transport, &FixedClock::new(0), &mut store, &CancelToken::new());
    assert_eq!(code(result), ErrorCode::RedirectRejected);

    // A redirect without Location.
    let transport = ScriptedTransport::new();
    transport.push(&entry, Scripted::status(302));
    let mut store = MemoryPartStore::default();
    let result = run_download(&transport, &FixedClock::new(0), &mut store, &CancelToken::new());
    assert_eq!(code(result), ErrorCode::RedirectRejected);
}

#[test]
fn download_failures_are_distinct_and_leave_no_candidate() {
    let entry = asset_url(TAG, ARCHIVE_NAME);
    let data = payload_bytes();
    let declared = data.len() as u64;

    struct Case {
        name: &'static str,
        response: Scripted,
        expected: ErrorCode,
    }
    let cases = vec![
        Case {
            name: "declared length over the cap",
            response: Scripted::body(200, vec![0; data.len() + 1000]),
            expected: ErrorCode::LimitExceeded,
        },
        Case {
            name: "stream over the cap without length",
            response: Scripted::body_without_length(200, vec![0; data.len() + 1000]),
            expected: ErrorCode::LimitExceeded,
        },
        Case {
            name: "short body without length",
            response: Scripted::body_without_length(200, data[..4000].to_vec()),
            expected: ErrorCode::SizeMismatch,
        },
        Case {
            name: "clean EOF before declared length",
            response: Scripted::short(data[..100].to_vec(), declared),
            expected: ErrorCode::Truncated,
        },
        Case {
            name: "connection reset mid-stream",
            response: Scripted::broken(data[..100].to_vec(), declared, io::ErrorKind::ConnectionReset),
            expected: ErrorCode::Truncated,
        },
        Case {
            name: "read stall",
            response: Scripted::broken(data[..100].to_vec(), declared, io::ErrorKind::TimedOut),
            expected: ErrorCode::Timeout,
        },
        Case {
            name: "same length, other content",
            response: Scripted::body(200, vec![9; data.len()]),
            expected: ErrorCode::HashMismatch,
        },
        Case {
            name: "not found",
            response: Scripted::status(404),
            expected: ErrorCode::NotFound,
        },
        Case {
            name: "rate limited",
            response: Scripted::status(429),
            expected: ErrorCode::RateLimited,
        },
        Case {
            name: "tls failure",
            response: Scripted::fail(TransportFailure::Tls),
            expected: ErrorCode::NetworkUnavailable,
        },
    ];
    for case in cases {
        let transport = ScriptedTransport::new();
        transport.push(&entry, case.response);
        let mut store = MemoryPartStore::default();
        let result = run_download(&transport, &FixedClock::new(0), &mut store, &CancelToken::new());
        assert_eq!(code(result), case.expected, "{}", case.name);
        assert!(store.committed().is_none(), "{}", case.name);
        assert!(store.was_discarded(), "{}", case.name);
    }
}

#[test]
fn download_reports_a_full_disk() {
    let transport = ScriptedTransport::new();
    transport.push(&asset_url(TAG, ARCHIVE_NAME), Scripted::body(200, payload_bytes()));
    let mut store = MemoryPartStore::with_capacity(1000);
    let result = run_download(&transport, &FixedClock::new(0), &mut store, &CancelToken::new());
    assert_eq!(code(result), ErrorCode::InsufficientSpace);
    assert!(store.was_discarded());
    assert!(store.committed().is_none());
}

#[test]
fn download_total_deadline_is_enforced_by_the_clock() {
    let transport = ScriptedTransport::new();
    transport.push(&asset_url(TAG, ARCHIVE_NAME), Scripted::body(200, payload_bytes()));
    let clock = FixedClock::with_step(0, DOWNLOAD_TOTAL_MS + 1);
    let mut store = MemoryPartStore::default();
    let result = run_download(&transport, &clock, &mut store, &CancelToken::new());
    assert_eq!(code(result), ErrorCode::Timeout);
    assert!(store.was_discarded());
}

/// A scripted response that first spends `ms` of monotonic time.
fn after_ms(clock: &Arc<FixedClock>, ms: u64, response: Scripted) -> Scripted {
    let clock = Arc::clone(clock);
    Scripted::Custom(Box::new(move || {
        clock.advance(ms);
        match response {
            Scripted::Respond(r) => Ok(r),
            Scripted::Fail(e) => Err(e),
            Scripted::Custom(make) => make(),
            Scripted::Slow { .. } => unreachable!("after_ms wraps plain responses only"),
        }
    }))
}

#[test]
fn download_deadline_covers_redirect_and_open() {
    let entry = asset_url(TAG, ARCHIVE_NAME);
    let target = "https://objects.githubusercontent.com/blob/1";
    let clock = Arc::new(FixedClock::new(0));
    let transport = ScriptedTransport::new();
    // The first hop alone uses most of the budget; the second uses the rest.
    transport.push(
        &entry,
        after_ms(&clock, DOWNLOAD_TOTAL_MS - 1000, Scripted::redirect(302, target)),
    );
    transport.push(target, after_ms(&clock, 1000, Scripted::body(200, payload_bytes())));
    let mut store = MemoryPartStore::default();
    let result = run_download(&transport, &clock, &mut store, &CancelToken::new());
    assert_eq!(code(result), ErrorCode::Timeout);
    assert!(store.was_discarded());
    assert!(store.committed().is_none());

    // Once the budget is gone before a hop, that hop is never requested.
    let transport = ScriptedTransport::new();
    let clock = Arc::new(FixedClock::new(0));
    transport.push(
        &entry,
        after_ms(&clock, DOWNLOAD_TOTAL_MS, Scripted::redirect(302, target)),
    );
    transport.push(target, Scripted::body(200, payload_bytes()));
    let mut store = MemoryPartStore::default();
    let result = run_download(&transport, &clock, &mut store, &CancelToken::new());
    assert_eq!(code(result), ErrorCode::Timeout);
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn transport_timeouts_never_exceed_the_remaining_budget() {
    let entry = asset_url(TAG, ARCHIVE_NAME);
    let target = "https://objects.githubusercontent.com/blob/1";
    let clock = Arc::new(FixedClock::new(0));
    let transport = ScriptedTransport::new();
    transport.push(&entry, after_ms(&clock, 20, Scripted::redirect(302, target)));
    transport.push(target, Scripted::body(200, payload_bytes()));
    let mut store = MemoryPartStore::default();
    run_download(&transport, &clock, &mut store, &CancelToken::new()).unwrap();
    let requests = transport.requests();
    assert_eq!(requests[0].connect_timeout_ms, CONNECT_TIMEOUT_MS);
    assert_eq!(requests[0].read_timeout_ms, READ_STALL_MS);
    assert_eq!(requests[0].deadline_ms, DOWNLOAD_TOTAL_MS);

    // Near the end of the metadata budget the per-request timeouts shrink.
    let clock = Arc::new(FixedClock::new(0));
    let transport = ScriptedTransport::new();
    transport.push(
        &latest_release_url(),
        Scripted::body(200, release_json("v0.27.0", &full_assets(), false, false)),
    );
    let fetcher = Fetcher {
        transport: &transport,
        clock: &*clock,
    };
    let deadline = fetcher.metadata_deadline();
    clock.advance(METADATA_TOTAL_MS - 500);
    fetcher
        .check_latest_within(
            &Version::new(0, 26, 0),
            Target::WindowsX64,
            &deadline,
            &CancelToken::new(),
        )
        .unwrap();
    let request = &transport.requests()[0];
    assert_eq!(request.connect_timeout_ms, 500);
    assert_eq!(request.read_timeout_ms, 500);
}

#[test]
fn single_get_cannot_outlive_the_operation_deadline() {
    // The connect + head phase would take longer than the whole download
    // budget: the transport is cut off at the deadline and reports a timeout.
    let entry = asset_url(TAG, ARCHIVE_NAME);
    let clock = Arc::new(FixedClock::new(0));
    let transport = ScriptedTransport::new();
    transport.push(
        &entry,
        Scripted::slow(
            &clock,
            DOWNLOAD_TOTAL_MS + 1,
            Scripted::body(200, payload_bytes()),
        ),
    );
    let mut store = MemoryPartStore::default();
    let result = run_download(&transport, &clock, &mut store, &CancelToken::new());
    assert_eq!(code(result), ErrorCode::Timeout);
    assert_eq!(transport.requests().len(), 1);
    assert_eq!(transport.requests()[0].deadline_ms, DOWNLOAD_TOTAL_MS);
    assert!(store.was_discarded());
    assert!(store.committed().is_none());

    // A transport that ignores the deadline and returns late is still
    // rejected by the fetcher.
    let clock = Arc::new(FixedClock::new(0));
    let transport = ScriptedTransport::new();
    transport.push(
        &entry,
        after_ms(&clock, DOWNLOAD_TOTAL_MS, Scripted::body(200, payload_bytes())),
    );
    let mut store = MemoryPartStore::default();
    let result = run_download(&transport, &clock, &mut store, &CancelToken::new());
    assert_eq!(code(result), ErrorCode::Timeout);
    assert!(store.committed().is_none());
}

#[test]
fn body_read_crossing_the_deadline_times_out_without_commit() {
    let entry = asset_url(TAG, ARCHIVE_NAME);
    let respond = |clock: &Arc<FixedClock>, read_ms: u64, ignore_timeout: bool| {
        Scripted::Respond(Response {
            status: 200,
            location: None,
            content_length: Some(payload_bytes().len() as u64),
            body: Box::new(SlowBody::new(payload_bytes(), clock, read_ms, ignore_timeout)),
        })
    };

    // The head arrives with 500 ms left; the first read needs 1000 ms. A body
    // honoring its timeout fails at the deadline.
    let clock = Arc::new(FixedClock::new(0));
    let transport = ScriptedTransport::new();
    transport.push(
        &entry,
        after_ms(&clock, DOWNLOAD_TOTAL_MS - 500, respond(&clock, 1000, false)),
    );
    let mut store = MemoryPartStore::default();
    let result = run_download(&transport, &clock, &mut store, &CancelToken::new());
    assert_eq!(code(result), ErrorCode::Timeout);
    assert!(store.was_discarded());
    assert!(store.committed().is_none());

    // A body that overruns the deadline and then succeeds is not trusted.
    let clock = Arc::new(FixedClock::new(0));
    let transport = ScriptedTransport::new();
    transport.push(
        &entry,
        after_ms(&clock, DOWNLOAD_TOTAL_MS - 500, respond(&clock, 1000, true)),
    );
    let mut store = MemoryPartStore::default();
    let result = run_download(&transport, &clock, &mut store, &CancelToken::new());
    assert_eq!(code(result), ErrorCode::Timeout);
    assert!(store.was_discarded());
    assert!(store.committed().is_none());

    // The same body well inside the budget completes and commits.
    let clock = Arc::new(FixedClock::new(0));
    let transport = ScriptedTransport::new();
    transport.push(&entry, respond(&clock, 10, false));
    let mut store = MemoryPartStore::default();
    run_download(&transport, &clock, &mut store, &CancelToken::new()).unwrap();
    assert_eq!(store.committed().unwrap(), payload_bytes());
}

#[test]
fn metadata_assets_share_one_deadline() {
    let selection = select_release(
        &release_json("v0.27.0", &full_assets(), false, false),
        &Version::new(0, 26, 0),
        Target::WindowsX64,
    )
    .unwrap();

    // Catalog and signature each fit in 30s alone but not together.
    let clock = Arc::new(FixedClock::new(0));
    let transport = ScriptedTransport::new();
    transport.push(
        &selection.catalog_url,
        after_ms(&clock, 20_000, Scripted::body(200, b"catalog".to_vec())),
    );
    transport.push(
        &selection.signature_url,
        after_ms(&clock, 20_000, Scripted::body(200, b"sig".to_vec())),
    );
    let fetcher = Fetcher {
        transport: &transport,
        clock: &*clock,
    };
    let result = fetcher.fetch_catalog(&selection, &CancelToken::new());
    assert_eq!(code(result), ErrorCode::Timeout);

    // latest-release JSON and catalog fetch share a caller-provided deadline.
    let clock = Arc::new(FixedClock::new(0));
    let transport = ScriptedTransport::new();
    transport.push(
        &latest_release_url(),
        after_ms(
            &clock,
            METADATA_TOTAL_MS - 1000,
            Scripted::body(200, release_json("v0.27.0", &full_assets(), false, false)),
        ),
    );
    transport.push(
        &selection.catalog_url,
        after_ms(&clock, 2000, Scripted::body(200, b"catalog".to_vec())),
    );
    transport.push(&selection.signature_url, Scripted::body(200, b"sig".to_vec()));
    let fetcher = Fetcher {
        transport: &transport,
        clock: &*clock,
    };
    let cancel = CancelToken::new();
    let deadline = fetcher.metadata_deadline();
    let picked = fetcher
        .check_latest_within(&Version::new(0, 26, 0), Target::WindowsX64, &deadline, &cancel)
        .unwrap();
    let result = fetcher.fetch_catalog_within(&picked, &deadline, &cancel);
    assert_eq!(code(result), ErrorCode::Timeout);
}

#[test]
fn work_within_the_deadlines_succeeds() {
    let selection = select_release(
        &release_json("v0.27.0", &full_assets(), false, false),
        &Version::new(0, 26, 0),
        Target::WindowsX64,
    )
    .unwrap();
    let clock = Arc::new(FixedClock::new(0));
    let transport = ScriptedTransport::new();
    transport.push(
        &latest_release_url(),
        after_ms(
            &clock,
            10_000,
            Scripted::body(200, release_json("v0.27.0", &full_assets(), false, false)),
        ),
    );
    transport.push(
        &selection.catalog_url,
        after_ms(&clock, 10_000, Scripted::body(200, b"catalog".to_vec())),
    );
    transport.push(
        &selection.signature_url,
        after_ms(&clock, 9_000, Scripted::body(200, b"sig".to_vec())),
    );
    let fetcher = Fetcher {
        transport: &transport,
        clock: &*clock,
    };
    let cancel = CancelToken::new();
    let deadline = fetcher.metadata_deadline();
    let picked = fetcher
        .check_latest_within(&Version::new(0, 26, 0), Target::WindowsX64, &deadline, &cancel)
        .unwrap();
    let (catalog, signature) = fetcher
        .fetch_catalog_within(&picked, &deadline, &cancel)
        .unwrap();
    assert_eq!(catalog, b"catalog");
    assert_eq!(signature, b"sig");

    // A slow but in-budget archive download succeeds across redirect + body.
    let entry = asset_url(TAG, ARCHIVE_NAME);
    let target = "https://objects.githubusercontent.com/blob/1";
    let clock = Arc::new(FixedClock::new(0));
    let transport = ScriptedTransport::new();
    transport.push(
        &entry,
        after_ms(&clock, DOWNLOAD_TOTAL_MS / 3, Scripted::redirect(302, target)),
    );
    transport.push(
        target,
        after_ms(&clock, DOWNLOAD_TOTAL_MS / 3, Scripted::body(200, payload_bytes())),
    );
    let mut store = MemoryPartStore::default();
    run_download(&transport, &clock, &mut store, &CancelToken::new()).unwrap();
    assert_eq!(store.committed().unwrap(), payload_bytes());
}

struct CancelOnRead {
    token: CancelToken,
    sent: bool,
}

impl BodyReader for CancelOnRead {
    fn read_within(&mut self, buf: &mut [u8], _timeout_ms: u64) -> io::Result<usize> {
        if self.sent {
            return Ok(0);
        }
        self.sent = true;
        self.token.cancel();
        let n = buf.len().min(10);
        buf[..n].fill(1);
        Ok(n)
    }
}

#[test]
fn cancel_before_and_during_download_discards_the_part() {
    let entry = asset_url(TAG, ARCHIVE_NAME);

    let transport = ScriptedTransport::new();
    let token = CancelToken::new();
    token.cancel();
    let mut store = MemoryPartStore::default();
    let result = run_download(&transport, &FixedClock::new(0), &mut store, &token);
    assert_eq!(code(result), ErrorCode::Cancelled);
    assert!(transport.requests().is_empty());

    let token = CancelToken::new();
    let reader_token = token.clone();
    let transport = ScriptedTransport::new();
    transport.push(
        &entry,
        Scripted::Custom(Box::new(move || {
            Ok::<Response, TransportError>(Response {
                status: 200,
                location: None,
                content_length: None,
                body: Box::new(CancelOnRead {
                    token: reader_token,
                    sent: false,
                }),
            })
        })),
    );
    let mut store = MemoryPartStore::default();
    let result = run_download(&transport, &FixedClock::new(0), &mut store, &token);
    assert_eq!(code(result), ErrorCode::Cancelled);
    assert!(store.was_discarded());
    assert!(store.committed().is_none());
}

#[test]
fn only_hane_assets_can_be_requested() {
    let transport = ScriptedTransport::new();
    let clock = FixedClock::new(0);
    let fetcher = Fetcher {
        transport: &transport,
        clock: &clock,
    };
    let data = payload_bytes();
    let hash = sha256_hex(&data);
    let mut store = MemoryPartStore::default();
    for (tag, name) in [("v0.27.0", "../evil.zip"), ("latest", ARCHIVE_NAME), ("v0.27.0", "other.zip")] {
        let request = ArchiveDownload {
            tag,
            name,
            size: data.len() as u64,
            sha256: &hash,
        };
        let result = fetcher.download_archive(&request, &mut store, &CancelToken::new());
        assert!(result.is_err(), "{tag} {name}");
    }
    assert!(transport.requests().is_empty());
}

#[test]
fn dir_part_store_commits_atomically_and_discards() {
    let dir = TempDir::new("part");
    let mut store = DirPartStore::new(dir.path(), "candidate.zip").unwrap();
    {
        let mut writer = store.begin_part().unwrap();
        io::Write::write_all(&mut writer, b"partial").unwrap();
    }
    assert!(dir.path().join("candidate.zip.part").exists());
    assert!(!dir.path().join("candidate.zip").exists());
    store.discard_part();
    assert!(!dir.path().join("candidate.zip.part").exists());

    {
        let mut writer = store.begin_part().unwrap();
        io::Write::write_all(&mut writer, b"complete").unwrap();
    }
    store.commit_part().unwrap();
    assert_eq!(fs::read(store.complete_path()).unwrap(), b"complete");
    assert!(!dir.path().join("candidate.zip.part").exists());
    assert!(DirPartStore::new(dir.path(), "../x").is_err());
}

// --------------------------------------------------------- U-04 archive

struct Fixture {
    release: WindowsRelease,
    candidate: VerifiedCandidate,
    policy: TrustPolicy,
}

fn fixture(payload: Vec<ZipEntry>) -> Fixture {
    let release = windows_release(NEW_VERSION, 2, payload);
    let candidate = candidate(&release);
    Fixture {
        release,
        candidate,
        policy: test_policy(),
    }
}

fn stage(fixture: &Fixture, limits: &ArchiveLimits, stage: &Path) -> Result<InspectedArchive> {
    let mut cursor = Cursor::new(fixture.release.zip.clone());
    stage_archive(&mut cursor, &fixture.candidate, &fixture.policy, limits, stage)
}

/// A sandbox with an empty `stage/` and a sentinel beside it.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new("stage");
        fs::create_dir(root.path().join("stage")).unwrap();
        fs::write(root.path().join("outside.txt"), b"untouched").unwrap();
        Self { root }
    }

    fn stage(&self) -> std::path::PathBuf {
        self.root.path().join("stage")
    }

    fn assert_pristine(&self) {
        let stage_entries = fs::read_dir(self.stage()).unwrap().count();
        assert_eq!(stage_entries, 0, "stage must stay empty");
        assert_eq!(fs::read(self.root.path().join("outside.txt")).unwrap(), b"untouched");
        let mut top: Vec<String> = fs::read_dir(self.root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        top.sort();
        assert_eq!(top, vec!["outside.txt".to_string(), "stage".to_string()]);
    }
}

#[test]
fn a_valid_archive_is_inspected_and_extracted() {
    let fixture = fixture(default_payload(NEW_VERSION));
    let sandbox = Sandbox::new();
    let inspected = stage(&fixture, &ArchiveLimits::default(), &sandbox.stage()).unwrap();
    assert!(inspected.inventory.is_some());
    assert_eq!(inspected.total_unpacked, fixture.candidate.target_entry().unpacked_size);
    let stage_dir = sandbox.stage();
    assert_eq!(
        fs::read(stage_dir.join("hane.exe")).unwrap(),
        format!("hane.exe {NEW_VERSION}").as_bytes()
    );
    assert!(
        stage_dir
            .join("shell-integration")
            .join(NEW_VERSION)
            .join("hane_shell_extension.dll")
            .is_file()
    );
    assert!(stage_dir.join(INVENTORY_FILE_NAME).is_file());
    assert!(stage_dir.join(INVENTORY_SIGNATURE_FILE_NAME).is_file());
}

fn hostile(entry: ZipEntry) -> Vec<ZipEntry> {
    let mut payload = default_payload(NEW_VERSION);
    payload.push(entry);
    payload
}

#[test]
fn hostile_archives_are_rejected_before_anything_is_written() {
    let mut link_attrs = ZipEntry::file("attrs.txt", b"x");
    link_attrs.attributes |= 0x400;
    let mut encrypted = ZipEntry::file("secret.txt", b"x");
    encrypted.flags = 0x0001;
    let mut strong = ZipEntry::file("strong.txt", b"x");
    strong.flags = 0x0040;
    let mut bzip = ZipEntry::file("packed.bin", b"x");
    bzip.method = 12;
    let mut aes = ZipEntry::file("aes.bin", b"x");
    aes.extra = vec![0x01, 0x99, 0x00, 0x00];
    let mut zip64 = ZipEntry::file("big.bin", b"x");
    zip64.extra = vec![0x01, 0x00, 0x00, 0x00];
    let mut bad_flags = ZipEntry::file("flags.bin", b"x");
    bad_flags.flags = 0x0010;
    let mut local_name_differs = ZipEntry::file("honest.txt", b"x");
    local_name_differs.local_name = Some(b"../evil.txt".to_vec());
    let mut dir_with_data = ZipEntry::dir("sub/");
    dir_with_data.declared_size = Some(5);
    let mut mode_mismatch = ZipEntry::file("looks-like-dir.txt", b"x");
    mode_mismatch.attributes = 0o040_755 << 16;

    let cases: Vec<(&str, ZipEntry)> = vec![
        ("parent traversal", ZipEntry::file("../evil.txt", b"x")),
        ("nested traversal", ZipEntry::file("a/../../evil.txt", b"x")),
        ("absolute path", ZipEntry::file("/abs.txt", b"x")),
        ("backslash separator", ZipEntry::file("..\\evil.txt", b"x")),
        ("backslash dir", ZipEntry::file("a\\b.txt", b"x")),
        ("drive letter", ZipEntry::file("C:evil.txt", b"x")),
        ("alternate data stream", ZipEntry::file("file.txt:stream", b"x")),
        ("reserved device", ZipEntry::file("CON.txt", b"x")),
        ("reserved device in dir", ZipEntry::file("dir/nul", b"x")),
        ("reserved com port", ZipEntry::file("com1", b"x")),
        ("trailing dot", ZipEntry::file("trailing.", b"x")),
        ("trailing space", ZipEntry::file("trailing ", b"x")),
        ("archiver metadata", ZipEntry::file("__MACOSX/x", b"x")),
        ("non ascii name", ZipEntry::file("caf\u{e9}.txt", b"x")),
        ("invalid utf-8", ZipEntry::file("x", b"x").raw_name(&[0xff, 0xfe, b'a'])),
        ("empty component", ZipEntry::file("a//b", b"x")),
        ("control character", ZipEntry::file("a\u{1}b", b"x")),
        ("symlink", ZipEntry::file("link", b"target").mode(0o120_777)),
        ("device node", ZipEntry::file("dev", b"x").mode(0o060_644)),
        ("reparse attribute", link_attrs),
        ("encrypted", encrypted),
        ("strong encryption", strong),
        ("unsupported compression", bzip),
        ("aes extra", aes),
        ("zip64 extra", zip64),
        ("unsupported flags", bad_flags),
        ("local header names differ", local_name_differs),
        ("directory carrying data", dir_with_data),
        ("mode contradicts name", mode_mismatch),
        ("duplicate name", ZipEntry::file("README.md", b"other")),
        ("case collision", ZipEntry::file("readme.MD", b"other")),
        ("file below a file", ZipEntry::file("README.md/child", b"x")),
        ("file over a directory", ZipEntry::file("shell-integration", b"x")),
        ("unexpected executable", ZipEntry::file("evil.exe", b"MZ").executable()),
        ("unexpected script", ZipEntry::file("run.ps1", b"x")),
        ("decompression bomb", ZipEntry::file("bomb.bin", &vec![0u8; 8 << 20])),
    ];
    for (name, entry) in cases {
        let fixture = fixture(hostile(entry));
        let sandbox = Sandbox::new();
        let result = stage(&fixture, &ArchiveLimits::default(), &sandbox.stage());
        let error = result.err().unwrap_or_else(|| panic!("{name} was accepted"));
        assert!(
            matches!(error.code, ErrorCode::ArchiveRejected | ErrorCode::LimitExceeded),
            "{name}: {error}"
        );
        sandbox.assert_pristine();
    }
}

#[test]
fn declared_sizes_must_match_the_real_bytes() {
    let mut smaller = ZipEntry::file("hane.exe", b"hane.exe 0.27.0");
    smaller.declared_size = Some(3);
    let mut larger = ZipEntry::file("hane.exe", b"hane.exe 0.27.0");
    larger.declared_size = Some(500);
    let mut bad_crc = ZipEntry::file("hane.exe", b"hane.exe 0.27.0");
    bad_crc.crc = Some(0);
    for (name, entry) in [("smaller", smaller), ("larger", larger), ("crc", bad_crc)] {
        let mut payload = default_payload(NEW_VERSION);
        payload.retain(|e| e.name != b"hane.exe");
        payload.push(entry);
        let fixture = fixture(payload);
        let sandbox = Sandbox::new();
        let result = stage(&fixture, &ArchiveLimits::default(), &sandbox.stage());
        assert_eq!(code(result), ErrorCode::ArchiveRejected, "{name}");
        sandbox.assert_pristine();
    }
}

#[test]
fn archive_limits_are_enforced_on_counts_and_bytes() {
    let fixture = fixture(default_payload(NEW_VERSION));
    let defaults = ArchiveLimits::default();
    let limits = [
        ArchiveLimits {
            max_entries: 3,
            ..defaults.clone()
        },
        ArchiveLimits {
            max_entry_bytes: 4,
            ..defaults.clone()
        },
        ArchiveLimits {
            max_total_bytes: 10,
            ..defaults.clone()
        },
        ArchiveLimits {
            max_archive_bytes: 100,
            ..defaults
        },
    ];
    for (index, limits) in limits.iter().enumerate() {
        let sandbox = Sandbox::new();
        let result = stage(&fixture, limits, &sandbox.stage());
        assert_eq!(code(result), ErrorCode::LimitExceeded, "limit case {index}");
        sandbox.assert_pristine();
    }
}

#[test]
fn layout_must_match_the_documented_files() {
    let mut without_helper = default_payload(NEW_VERSION);
    without_helper.retain(|e| e.name != b"hane-updater.exe");
    let mut payloads = vec![("missing helper", without_helper)];
    let mut unknown_dir = default_payload(NEW_VERSION);
    unknown_dir.push(ZipEntry::dir("plugins/"));
    payloads.push(("unknown directory", unknown_dir));
    let mut deep_dll = default_payload(NEW_VERSION);
    deep_dll.push(ZipEntry::file("shell-integration/a/b/hane_shell_extension.dll", b"x"));
    payloads.push(("misplaced dll", deep_dll));
    for (name, payload) in payloads {
        let fixture = fixture(payload);
        let sandbox = Sandbox::new();
        let result = stage(&fixture, &ArchiveLimits::default(), &sandbox.stage());
        assert_eq!(code(result), ErrorCode::ArchiveRejected, "{name}");
        sandbox.assert_pristine();
    }
}

#[test]
fn unpacked_total_must_match_the_catalog() {
    let release = windows_release(NEW_VERSION, 2, default_payload(NEW_VERSION));
    let mut doc = release.doc.clone();
    doc.targets.values_mut().for_each(|t| t.unpacked_size += 1);
    let (bytes, sig) = sign_catalog(&doc);
    let candidate = verify_with(
        &bytes,
        &sig,
        &test_policy(),
        &FixedClock::new(test_now()),
        Target::WindowsX64,
        CURRENT,
        None,
    )
    .unwrap();
    let sandbox = Sandbox::new();
    let mut cursor = Cursor::new(release.zip.clone());
    let result = stage_archive(
        &mut cursor,
        &candidate,
        &test_policy(),
        &ArchiveLimits::default(),
        &sandbox.stage(),
    );
    assert_eq!(code(result), ErrorCode::SizeMismatch);
    sandbox.assert_pristine();
}

#[test]
fn inventory_must_agree_with_archive_signature_and_catalog() {
    let sandbox_check = |fixture: &Fixture| {
        let sandbox = Sandbox::new();
        let result = stage(fixture, &ArchiveLimits::default(), &sandbox.stage());
        let error = code(result);
        sandbox.assert_pristine();
        error
    };

    // Signed inventory lists different content of the same length.
    let listed = vec![
        ("hane.exe".to_string(), b"hane.exe 0.27.X".to_vec()),
        (
            "hane-updater.exe".to_string(),
            format!("hane-updater.exe {NEW_VERSION}").into_bytes(),
        ),
        (
            format!("shell-integration/{NEW_VERSION}/hane_shell_extension.dll"),
            format!("dll {NEW_VERSION}").into_bytes(),
        ),
        ("README.md".to_string(), b"readme".to_vec()),
    ];
    let release = windows_release_with(NEW_VERSION, 2, default_payload(NEW_VERSION), Some(listed));
    let fixture_a = Fixture {
        candidate: candidate(&release),
        release,
        policy: test_policy(),
    };
    assert_eq!(sandbox_check(&fixture_a), ErrorCode::HashMismatch);

    // The inventory lists a file the archive does not contain.
    let mut listed_extra: Vec<(String, Vec<u8>)> = default_payload(NEW_VERSION)
        .iter()
        .filter(|e| !e.is_dir())
        .map(|e| (String::from_utf8_lossy(&e.name).into_owned(), e.data.clone()))
        .collect();
    listed_extra.push(("LICENSE".to_string(), b"license".to_vec()));
    let release = windows_release_with(NEW_VERSION, 2, default_payload(NEW_VERSION), Some(listed_extra));
    let fixture_b = Fixture {
        candidate: candidate(&release),
        release,
        policy: test_policy(),
    };
    assert_eq!(sandbox_check(&fixture_b), ErrorCode::ArchiveRejected);

    // Inventory signed by a key the policy does not trust.
    let good = windows_release(NEW_VERSION, 2, default_payload(NEW_VERSION));
    let mut entries = default_payload(NEW_VERSION);
    entries.push(ZipEntry::file(INVENTORY_FILE_NAME, &good.inventory));
    entries.push(ZipEntry::file(
        INVENTORY_SIGNATURE_FILE_NAME,
        &sign_with(2, crate::testing::TEST_KEY_ID, INVENTORY_DOMAIN, &good.inventory),
    ));
    let forged = WindowsRelease {
        zip: build_zip(&entries),
        catalog: good.catalog.clone(),
        catalog_sig: good.catalog_sig.clone(),
        inventory: good.inventory.clone(),
        inventory_sig: good.inventory_sig.clone(),
        doc: good.doc.clone(),
    };
    let fixture_c = Fixture {
        candidate: candidate(&good),
        release: forged,
        policy: test_policy(),
    };
    assert_eq!(sandbox_check(&fixture_c), ErrorCode::SignatureInvalid);

    // A different (validly signed) inventory than the one the catalog binds.
    let other = windows_release(NEW_VERSION, 2, {
        let mut payload = default_payload(NEW_VERSION);
        payload.retain(|e| e.name != b"README.md");
        payload.push(ZipEntry::file("README.md", b"README"));
        payload
    });
    let fixture_d = Fixture {
        candidate: candidate(&good),
        release: other,
        policy: test_policy(),
    };
    assert_eq!(sandbox_check(&fixture_d), ErrorCode::HashMismatch);
}

#[test]
fn extraction_requires_an_empty_real_directory() {
    let fixture = fixture(default_payload(NEW_VERSION));
    let sandbox = Sandbox::new();
    fs::write(sandbox.stage().join("existing.txt"), b"keep").unwrap();
    let result = stage(&fixture, &ArchiveLimits::default(), &sandbox.stage());
    assert_eq!(code(result), ErrorCode::StageNotEmpty);
    assert_eq!(fs::read(sandbox.stage().join("existing.txt")).unwrap(), b"keep");

    let missing = sandbox.root.path().join("no-such-stage");
    assert!(stage(&fixture, &ArchiveLimits::default(), &missing).is_err());
    assert!(!missing.exists());
}

#[cfg(unix)]
#[test]
fn extraction_does_not_enter_a_symlinked_stage() {
    let fixture = fixture(default_payload(NEW_VERSION));
    let sandbox = Sandbox::new();
    let real = sandbox.root.path().join("elsewhere");
    fs::create_dir(&real).unwrap();
    let link = sandbox.root.path().join("link-stage");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let result = stage(&fixture, &ArchiveLimits::default(), &link);
    assert_eq!(code(result), ErrorCode::StageNotEmpty);
    assert_eq!(fs::read_dir(real.as_path()).unwrap().count(), 0);
}

fn macos_zip(extra: Vec<ZipEntry>, with_helper: bool) -> Vec<u8> {
    let mut entries = vec![
        ZipEntry::dir("hane-macos-arm64/"),
        ZipEntry::dir("hane-macos-arm64/Hane.app/"),
        ZipEntry::file("hane-macos-arm64/Hane.app/Contents/Info.plist", b"plist"),
        ZipEntry::file("hane-macos-arm64/Hane.app/Contents/MacOS/hane", b"bin").executable(),
        ZipEntry::file("hane-macos-arm64/Hane.app/Contents/Resources/hane-install.json", b"{}"),
        ZipEntry::file(
            "hane-macos-arm64/Hane.app/Contents/Resources/hane-install.json.sig",
            b"{}",
        ),
        ZipEntry::file("hane-macos-arm64/README.md", b"readme"),
    ];
    if with_helper {
        entries.push(
            ZipEntry::file("hane-macos-arm64/Hane.app/Contents/Helpers/hane-updater", b"h")
                .executable(),
        );
    }
    entries.extend(extra);
    build_zip(&entries)
}

fn macos_layout(zip: Vec<u8>) -> Result<()> {
    let entries = scan_archive(&mut Cursor::new(zip), &ArchiveLimits::default())?;
    check_layout(Target::MacosArm64, &entries)
}

#[test]
fn macos_layout_allows_only_the_bundle_and_documents() {
    assert!(macos_layout(macos_zip(vec![], true)).is_ok());
    assert_eq!(code(macos_layout(macos_zip(vec![], false))), ErrorCode::ArchiveRejected);
    let extras = [
        ZipEntry::file("hane-macos-arm64/Hane.app/Contents/Resources/run.sh", b"#!").executable(),
        ZipEntry::file("evil/payload", b"x"),
        ZipEntry::file("hane-macos-arm64/stray.bin", b"x"),
        ZipEntry::file("__MACOSX/._Hane", b"x"),
    ];
    for extra in extras {
        let name = String::from_utf8_lossy(&extra.name).into_owned();
        assert_eq!(
            code(macos_layout(macos_zip(vec![extra], true))),
            ErrorCode::ArchiveRejected,
            "{name}"
        );
    }
}

#[test]
fn malformed_containers_are_rejected() {
    let good = build_zip(&[ZipEntry::file("a.txt", b"hello")]);
    let limits = ArchiveLimits::default();
    let scan = |bytes: Vec<u8>| scan_archive(&mut Cursor::new(bytes), &limits);
    assert!(scan(good.clone()).is_ok());
    assert_eq!(code(scan(Vec::new())), ErrorCode::ArchiveRejected);
    assert_eq!(code(scan(b"PK not really a zip file at all".to_vec())), ErrorCode::ArchiveRejected);
    // Truncation removes the end record.
    assert_eq!(code(scan(good[..good.len() - 5].to_vec())), ErrorCode::ArchiveRejected);
    // Garbage between the central directory and the end record.
    let eocd_at = good.len() - 22;
    let mut gap = good[..eocd_at].to_vec();
    gap.extend_from_slice(b"junk");
    gap.extend_from_slice(&good[eocd_at..]);
    assert_eq!(code(scan(gap)), ErrorCode::ArchiveRejected);
    // Entries counted twice make the directory run short.
    let mut doubled = good.clone();
    doubled[good.len() - 12] = 2;
    doubled[good.len() - 14] = 2;
    assert_eq!(code(scan(doubled)), ErrorCode::ArchiveRejected);
    // Zero entries.
    assert_eq!(code(scan(build_zip(&[]))), ErrorCode::ArchiveRejected);
}

// ------------------------------------------------- U-05 identity and plan

#[test]
fn install_id_depends_on_every_fact() {
    let base = InstallFacts {
        os: "windows",
        volume_id: "vol-1",
        install_path: "C:/Hane",
        owner: "S-1-5-21-1",
    };
    let id = InstallId::derive(&base).unwrap();
    assert_eq!(id, InstallId::derive(&base).unwrap());
    assert_eq!(id.as_str().len(), 32);
    assert!(id.coordination_dir_name().starts_with(".hane-update-"));
    for changed in [
        InstallFacts { os: "macos", ..base },
        InstallFacts { volume_id: "vol-2", ..base },
        InstallFacts { install_path: "C:/Hane2", ..base },
        InstallFacts { owner: "S-1-5-21-2", ..base },
    ] {
        assert_ne!(id, InstallId::derive(&changed).unwrap());
    }
    // Field boundaries are length-prefixed: shifting bytes between fields differs.
    let a = InstallFacts { os: "ab", volume_id: "c", install_path: "p", owner: "o" };
    let b = InstallFacts { os: "a", volume_id: "bc", install_path: "p", owner: "o" };
    assert_ne!(InstallId::derive(&a).unwrap(), InstallId::derive(&b).unwrap());
    assert_eq!(
        code(InstallId::derive(&InstallFacts { owner: "", ..base })),
        ErrorCode::UnsupportedInstall
    );
    assert!(InstallId::parse("XYZ").is_err());
    assert!(InstallId::parse(id.as_str()).is_ok());
    assert_ne!(TransactionId::generate().unwrap(), TransactionId::generate().unwrap());
}

#[test]
fn install_path_text_keeps_unicode_and_rejects_lossy_paths() {
    use std::path::Path;
    assert_eq!(
        crate::identity::install_path_text(Path::new("/Applications/Hane.app")).unwrap(),
        "/Applications/Hane.app"
    );
}

#[cfg(unix)]
#[test]
fn distinct_invalid_byte_paths_never_share_an_install_id() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    let a = Path::new(OsStr::from_bytes(b"/Apps/\xff/Hane.app"));
    let b = Path::new(OsStr::from_bytes(b"/Apps/\xfe/Hane.app"));
    // The lossy form collides; the contract must reject instead of adopting it.
    assert_eq!(a.to_string_lossy(), b.to_string_lossy());
    assert_eq!(
        code(crate::identity::install_path_text(a)),
        ErrorCode::UnsupportedInstall
    );
    assert_eq!(
        code(crate::identity::install_path_text(b)),
        ErrorCode::UnsupportedInstall
    );
}

#[cfg(windows)]
#[test]
fn distinct_invalid_utf16_paths_never_share_an_install_id() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;
    let wide = |unit: u16| {
        let mut units: Vec<u16> = "C:\\Apps\\".encode_utf16().collect();
        units.push(unit);
        PathBuf::from(OsString::from_wide(&units))
    };
    let (a, b) = (wide(0xD800), wide(0xD801));
    assert_eq!(a.to_string_lossy(), b.to_string_lossy());
    assert_eq!(
        code(crate::identity::install_path_text(&a)),
        ErrorCode::UnsupportedInstall
    );
    assert_eq!(
        code(crate::identity::install_path_text(&b)),
        ErrorCode::UnsupportedInstall
    );
}

fn old_payload() -> Vec<ZipEntry> {
    let mut payload = default_payload(CURRENT);
    payload.push(ZipEntry::file("Trust-HaneShellCertificate.ps1", b"trust script"));
    payload
}

struct World {
    plan: ApplyPlan,
    old_files: BTreeMap<String, Vec<u8>>,
    new_files: BTreeMap<String, Vec<u8>>,
    install_id: InstallId,
    candidate: VerifiedCandidate,
    old_inventory: SignedInventory,
    new_inventory: SignedInventory,
}

fn files_of(payload: &[ZipEntry], release: &WindowsRelease) -> BTreeMap<String, Vec<u8>> {
    let mut files: BTreeMap<String, Vec<u8>> = payload
        .iter()
        .filter(|e| !e.is_dir())
        .map(|e| (String::from_utf8_lossy(&e.name).into_owned(), e.data.clone()))
        .collect();
    files.insert(INVENTORY_FILE_NAME.to_string(), release.inventory.clone());
    files.insert(INVENTORY_SIGNATURE_FILE_NAME.to_string(), release.inventory_sig.clone());
    files
}

fn world() -> World {
    let old_release = windows_release(CURRENT, 1, old_payload());
    let new_release = windows_release(NEW_VERSION, 2, default_payload(NEW_VERSION));
    let policy = test_policy();
    let old_inventory = SignedInventory::verify(&old_release.inventory, &old_release.inventory_sig, &policy).unwrap();
    let new_inventory = SignedInventory::verify(&new_release.inventory, &new_release.inventory_sig, &policy).unwrap();
    let candidate = candidate(&new_release);
    let install_id = InstallId::derive(&InstallFacts {
        os: "windows",
        volume_id: "vol-1",
        install_path: "C:/Hane",
        owner: "S-1-5-21-1",
    })
    .unwrap();
    let plan = ApplyPlan::for_inventories(
        install_id.clone(),
        TransactionId::parse(&"ab".repeat(16)).unwrap(),
        &old_inventory,
        &new_inventory,
        &candidate,
    )
    .unwrap();
    World {
        plan,
        old_files: files_of(&old_payload(), &old_release),
        new_files: files_of(&default_payload(NEW_VERSION), &new_release),
        install_id,
        candidate,
        old_inventory,
        new_inventory,
    }
}

impl World {
    fn old_disk(&self) -> MemoryDisk {
        let mut disk = MemoryDisk::default();
        for (path, bytes) in &self.old_files {
            disk.set(path, bytes);
        }
        for step in &self.plan.steps {
            if let (Some(backup), Some(_)) = (&step.backup_rel, &step.old) {
                disk.set(backup, &self.old_files[&step.rel_path]);
            }
        }
        disk
    }

    fn apply_step(&self, disk: &mut MemoryDisk, index: usize) {
        let step = &self.plan.steps[index];
        match &step.new {
            Some(_) => disk.set(&step.rel_path, &self.new_files[&step.rel_path]),
            None => disk.remove(&step.rel_path),
        }
    }
}

#[test]
fn apply_plan_pins_old_and_new_identity() {
    let world = world();
    let plan = &world.plan;
    let paths: Vec<&str> = plan.steps.iter().map(|s| s.rel_path.as_str()).collect();
    assert_eq!(paths.last(), Some(&"hane.exe"), "hane.exe is replaced last");
    assert!(!paths.contains(&"README.md"), "unchanged files are not touched");
    assert!(
        !paths.iter().any(|p| p.contains("shell-integration/0.26.0")),
        "the previous shell DLL is retained"
    );
    let dll = plan
        .steps
        .iter()
        .find(|s| s.rel_path.ends_with("0.27.0/hane_shell_extension.dll"))
        .expect("new DLL is added");
    assert!(dll.old.is_none() && dll.new.is_some() && dll.backup_rel.is_none());
    let removed = plan
        .steps
        .iter()
        .find(|s| s.rel_path == "Trust-HaneShellCertificate.ps1")
        .expect("dropped file is removed");
    assert!(removed.new.is_none() && removed.old.is_some() && removed.backup_rel.is_some());
    assert!(paths.contains(&INVENTORY_FILE_NAME) && paths.contains(&INVENTORY_SIGNATURE_FILE_NAME));
    for step in &plan.steps {
        crate::paths::validate_rel_path(&step.rel_path).unwrap();
    }
    assert_eq!(plan.source.version, CURRENT);
    assert_eq!(plan.destination.version, NEW_VERSION);
    assert_eq!(
        plan.destination.inventory_sha256.as_deref(),
        Some(world.new_inventory.raw_sha256())
    );
    assert_eq!(plan.source.inventory_sha256.as_deref(), Some(world.old_inventory.raw_sha256()));
    assert_eq!(plan.plan_hash().unwrap(), plan.clone().plan_hash().unwrap());
}

#[test]
fn apply_plan_requires_an_applicable_candidate() {
    let world = world();
    // Same version: not newer, so not applicable.
    let same = windows_release(CURRENT, 3, default_payload(CURRENT));
    let same_candidate = candidate(&same);
    let same_inventory = SignedInventory::verify(&same.inventory, &same.inventory_sig, &test_policy()).unwrap();
    let result = ApplyPlan::for_inventories(
        world.install_id.clone(),
        TransactionId::generate().unwrap(),
        &world.old_inventory,
        &same_inventory,
        &same_candidate,
    );
    assert_eq!(code(result), ErrorCode::NotApplicable);
    // The installed inventory must be older than the candidate.
    let result = ApplyPlan::for_inventories(
        world.install_id.clone(),
        TransactionId::generate().unwrap(),
        &world.new_inventory,
        &world.new_inventory,
        &world.candidate,
    );
    assert_eq!(code(result), ErrorCode::NotApplicable);
    // The inventory must be the one the catalog binds.
    let other = windows_release(NEW_VERSION, 4, {
        let mut payload = default_payload(NEW_VERSION);
        payload.push(ZipEntry::file("LICENSE", b"license"));
        payload
    });
    let other_inventory = SignedInventory::verify(&other.inventory, &other.inventory_sig, &test_policy()).unwrap();
    let result = ApplyPlan::for_inventories(
        world.install_id.clone(),
        TransactionId::generate().unwrap(),
        &world.old_inventory,
        &other_inventory,
        &world.candidate,
    );
    assert!(result.is_err());
}

#[test]
fn inventories_need_a_valid_signature_and_content() {
    let release = windows_release(NEW_VERSION, 2, default_payload(NEW_VERSION));
    let policy = test_policy();
    assert!(SignedInventory::verify(&release.inventory, &release.inventory_sig, &policy).is_ok());
    assert_eq!(
        code(SignedInventory::verify(&release.inventory, &release.inventory_sig, &TrustPolicy::production())),
        ErrorCode::TrustUnconfigured
    );
    let wrong_domain = sign(CATALOG_DOMAIN, &release.inventory);
    assert_eq!(
        code(SignedInventory::verify(&release.inventory, &wrong_domain, &policy)),
        ErrorCode::SignatureInvalid
    );
    let mut tampered = release.inventory.clone();
    let last = tampered.len() - 2;
    tampered[last] ^= 1;
    assert_eq!(
        code(SignedInventory::verify(&tampered, &release.inventory_sig, &policy)),
        ErrorCode::SignatureInvalid
    );
    // Signed but unsafe: a traversal path in the inventory.
    let unsafe_listing = windows_release_with(
        NEW_VERSION,
        2,
        default_payload(NEW_VERSION),
        Some(vec![("../evil.exe".to_string(), b"x".to_vec())]),
    );
    assert_eq!(
        code(SignedInventory::verify(&unsafe_listing.inventory, &unsafe_listing.inventory_sig, &policy)),
        ErrorCode::ArchiveRejected
    );
    let duplicate = windows_release_with(
        NEW_VERSION,
        2,
        default_payload(NEW_VERSION),
        Some(vec![
            ("a.txt".to_string(), b"x".to_vec()),
            ("A.TXT".to_string(), b"y".to_vec()),
        ]),
    );
    assert_eq!(
        code(SignedInventory::verify(&duplicate.inventory, &duplicate.inventory_sig, &policy)),
        ErrorCode::MalformedMetadata
    );
}

#[test]
fn preconditions_stop_on_modified_or_foreign_files() {
    let world = world();
    let disk = world.old_disk();
    check_plan_preconditions(&world.plan, &disk).unwrap();

    let mut modified = world.old_disk();
    modified.set("hane.exe", b"patched by someone");
    assert_eq!(
        code(check_plan_preconditions(&world.plan, &modified)),
        ErrorCode::InstallModified
    );
    let mut missing = world.old_disk();
    missing.remove("hane-updater.exe");
    assert_eq!(
        code(check_plan_preconditions(&world.plan, &missing)),
        ErrorCode::InstallModified
    );
    let mut foreign = world.old_disk();
    foreign.set(
        &format!("shell-integration/{NEW_VERSION}/hane_shell_extension.dll"),
        b"unknown file",
    );
    assert_eq!(
        code(check_plan_preconditions(&world.plan, &foreign)),
        ErrorCode::InstallModified
    );
}

#[test]
fn preconditions_verify_owned_files_that_the_update_does_not_replace() {
    let world = world();
    assert!(
        world.plan.unchanged.iter().any(|f| f.rel_path == "README.md"),
        "README.md is owned and unchanged"
    );
    check_plan_preconditions(&world.plan, &world.old_disk()).unwrap();

    let mut modified = world.old_disk();
    modified.set("README.md", b"edited by someone");
    assert_eq!(
        code(check_plan_preconditions(&world.plan, &modified)),
        ErrorCode::InstallModified
    );
    let mut missing = world.old_disk();
    missing.remove("README.md");
    assert_eq!(
        code(check_plan_preconditions(&world.plan, &missing)),
        ErrorCode::InstallModified
    );
}

// ------------------------------------------------------ U-12 journal

fn journal(world: &World) -> UpdateJournal {
    UpdateJournal::begin(&world.plan, Some(sha256_hex(b"ticket"))).unwrap()
}

#[test]
fn journal_phases_and_steps_move_only_forward() {
    let world = world();
    let mut journal = journal(&world);
    assert_eq!(journal.phase, Phase::Prepared);
    assert_eq!(journal.steps.len(), world.plan.steps.len());
    assert_eq!(journal.plan_hash, world.plan.plan_hash().unwrap());

    assert_eq!(code(journal.transition(Phase::Applying)), ErrorCode::TransitionInvalid);
    assert_eq!(code(journal.transition(Phase::Committed)), ErrorCode::TransitionInvalid);
    assert_eq!(code(journal.set_step(0, StepState::Intent)), ErrorCode::TransitionInvalid);
    journal.transition(Phase::ExitAuthorized).unwrap();
    journal.transition(Phase::Applying).unwrap();
    assert_eq!(code(journal.set_step(0, StepState::Applied)), ErrorCode::TransitionInvalid);
    journal.set_step(0, StepState::Intent).unwrap();
    journal.set_step(0, StepState::Applied).unwrap();
    assert_eq!(code(journal.set_step(0, StepState::Intent)), ErrorCode::TransitionInvalid);
    assert_eq!(code(journal.set_step(0, StepState::Restored)), ErrorCode::TransitionInvalid);
    assert_eq!(code(journal.set_step(999, StepState::Intent)), ErrorCode::TransitionInvalid);
    journal.transition(Phase::AwaitingHealth).unwrap();
    journal.transition(Phase::Committed).unwrap();
    for next in [Phase::RollingBack, Phase::RolledBack, Phase::Applying, Phase::RecoveryRequired] {
        assert_eq!(code(journal.transition(next)), ErrorCode::TransitionInvalid, "{next:?}");
    }

    let mut failing = self::journal(&world);
    failing.transition(Phase::RecoveryRequired).unwrap();
    assert_eq!(code(failing.transition(Phase::RolledBack)), ErrorCode::TransitionInvalid);
}

#[test]
fn durable_journal_does_not_advance_when_the_flush_fails() {
    let world = world();
    let store = FaultyJournalStore::default();
    let mut durable = DurableJournal::create(store.clone(), journal(&world)).unwrap();
    assert_eq!(
        code(DurableJournal::create(store.clone(), journal(&world))),
        ErrorCode::StaleJournal
    );

    store.script(vec![Some(Fault::FailBefore)]);
    assert_eq!(code(durable.advance_phase(Phase::ExitAuthorized)), ErrorCode::Io);
    assert_eq!(durable.journal().phase, Phase::Prepared);
    assert_eq!(store.stored().unwrap().phase, Phase::Prepared);

    durable.advance_phase(Phase::ExitAuthorized).unwrap();
    assert_eq!(durable.journal().generation, 1);
    assert_eq!(store.stored().unwrap().phase, Phase::ExitAuthorized);

    // The flush lands but the call reports failure: the writer cannot know,
    // so it refuses to continue and recovery has to read the disk.
    store.script(vec![Some(Fault::FailAfter)]);
    assert_eq!(code(durable.advance_phase(Phase::Applying)), ErrorCode::Io);
    assert_eq!(durable.journal().phase, Phase::ExitAuthorized);
    assert_eq!(store.stored().unwrap().phase, Phase::Applying);
    assert_eq!(
        code(durable.advance_phase(Phase::Applying)),
        ErrorCode::StaleJournal
    );
    // A restarted process reopens what is really stored.
    let reopened = DurableJournal::open(store.clone()).unwrap().unwrap();
    assert_eq!(reopened.journal().phase, Phase::Applying);
    assert!(DurableJournal::open(FaultyJournalStore::default()).unwrap().is_none());
}

#[test]
fn file_journal_store_round_trips_and_rejects_bad_files() {
    let world = world();
    let dir = TempDir::new("journal");
    let path = dir.path().join("journal.json");
    let mut store = FileJournalStore::new(path.clone());
    assert!(store.load().unwrap().is_none());
    let mut journal = journal(&world);
    journal.generation = 3;
    store.store(&journal).unwrap();
    assert_eq!(store.load().unwrap(), Some(journal.clone()));
    assert!(!dir.path().join(".journal.json.tmp").exists());

    let text = fs::read_to_string(&path).unwrap();
    let corrupt: Vec<(&str, String)> = vec![
        ("not json", "not json".to_string()),
        ("unknown field", text.replacen('{', r#"{"surprise":1,"#, 1)),
        ("duplicate key", text.replacen('{', r#"{"schema":1,"#, 1)),
        ("traversal", text.replace("hane-updater.exe", "../hane-updater.exe")),
        ("bad schema", text.replacen(r#""schema":1"#, r#""schema":9"#, 1)),
    ];
    for (name, content) in corrupt {
        fs::write(&path, content).unwrap();
        let error = store.load().err().unwrap_or_else(|| panic!("{name} was accepted"));
        assert!(
            matches!(error.code, ErrorCode::JournalInvalid | ErrorCode::ArchiveRejected),
            "{name}: {error}"
        );
    }
    fs::write(path.as_path(), vec![b' '; (crate::limits::MAX_JOURNAL_BYTES + 1) as usize]).unwrap();
    assert_eq!(code(store.load()), ErrorCode::LimitExceeded);
}

/// Index of a step that replaces an existing file (so it owns a backup path).
fn replacing_step(world: &World) -> usize {
    world
        .plan
        .steps
        .iter()
        .position(|s| s.old.is_some() && s.backup_rel.is_some())
        .expect("plan has a replacing step")
}

#[test]
fn journal_begin_pins_the_backup_path_to_the_step_namespace() {
    let world = world();
    let index = replacing_step(&world);
    let step = &world.plan.steps[index];
    assert_eq!(step.backup_rel.as_deref(), Some(format!("backup/{}", step.rel_path).as_str()));
    // The canonical `backup/<rel_path>` is accepted.
    UpdateJournal::begin(&world.plan, None).unwrap();

    let other = format!("backup/{}-other", step.rel_path);
    for bad in ["user.md", "backup/other.bin", other.as_str(), "backup", "backup/"] {
        let mut plan = world.plan.clone();
        plan.steps[index].backup_rel = Some(bad.to_string());
        let result = UpdateJournal::begin(&plan, None);
        let error = result.err().unwrap_or_else(|| panic!("backup {bad:?} was accepted"));
        assert!(
            matches!(error.code, ErrorCode::JournalInvalid | ErrorCode::ArchiveRejected),
            "{bad:?}: {error}"
        );
    }
    // Rejection is exact for a well-formed path outside the step's namespace.
    let mut plan = world.plan.clone();
    plan.steps[index].backup_rel = Some("user.md".to_string());
    assert_eq!(code(UpdateJournal::begin(&plan, None)), ErrorCode::JournalInvalid);
    plan.steps[index].backup_rel = Some("backup/other.bin".to_string());
    assert_eq!(code(UpdateJournal::begin(&plan, None)), ErrorCode::JournalInvalid);

    // A step without an old file must not carry a backup path.
    let mut plan = world.plan.clone();
    plan.steps[index].old = None;
    assert_eq!(code(UpdateJournal::begin(&plan, None)), ErrorCode::JournalInvalid);
}

#[test]
fn durable_journal_create_rejects_a_foreign_backup_path_and_stores_nothing() {
    let world = world();
    let index = replacing_step(&world);
    let mut tampered = journal(&world);
    tampered.steps[index].backup_rel = Some("user.md".to_string());

    let store = FaultyJournalStore::default();
    assert_eq!(
        code(DurableJournal::create(store.clone(), tampered.clone())),
        ErrorCode::JournalInvalid
    );
    assert!(store.stored().is_none());

    let dir = TempDir::new("journal-backup-create");
    let user_file = dir.path().join("user.md");
    fs::write(&user_file, b"user data").unwrap();
    let path = dir.path().join("journal.json");
    assert_eq!(
        code(DurableJournal::create(FileJournalStore::new(path.clone()), tampered)),
        ErrorCode::JournalInvalid
    );
    assert!(!path.exists());
    assert_eq!(fs::read(&user_file).unwrap(), b"user data");
}

#[test]
fn reopening_a_journal_with_a_tampered_backup_path_is_rejected() {
    let world = world();
    let index = replacing_step(&world);
    let dir = TempDir::new("journal-backup-open");
    let user_file = dir.path().join("user.md");
    fs::write(&user_file, b"user data").unwrap();
    let path = dir.path().join("journal.json");

    let good = journal(&world);
    let mut store = FileJournalStore::new(path.clone());
    store.store(&good).unwrap();
    assert_eq!(
        DurableJournal::open(FileJournalStore::new(path.clone())).unwrap().unwrap().journal(),
        &good
    );

    let mut foreign = good.clone();
    foreign.steps[index].backup_rel = Some("user.md".to_string());
    let mut other_namespace = good.clone();
    other_namespace.steps[index].backup_rel = Some("backup/other.bin".to_string());
    let mut no_old = good.clone();
    no_old.steps[index].old = None;
    for (name, tampered) in [
        ("foreign path", foreign),
        ("other namespace", other_namespace),
        ("backup without old", no_old),
    ] {
        // Write the bytes directly: `store` would refuse an invalid journal.
        fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        let loaded = FileJournalStore::new(path.clone()).load();
        assert_eq!(code(loaded), ErrorCode::JournalInvalid, "{name}");
        let opened = DurableJournal::open(FileJournalStore::new(path.clone()));
        assert_eq!(code(opened), ErrorCode::JournalInvalid, "{name}");
        assert_eq!(fs::read(&user_file).unwrap(), b"user data", "{name}");
    }
}

#[test]
fn fs_disk_view_observes_identity_without_following_links() {
    let dir = TempDir::new("disk");
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("sub").join("file.bin"), b"content").unwrap();
    let view = FsDiskView::new(dir.path());
    assert_eq!(
        view.observe("sub/file.bin").unwrap(),
        Observed::File(FileIdentity::of(b"content"))
    );
    assert_eq!(view.observe("sub/none.bin").unwrap(), Observed::Missing);
    assert!(view.observe("../outside").is_err());
    assert!(view.observe("/etc/passwd").is_err());
    assert_eq!(code(view.observe("sub")), ErrorCode::InstallModified);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.path().join("sub").join("file.bin"), dir.path().join("link")).unwrap();
        assert_eq!(code(view.observe("link")), ErrorCode::InstallModified);
    }
}

/// Needs permission to create directory symlinks: always available on Unix;
/// on Windows it requires Developer Mode or elevation, so the test is skipped
/// there when creation fails (junction/reparse handling is then unverified).
#[test]
fn fs_disk_view_rejects_linked_ancestor_directories() {
    let dir = TempDir::new("disk-link-root");
    let outside = TempDir::new("disk-link-outside");
    fs::create_dir(outside.path().join("shell-integration")).unwrap();
    fs::write(outside.path().join("shell-integration").join("file.bin"), b"content").unwrap();
    let link = dir.path().join("linked");
    #[cfg(unix)]
    let created = std::os::unix::fs::symlink(outside.path().join("shell-integration"), &link).is_ok();
    #[cfg(windows)]
    let created = std::os::windows::fs::symlink_dir(outside.path().join("shell-integration"), &link).is_ok();
    #[cfg(not(any(unix, windows)))]
    let created = false;
    if !created {
        eprintln!("skipped: directory symlinks cannot be created in this environment");
        return;
    }
    let view = FsDiskView::new(dir.path());
    assert!(view.observe("linked/file.bin").is_err());
    assert_eq!(code(view.observe("linked/file.bin")), ErrorCode::InstallModified);
}

// ------------------------------------------------------ U-12 recovery

fn decide(world: &World, journal: &UpdateJournal, disk: &dyn DiskView) -> RecoveryDecision {
    decide_recovery(Some(journal), &world.install_id, disk).unwrap()
}

#[test]
fn recovery_without_a_journal_does_nothing() {
    let world = world();
    let decision = decide_recovery(None, &world.install_id, &world.old_disk()).unwrap();
    assert_eq!(decision, RecoveryDecision::NoJournal);
}

#[test]
fn before_applying_the_old_version_is_kept_and_never_reapplied() {
    let world = world();
    for phase in [Phase::Prepared, Phase::ExitAuthorized, Phase::RolledBack] {
        let mut journal = journal(&world);
        journal.phase = phase;
        assert_eq!(decide(&world, &journal, &world.old_disk()), RecoveryDecision::KeepOld, "{phase:?}");
    }
    // A change that the journal never recorded is not guessed away.
    let mut journal = journal(&world);
    journal.phase = Phase::ExitAuthorized;
    let mut disk = world.old_disk();
    world.apply_step(&mut disk, 0);
    assert!(matches!(
        decide(&world, &journal, &disk),
        RecoveryDecision::RecoveryRequired(_)
    ));
}

#[test]
fn every_interruption_point_during_apply_rolls_back_to_the_old_version() {
    let world = world();
    let total = world.plan.steps.len();
    for applied in 0..=total {
        let mut disk = world.old_disk();
        for index in 0..applied {
            world.apply_step(&mut disk, index);
        }
        for phase in [Phase::Applying, Phase::RollingBack] {
            let mut journal = journal(&world);
            journal.phase = phase;
            // The journal may lag behind the disk by one step; the disk wins.
            for index in 0..applied.saturating_sub(1) {
                journal.steps[index].state = StepState::Applied;
            }
            let decision = decide(&world, &journal, &disk);
            if applied == 0 {
                assert_eq!(decision, RecoveryDecision::KeepOld);
                continue;
            }
            let (actions, require_child_exit) = match decision {
                RecoveryDecision::RollBack {
                    actions,
                    require_child_exit,
                } => (actions, require_child_exit),
                other => panic!("expected a rollback after {applied} applied steps, got {other:?}"),
            };
            assert!(!require_child_exit);
            assert_eq!(actions.len(), applied);
            for (action, step) in actions.iter().zip(&world.plan.steps) {
                match (action, &step.old) {
                    (RollbackAction::RestoreFromBackup { rel_path, restore, backup_rel, .. }, Some(old)) => {
                        assert_eq!(rel_path, &step.rel_path);
                        assert_eq!(restore, old);
                        assert_eq!(Some(backup_rel), step.backup_rel.as_ref());
                    }
                    (RollbackAction::RemoveAdded { rel_path, expected }, None) => {
                        assert_eq!(rel_path, &step.rel_path);
                        assert_eq!(Some(expected), step.new.as_ref());
                    }
                    other => panic!("unexpected action {other:?}"),
                }
            }
        }
    }
}

#[test]
fn awaiting_health_without_commit_rolls_back_after_the_child_exits() {
    let world = world();
    let mut disk = world.old_disk();
    for index in 0..world.plan.steps.len() {
        world.apply_step(&mut disk, index);
    }
    let mut journal = journal(&world);
    journal.phase = Phase::AwaitingHealth;
    let RecoveryDecision::RollBack { actions, require_child_exit } = decide(&world, &journal, &disk) else {
        panic!("expected a rollback");
    };
    assert!(require_child_exit);
    assert_eq!(actions.len(), world.plan.steps.len());
}

#[test]
fn unknown_content_or_damaged_backups_stop_recovery_without_guessing() {
    let world = world();
    let mut journal = journal(&world);
    journal.phase = Phase::Applying;

    // A recorded file now holds content that is neither old nor new.
    let mut foreign = world.old_disk();
    foreign.set("hane.exe", b"neither old nor new");
    assert!(matches!(decide(&world, &journal, &foreign), RecoveryDecision::RecoveryRequired(_)));

    // The new file is in place but its backup is gone or damaged.
    let last = world.plan.steps.len() - 1;
    assert_eq!(world.plan.steps[last].rel_path, "hane.exe");
    let backup = world.plan.steps[last].backup_rel.clone().unwrap();
    for damage in [None, Some(&b"damaged"[..])] {
        let mut disk = world.old_disk();
        world.apply_step(&mut disk, last);
        match damage {
            None => disk.remove(&backup),
            Some(bytes) => disk.set(&backup, bytes),
        }
        assert!(
            matches!(decide(&world, &journal, &disk), RecoveryDecision::RecoveryRequired(_)),
            "{damage:?}"
        );
    }
}

#[test]
fn a_stop_between_move_aside_and_link_in_restores_the_backup() {
    // Windows displaces the verified file before linking the new one in. A stop
    // in between leaves the pathname absent; the verified backup restores it.
    let world = world();
    let last = world.plan.steps.len() - 1;
    let step = &world.plan.steps[last];
    assert_eq!(step.rel_path, "hane.exe");
    let backup = step.backup_rel.clone().unwrap();
    for phase in [Phase::Applying, Phase::AwaitingHealth, Phase::RollingBack] {
        let mut journal = journal(&world);
        journal.phase = phase;
        journal.steps[last].state = StepState::Intent;
        let mut disk = world.old_disk();
        disk.remove("hane.exe");
        let RecoveryDecision::RollBack { actions, .. } = decide(&world, &journal, &disk) else {
            panic!("a missing file with an intact backup must roll back ({phase:?})");
        };
        assert_eq!(
            actions,
            vec![RollbackAction::RestoreFromBackup {
                rel_path: "hane.exe".to_string(),
                backup_rel: backup.clone(),
                restore: step.old.clone().unwrap(),
                expected_current: None,
            }]
        );

        // A missing or damaged backup is never guessed at.
        for damage in [None, Some(&b"damaged"[..])] {
            let mut broken = disk.clone();
            match damage {
                None => broken.remove(&backup),
                Some(bytes) => broken.set(&backup, bytes),
            }
            assert!(matches!(
                decide(&world, &journal, &broken),
                RecoveryDecision::RecoveryRequired(_)
            ));
        }

        // Third-party content at the path is still protected.
        let mut foreign = disk.clone();
        foreign.set("hane.exe", b"neither old nor new");
        assert!(matches!(
            decide(&world, &journal, &foreign),
            RecoveryDecision::RecoveryRequired(_)
        ));
    }
}

#[test]
fn a_durable_commit_is_kept_and_never_rolled_back_by_guesswork() {
    let world = world();
    let mut disk = world.old_disk();
    for index in 0..world.plan.steps.len() {
        world.apply_step(&mut disk, index);
    }
    let mut journal = journal(&world);
    journal.phase = Phase::Committed;
    assert_eq!(decide(&world, &journal, &disk), RecoveryDecision::KeepNew);

    // After commit a divergent file is reported, not reverted.
    disk.set("hane.exe", &world.old_files["hane.exe"]);
    assert!(matches!(decide(&world, &journal, &disk), RecoveryDecision::RecoveryRequired(_)));
}

#[test]
fn foreign_installs_and_failed_recoveries_require_manual_action() {
    let world = world();
    let mut journal = journal(&world);
    journal.phase = Phase::Prepared;
    let other = InstallId::derive(&InstallFacts {
        os: "windows",
        volume_id: "vol-9",
        install_path: "D:/Hane",
        owner: "S-1-5-21-1",
    })
    .unwrap();
    let decision = decide_recovery(Some(&journal), &other, &world.old_disk()).unwrap();
    assert!(matches!(decision, RecoveryDecision::RecoveryRequired(_)));

    journal.phase = Phase::RecoveryRequired;
    assert!(matches!(
        decide(&world, &journal, &world.old_disk()),
        RecoveryDecision::RecoveryRequired(_)
    ));
}

struct BrokenDisk;

impl DiskView for BrokenDisk {
    fn observe(&self, _rel_path: &str) -> Result<Observed> {
        Err(crate::error::UpdateError::new(ErrorCode::Io, "disk unreadable"))
    }
}

#[test]
fn an_unreadable_disk_is_an_error_not_a_decision() {
    let world = world();
    let mut journal = journal(&world);
    journal.phase = Phase::Applying;
    let result = decide_recovery(Some(&journal), &world.install_id, &BrokenDisk);
    assert_eq!(code(result), ErrorCode::Io);
}

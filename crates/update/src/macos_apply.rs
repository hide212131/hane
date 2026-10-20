//! macOS bundle apply path of the helper (Issue #510, U4 increment 4).
//!
//! Everything here is OS-neutral so it can be tested anywhere. Each OS fact the
//! swap depends on (volume kind, `RENAME_SWAP` support and call, the bundle
//! check) comes from a [`BundleHost`]; `platform::macos::MacHost` is the only
//! real one and it is compiled on macOS only.
//!
//! Trust comes from the Ed25519 update key alone: the catalog and the
//! inventory inside the bundle are signed with it. No Apple Developer ID, Team
//! ID or notarization is required, and none of Apple's protections is turned
//! off (no quarantine removal, no re-signing, no Gatekeeper bypass).
//!
//! The helper accepts no path, command or environment. Like the Windows path it
//! reads only `Coordination::transaction_dir(<its own transaction>)/candidate/`
//! and derives everything else itself:
//!
//! 1. the current bundle: fixed Bundle ID, Info.plist version / source SHA /
//!    updater protocol, and every file matching the signed inventory it carries,
//! 2. the candidate catalog and signature (verified again, bound to the
//!    requested candidate id) and the archive hash, inspection (including the
//!    signed inventory) and extraction into the transaction's own `stage/`,
//! 3. the staged candidate bundle: the same Info.plist facts, now also bound to
//!    the signed catalog, and the same signed-inventory check,
//! 4. that the install bundle and the staged candidate are on the same local,
//!    writable APFS volume and that this volume really swaps (a probe swap of
//!    two scratch directories between the two parents).
//!
//! Only then is the journal created. It has one step, `Hane.app`, whose old and
//! new identities are the bundle identities ([`bundle_identity`]); the staged
//! candidate's location doubles as the backup slot (`backup/Hane.app`). The
//! intent is durable before the single `RENAME_SWAP`-style exchange, and the
//! result is observed and made durable afterwards. The old bundle is never
//! deleted first and never overwritten: after the exchange it simply sits in
//! the slot, which is what a rollback exchanges back.
//!
//! Recovery reuses the journal-versus-disk engine of [`crate::apply`] through
//! [`BundleOps`]: the direction is decided from the bundle identities actually
//! on disk (live and slot), never from the journal alone, so an interruption at
//! any boundary can neither swap twice nor guess. A bundle that is neither the
//! recorded old nor new identity, a damaged or missing backup, or a journal that
//! does not match ends as `RecoveryRequired` with nothing written.
//!
//! Not verified here: the real `renamex_np` call and the power-loss durability
//! of APFS. They need a real macOS volume and a real two-version candidate
//! (U2 / U5 evidence).

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::apply::{self, FileOps, RecoveryOutcome};
use crate::archive::{ArchiveLimits, stage_archive};
use crate::catalog::{VerifyContext, verify_catalog};
use crate::clock::Clock;
use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{FileSeenStore, hash_regular_file, hash_regular_file_under, read_limited};
use crate::health::{HealthLauncher, LaunchFailure, LaunchRequest, conclude_with_child};
use crate::identity::{
    ApplyPlan, FileIdentity, InstallId, PlanStep, ReleaseIdentity, SignedInventory, TransactionId,
};
use crate::journal::{
    DiskView, DurableJournal, FileJournalStore, JournalStore, Observed, Phase, StepState,
    UpdateJournal, identity_matches,
};
use crate::limits::{
    JOURNAL_RESERVE_BYTES, MAX_CATALOG_BYTES, MAX_INVENTORY_BYTES, MAX_SIGNATURE_BYTES,
};
use crate::model::{
    MACOS_BUNDLE_ID, MACOS_INVENTORY_REL, MACOS_INVENTORY_SIGNATURE_REL, Target, UPDATER_PROTOCOL,
};
use crate::platform::Placement;
use crate::platform::lease::Coordination;
use crate::trust::{SeenStore, TrustPolicy};
use crate::util::is_lower_hex;
use crate::windows_apply::{
    CANDIDATE_ARCHIVE, CANDIDATE_CATALOG, CANDIDATE_DIR, CANDIDATE_SIGNATURE, NoChild,
    settled_result, valid_candidate_id,
};

/// The journal's one step and its backup path. They are names inside the
/// journal, not paths: the live bundle is `Placement::install_root` and the
/// backup is [`slot_path`].
pub const BUNDLE_REL: &str = "Hane.app";
pub const BACKUP_REL: &str = "backup/Hane.app";

const MAX_PLIST_BYTES: u64 = 256 << 10;
const STAGE_DIR: &str = "stage";
const STAGED_APP: [&str; 2] = ["hane-macos-arm64", "Hane.app"];
const MAIN_EXE: &str = "Contents/MacOS/hane";
/// The updater helper inside the bundle, relative to `Hane.app`.
pub const HELPER_REL: &str = "Contents/Helpers/hane-updater";
/// The files a complete bundle must contain. The identity itself covers every
/// entry under `Hane.app`; `CodeResources` is only a static manifest and does not
/// vouch for the bytes actually on disk.
const IDENTITY_FILES: [&str; 4] = [
    "Contents/Info.plist",
    MAIN_EXE,
    HELPER_REL,
    "Contents/_CodeSignature/CodeResources",
];

// ------------------------------------------------------------- the OS seam

/// What the volume query reports about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeFacts {
    /// Equal ids mean the same volume.
    pub id: String,
    pub apfs: bool,
    pub local: bool,
    pub read_only: bool,
}

/// OS-specific operations behind the bundle swap. Implementations must not
/// follow a fallback: no two-step rename, no quarantine removal, no ad-hoc
/// re-signing, no Gatekeeper bypass.
pub trait BundleHost {
    fn volume(&self, path: &Path) -> Result<VolumeFacts>;
    /// Bytes that can still be allocated on the volume of `path`.
    fn available_bytes(&self, path: &Path) -> Result<u64>;
    /// True only if an exchange of two scratch directories created in
    /// `first_dir` and `second_dir` really worked. They are the parents the
    /// real exchange will touch. The scratch directories are removed again.
    fn supports_swap(&self, first_dir: &Path, second_dir: &Path) -> Result<bool>;
    /// Atomically exchanges two existing paths on one volume. It either did
    /// exchange them or changed nothing.
    fn swap(&self, first: &Path, second: &Path) -> Result<()>;
    /// Flushes the parent directories of both exchanged paths to stable
    /// storage, so the exchanged directory entries survive a power loss. An
    /// error means the exchange is not known to be durable.
    fn sync_swap(&self, first: &Path, second: &Path) -> Result<()>;
    /// Verification of the complete bundle against the Ed25519-signed
    /// inventory it carries ([`verify_bundle_inventory`]). It is the only
    /// content check: no Apple identity, notarization or Gatekeeper verdict is
    /// asked for, and none is bypassed.
    fn verify_bundle_inventory(&self, bundle: &Path, policy: &TrustPolicy) -> Result<()>;
    /// The relocation mark stored on the bundle directory itself (an extended
    /// attribute, which follows the directory through a rename or a move), or
    /// `None` if there is none. Any other failure is an error.
    fn bundle_mark(&self, bundle: &Path) -> Result<Option<Vec<u8>>>;
    /// Durably stores the relocation mark on the bundle directory. The mark is
    /// not part of the sealed contents, so signatures and [`bundle_identity`]
    /// are unaffected.
    fn set_bundle_mark(&self, bundle: &Path, mark: &[u8]) -> Result<()>;
    /// Flushes the bundle directory's metadata (the mark's extended attribute)
    /// to stable storage. A successful `set_bundle_mark` alone does not survive
    /// a power loss; the journal and the exchange must not begin before this
    /// succeeds.
    fn sync_bundle_mark(&self, bundle: &Path) -> Result<()>;
    /// Removes the relocation mark; a bundle without one is not an error.
    fn clear_bundle_mark(&self, bundle: &Path) -> Result<()>;
}

/// Written to the staged candidate before the exchange, so that the new bundle
/// names the install it was swapped into wherever it is later launched from.
/// The `InstallId` is derived from the bundle's path; a bundle moved (Finder)
/// between the exchange and the identity check would otherwise start under a
/// different `InstallId` and never see the pending journal of this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelocationMark {
    pub install_id: InstallId,
    pub transaction: TransactionId,
    pub coordination_parent: PathBuf,
}

const MARK_MAGIC: &str = "hane-relocation-v1";

impl RelocationMark {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let parent = crate::identity::install_path_text(&self.coordination_parent)?;
        if parent.contains('\n') {
            return Err(UpdateError::new(
                ErrorCode::UnsupportedInstall,
                "the coordination parent cannot be recorded",
            ));
        }
        Ok(format!(
            "{MARK_MAGIC}\n{}\n{}\n{parent}\n",
            self.install_id.as_str(),
            self.transaction.as_str()
        )
        .into_bytes())
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let damaged = || {
            UpdateError::new(ErrorCode::InstallModified, "the bundle relocation mark is damaged")
        };
        let text = std::str::from_utf8(bytes).map_err(|_| damaged())?;
        let mut lines = text.strip_suffix('\n').ok_or_else(damaged)?.split('\n');
        if lines.next() != Some(MARK_MAGIC) {
            return Err(damaged());
        }
        let install_id = InstallId::parse(lines.next().ok_or_else(damaged)?).map_err(|_| damaged())?;
        let transaction =
            TransactionId::parse(lines.next().ok_or_else(damaged)?).map_err(|_| damaged())?;
        let parent = PathBuf::from(lines.next().ok_or_else(damaged)?);
        if lines.next().is_some() || !parent.is_absolute() {
            return Err(damaged());
        }
        Ok(Self {
            install_id,
            transaction,
            coordination_parent: parent,
        })
    }
}

/// Startup check of a bundle carrying a relocation `mark`, given the
/// `InstallId` derived from where it runs now. Where the bundle still is the
/// marked install, the ordinary gate (same `InstallId`) handles its journal.
/// Where it runs under another `InstallId` (it was moved), the journal of the
/// install it was swapped into is consulted directly: anything but a durably
/// `Committed` one stops (`RolledBack` included), so a moved bundle can never bypass a
/// pending transaction. Unverifiable state is an error (fail closed).
///
/// The parent directory may have moved together with the bundle, so the
/// recorded coordination parent can be gone. The transaction bound by the mark
/// is then looked up under `current_parent`; if it cannot be found and verified
/// in either place the start is refused, never treated as "no journal".
pub fn check_relocation(
    mark: Option<&[u8]>,
    current: &InstallId,
    current_parent: &Path,
    bundle: &Path,
) -> Result<()> {
    let Some(bytes) = mark else {
        return Ok(());
    };
    let mark = RelocationMark::parse(bytes)?;
    if &mark.install_id == current {
        return Ok(());
    }
    // `UnsupportedInstall` would let startup treat the bundle as unmanaged, so
    // no failure here may keep that code.
    let stop = |err: UpdateError| {
        if matches!(err.code, ErrorCode::UnsupportedInstall | ErrorCode::TargetUnsupported) {
            UpdateError::new(ErrorCode::InstallModified, err.to_string())
        } else {
            err
        }
    };
    let unverifiable = || {
        UpdateError::new(
            ErrorCode::InstallModified,
            "the journal of the transaction bound to the relocation mark cannot be found",
        )
    };
    let mut journal = None;
    for parent in [mark.coordination_parent.as_path(), current_parent] {
        let Some(coordination) =
            Coordination::open_existing(parent, &mark.install_id).map_err(stop)?
        else {
            continue;
        };
        journal = FileJournalStore::new(coordination.journal_path(&mark.transaction))
            .load()
            .map_err(stop)?;
        if journal.is_some() {
            break;
        }
    }
    let Some(journal) = journal else {
        return Err(unverifiable());
    };
    if journal.transaction_id != mark.transaction || journal.install_id != mark.install_id {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the journal of the original install belongs to another transaction or install",
        ));
    }
    match journal.phase {
        // The mark can outlive the commit, so the bundle that now runs must be
        // exactly the one the committed journal recorded for `Hane.app`.
        Phase::Committed => require_committed_bundle(&journal, bundle),
        // A rolled-back transaction was rejected by the health check, so the
        // marked bundle must still pass its recovery gate; it never authorizes
        // a normal start from another location.
        Phase::RolledBack => Err(UpdateError::new(
            ErrorCode::InstallModified,
            "the bundle was moved after its update was rolled back at the original location",
        )),
        _ => Err(UpdateError::new(
            ErrorCode::InstallModified,
            "the bundle was moved while its update is still pending at the original location",
        )),
    }
}

/// A moved bundle may start on the strength of a `Committed` journal only if
/// the journal has exactly one `Hane.app` step and the bundle at `bundle` is
/// exactly that step's new identity. Anything else (no step, several steps, a
/// different or unreadable bundle) fails closed.
fn require_committed_bundle(journal: &UpdateJournal, bundle: &Path) -> Result<()> {
    let refuse = |message: &'static str| UpdateError::new(ErrorCode::InstallModified, message);
    let mut steps = journal.steps.iter().filter(|step| step.rel_path == BUNDLE_REL);
    let (Some(step), None) = (steps.next(), steps.next()) else {
        return Err(refuse(
            "the committed journal does not name the bundle in exactly one step",
        ));
    };
    let Some(expected) = &step.new else {
        return Err(refuse("the committed journal records no new bundle identity"));
    };
    match bundle_identity(bundle) {
        Ok(Observed::File(actual)) if &actual == expected => Ok(()),
        Ok(_) => Err(refuse(
            "the moved bundle differs from the bundle the committed journal recorded",
        )),
        Err(_) => Err(refuse("the identity of the moved bundle cannot be verified")),
    }
}

/// Startup check for a bundle on a volume this updater does not support (not
/// local APFS, or read-only). Such a placement is normally unmanaged, but a
/// bundle carrying a relocation mark may have been moved off the volume in the
/// middle of an update. The original transaction is therefore inspected first;
/// a pending or unverifiable one stops instead of allowing normal editing.
/// No volume UUID exists to derive the current `InstallId`, so a sentinel that
/// never equals a marked (APFS) install stands in for it.
pub fn check_relocation_on_unsupported_volume(
    mark: Option<&[u8]>,
    current_parent: &Path,
    bundle: &Path,
) -> Result<()> {
    let sentinel = InstallId::derive(&crate::identity::InstallFacts {
        os: "macos",
        volume_id: "unsupported-volume",
        install_path: "unsupported-volume",
        owner: "unsupported-volume",
    })?;
    check_relocation(mark, &sentinel, current_parent, bundle)
}

/// Every regular file below `dir` with its size and hash, keyed by the path
/// relative to the bundle. The signed inventory lists no link, so a symlink,
/// any other kind of entry or a non-UTF-8 name is an error.
fn collect_bundle_files(
    dir: &Path,
    prefix: &str,
    out: &mut BTreeMap<String, (u64, String)>,
) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            UpdateError::new(
                ErrorCode::InstallModified,
                format!("a name in {} is not UTF-8", dir.display()),
            )
        })?;
        let path = dir.join(&name);
        let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let kind = fs::symlink_metadata(&path)?.file_type();
        if kind.is_dir() {
            collect_bundle_files(&path, &rel, out)?;
        } else if kind.is_file() {
            let hashed = hash_regular_file(&path)?.ok_or_else(|| {
                UpdateError::new(
                    ErrorCode::InstallModified,
                    format!("{} vanished while being read", path.display()),
                )
            })?;
            out.insert(rel, hashed);
        } else {
            return Err(UpdateError::new(
                ErrorCode::InstallModified,
                format!("{} is not a plain file or directory", path.display()),
            ));
        }
    }
    Ok(())
}

/// Verifies `bundle` against the inventory it carries, signed with the
/// embedded Ed25519 update key: the signature, the target / protocol / layout,
/// the version and source SHA against the Info.plist, and that the files on
/// disk are exactly the listed ones (no missing, extra or changed file). An
/// unconfigured or wrong key, a bad signature or any difference fails closed.
pub fn verify_bundle_inventory(bundle: &Path, policy: &TrustPolicy) -> Result<SignedInventory> {
    ensure_real_dir(bundle)?;
    let missing = |what: &str| UpdateError::new(ErrorCode::NotFound, format!("{what} is missing"));
    let raw = read_limited(&bundle.join(MACOS_INVENTORY_REL), MAX_INVENTORY_BYTES)?
        .ok_or_else(|| missing("the bundle inventory"))?;
    let signature = read_limited(&bundle.join(MACOS_INVENTORY_SIGNATURE_REL), MAX_SIGNATURE_BYTES)?
        .ok_or_else(|| missing("the bundle inventory signature"))?;
    let signed = SignedInventory::verify(&raw, &signature, policy)?;
    let inventory = signed.inventory();
    let mismatch = |what: &str| {
        UpdateError::new(
            ErrorCode::IdentityMismatch,
            format!("the bundle inventory {what} does not describe this bundle"),
        )
    };
    if inventory.target != Target::MacosArm64.triple() {
        return Err(mismatch("target"));
    }
    if inventory.updater_protocol != UPDATER_PROTOCOL {
        return Err(UpdateError::new(
            ErrorCode::ProtocolUnsupported,
            "the bundle inventory updater protocol is not supported",
        ));
    }
    if inventory.layout_revision != Target::MacosArm64.layout_revision() {
        return Err(mismatch("layout revision"));
    }
    let facts = read_bundle_facts(bundle)?;
    if inventory.version != facts.short_version {
        return Err(mismatch("version"));
    }
    if inventory.source_sha != facts.source_commit {
        return Err(mismatch("source SHA"));
    }

    let mut on_disk = BTreeMap::new();
    collect_bundle_files(bundle, "", &mut on_disk)?;
    on_disk.remove(MACOS_INVENTORY_REL);
    on_disk.remove(MACOS_INVENTORY_SIGNATURE_REL);
    let changed = || {
        UpdateError::new(
            ErrorCode::InstallModified,
            "the bundle files differ from the signed inventory",
        )
    };
    if on_disk.len() != inventory.files.len() {
        return Err(changed());
    }
    for file in &inventory.files {
        match on_disk.get(&file.path) {
            Some((size, sha256)) if *size == file.size && *sha256 == file.sha256 => {}
            _ => return Err(changed()),
        }
    }
    Ok(signed)
}

// -------------------------------------------------------------- the bundle

fn value_after<'a>(plist: &'a str, key: &str, tag: &str) -> Option<&'a str> {
    let marker = format!("<key>{key}</key>");
    let rest = plist[plist.find(&marker)? + marker.len()..].trim_start();
    let body = rest.strip_prefix(format!("<{tag}>").as_str())?;
    let end = body.find(format!("</{tag}>").as_str())?;
    Some(body[..end].trim())
}

/// Reads a string value of an XML Info.plist; anything else fails closed.
pub fn plist_string(plist: &str, key: &str) -> Option<String> {
    value_after(plist, key, "string").map(str::to_owned)
}

/// Reads an integer value of an XML Info.plist; anything else fails closed.
pub fn plist_integer(plist: &str, key: &str) -> Option<u64> {
    value_after(plist, key, "integer")?.parse().ok()
}

/// What the Info.plist of a Hane bundle declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleFacts {
    pub bundle_id: String,
    pub short_version: String,
    pub build_version: String,
    pub source_commit: String,
    pub updater_protocol: u64,
}

fn ensure_real_dir(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(UpdateError::new(
            ErrorCode::InstallModified,
            format!("{} is not a real directory", path.display()),
        ));
    }
    Ok(())
}

pub fn read_bundle_facts(bundle: &Path) -> Result<BundleFacts> {
    let damaged = |what: &str| {
        UpdateError::new(ErrorCode::InstallModified, format!("the bundle Info.plist {what}"))
    };
    ensure_real_dir(bundle)?;
    hash_regular_file_under(bundle, &["Contents", "Info.plist"])?
        .ok_or_else(|| damaged("is missing"))?;
    let raw = read_limited(&bundle.join("Contents").join("Info.plist"), MAX_PLIST_BYTES)?
        .ok_or_else(|| damaged("is missing"))?;
    let plist = String::from_utf8(raw).map_err(|_| damaged("is not XML"))?;
    let text = |key: &str| plist_string(&plist, key).ok_or_else(|| damaged("lacks a required key"));
    Ok(BundleFacts {
        bundle_id: text("CFBundleIdentifier")?,
        short_version: text("CFBundleShortVersionString")?,
        build_version: text("CFBundleVersion")?,
        source_commit: text("HaneSourceCommit")?,
        updater_protocol: plist_integer(&plist, "HaneUpdaterProtocol")
            .ok_or_else(|| damaged("lacks a required key"))?,
    })
}

/// The facts both sides of an update must have, independent of any catalog.
fn check_bundle_facts(facts: &BundleFacts, what: &str) -> Result<()> {
    if facts.bundle_id != MACOS_BUNDLE_ID {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            format!("{what} does not have Hane's bundle identifier"),
        ));
    }
    if facts.updater_protocol != UPDATER_PROTOCOL {
        return Err(UpdateError::new(
            ErrorCode::ProtocolUnsupported,
            format!("{what} does not speak the supported updater protocol"),
        ));
    }
    if !is_lower_hex(&facts.source_commit, 40) || facts.short_version != facts.build_version {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            format!("{what} has inconsistent version facts"),
        ));
    }
    Ok(())
}

/// Identity of a whole bundle: the size and hash of its seal files, folded into
/// one [`FileIdentity`]. `Missing` only when nothing is at `bundle`; a link, a
/// non-directory or an incomplete bundle is an error, never a guess.
pub fn bundle_identity(bundle: &Path) -> Result<Observed> {
    match fs::symlink_metadata(bundle) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(UpdateError::new(
                ErrorCode::InstallModified,
                format!("{} is not a real bundle directory", bundle.display()),
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Observed::Missing),
        Err(err) => return Err(err.into()),
    }
    for rel in IDENTITY_FILES {
        let parts: Vec<&str> = rel.split('/').collect();
        hash_regular_file_under(bundle, &parts)?.ok_or_else(|| {
            UpdateError::new(ErrorCode::InstallModified, format!("the bundle lacks {rel}"))
        })?;
    }
    let mut manifest = String::new();
    collect_identity_entries(bundle, "", &mut manifest)?;
    Ok(Observed::File(FileIdentity::of(manifest.as_bytes())))
}

/// Appends one unambiguous record per entry below `dir`, in byte order of the
/// names: the type, the length-prefixed relative path and then the file size
/// and hash or the symlink target. Symlinks are recorded, never followed; any
/// other kind of entry, a non-UTF-8 name or a read failure is an error.
/// Timestamps and other volatile metadata are not part of the identity.
fn collect_identity_entries(dir: &Path, prefix: &str, out: &mut String) -> Result<()> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            UpdateError::new(
                ErrorCode::InstallModified,
                format!("a name in {} is not UTF-8", dir.display()),
            )
        })?;
        names.push(name);
    }
    names.sort();
    for name in names {
        let path = dir.join(&name);
        let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let meta = fs::symlink_metadata(&path)?;
        let kind = meta.file_type();
        if kind.is_symlink() {
            let target = fs::read_link(&path)?;
            let target = target.to_str().ok_or_else(|| {
                UpdateError::new(
                    ErrorCode::InstallModified,
                    format!("the target of {} is not UTF-8", path.display()),
                )
            })?;
            out.push_str(&format!("L {} {rel} {} {target}\n", rel.len(), target.len()));
        } else if kind.is_dir() {
            out.push_str(&format!("D {} {rel}\n", rel.len()));
            collect_identity_entries(&path, &rel, out)?;
        } else if kind.is_file() {
            let (size, sha256) = hash_regular_file(&path)?.ok_or_else(|| {
                UpdateError::new(
                    ErrorCode::InstallModified,
                    format!("{} vanished while being read", path.display()),
                )
            })?;
            out.push_str(&format!("F {} {rel} {size} {sha256}\n", rel.len()));
        } else {
            return Err(UpdateError::new(
                ErrorCode::InstallModified,
                format!("{} is neither a file, a directory nor a symlink", path.display()),
            ));
        }
    }
    Ok(())
}

/// Where the candidate bundle is staged, and so where the old bundle rests
/// after the exchange.
pub fn slot_path(transaction_dir: &Path) -> PathBuf {
    let mut path = transaction_dir.join(STAGE_DIR);
    for part in STAGED_APP {
        path.push(part);
    }
    path
}

/// The journal's view of the live bundle only (terminal receipts compare the
/// disk with `Hane.app`; nothing else is asked of it).
pub struct BundleView {
    live: PathBuf,
}

impl BundleView {
    pub fn new(live: &Path) -> Self {
        Self {
            live: live.to_path_buf(),
        }
    }
}

impl DiskView for BundleView {
    fn observe(&self, rel_path: &str) -> Result<Observed> {
        if rel_path == BUNDLE_REL {
            bundle_identity(&self.live)
        } else {
            Err(unknown_rel(rel_path))
        }
    }
}

fn unknown_rel(rel_path: &str) -> UpdateError {
    UpdateError::new(
        ErrorCode::JournalInvalid,
        format!("{rel_path} is not part of a bundle update"),
    )
}

/// The only mutation a bundle update has: exchanging the live bundle and the
/// slot. `restore` is the rollback exchange; nothing is ever copied, written or
/// removed.
pub struct BundleOps<'a> {
    host: &'a dyn BundleHost,
    live: PathBuf,
    slot: PathBuf,
    /// Fixed before the exchange; `None` for startup recovery, which has no
    /// earlier placement to compare with.
    guard: Option<PlacementGuard>,
    /// The bundle version taken from the signed candidate and the update-key
    /// policy; re-verified against the live bundle before the commit.
    commit_check: Option<CommitCheck>,
}

struct CommitCheck {
    version: String,
    policy: TrustPolicy,
}

/// Directory identity (device, inode) of the install parent and of the two
/// bundle directories, fixed before the exchange. An exchange keeps both
/// bundles' identities and only swaps which path names which, so the live and
/// slot paths must always name this pair, in either order. A bundle moved away
/// (Finder) or replaced by another directory is refused. The directories are
/// held open so their identities cannot be reused meanwhile.
#[cfg(unix)]
struct PlacementGuard {
    parent: PathBuf,
    parent_id: (u64, u64),
    live_id: (u64, u64),
    slot_id: (u64, u64),
    _pins: Vec<File>,
}

#[cfg(unix)]
impl PlacementGuard {
    fn dir_id(path: &Path, follow: bool) -> Result<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let meta = if follow { fs::metadata(path)? } else { fs::symlink_metadata(path)? };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(UpdateError::new(
                ErrorCode::InstallModified,
                format!("{} is not a real directory", path.display()),
            ));
        }
        Ok((meta.dev(), meta.ino()))
    }

    fn capture(live: &Path, slot: &Path) -> Result<Self> {
        let parent = live.parent().ok_or_else(|| {
            UpdateError::new(ErrorCode::UnsupportedInstall, "the bundle has no parent directory")
        })?;
        let pins = vec![File::open(parent)?, File::open(live)?, File::open(slot)?];
        Ok(Self {
            parent: parent.to_path_buf(),
            parent_id: Self::dir_id(parent, true)?,
            live_id: Self::dir_id(live, false)?,
            slot_id: Self::dir_id(slot, false)?,
            _pins: pins,
        })
    }

    fn verify(&self, live: &Path, slot: &Path) -> Result<()> {
        let moved = || {
            UpdateError::new(
                ErrorCode::InstallModified,
                "the installed bundle was moved or replaced during the update",
            )
        };
        let parent = Self::dir_id(&self.parent, true).map_err(|_| moved())?;
        let now_live = Self::dir_id(live, false).map_err(|_| moved())?;
        let now_slot = Self::dir_id(slot, false).map_err(|_| moved())?;
        let in_place = now_live == self.live_id && now_slot == self.slot_id;
        let exchanged = now_live == self.slot_id && now_slot == self.live_id;
        if parent != self.parent_id || !(in_place || exchanged) {
            return Err(moved());
        }
        Ok(())
    }
}

#[cfg(not(unix))]
struct PlacementGuard;

#[cfg(not(unix))]
impl PlacementGuard {
    fn capture(_live: &Path, _slot: &Path) -> Result<Self> {
        Ok(Self)
    }

    fn verify(&self, _live: &Path, _slot: &Path) -> Result<()> {
        Ok(())
    }
}

impl<'a> BundleOps<'a> {
    pub fn new(host: &'a dyn BundleHost, live: &Path, slot: &Path) -> Self {
        Self {
            host,
            live: live.to_path_buf(),
            slot: slot.to_path_buf(),
            guard: None,
            commit_check: None,
        }
    }

    /// Fixes what the live bundle must be (version from the signed candidate,
    /// the signed inventory under the update key) when the update is about to
    /// be committed.
    pub fn with_commit_check(mut self, version: String, policy: TrustPolicy) -> Self {
        self.commit_check = Some(CommitCheck { version, policy });
        self
    }

    /// Like [`BundleOps::new`], with the placement of the live bundle, its
    /// parent and the slot fixed now. Every exchange, rollback, commit and
    /// recovery re-verifies it.
    pub fn pinned(host: &'a dyn BundleHost, live: &Path, slot: &Path) -> Result<Self> {
        let mut ops = Self::new(host, live, slot);
        ops.guard = Some(PlacementGuard::capture(live, slot)?);
        Ok(ops)
    }

    fn check_placement(&self) -> Result<()> {
        match &self.guard {
            Some(guard) => guard.verify(&self.live, &self.slot),
            None => Ok(()),
        }
    }

    fn exchange(&self) -> Result<()> {
        self.check_placement()?;
        self.host.swap(&self.live, &self.slot)?;
        // The journal must not advance on an exchange that is not durable. An
        // error here leaves recovery to settle from the journal and the disk.
        self.host.sync_swap(&self.live, &self.slot)
    }
}

impl DiskView for BundleOps<'_> {
    fn observe(&self, rel_path: &str) -> Result<Observed> {
        match rel_path {
            BUNDLE_REL => bundle_identity(&self.live),
            BACKUP_REL => bundle_identity(&self.slot),
            other => Err(unknown_rel(other)),
        }
    }
}

fn no_file_ops() -> UpdateError {
    UpdateError::new(
        ErrorCode::NotApplicable,
        "a bundle update only exchanges the bundle; it never writes files",
    )
}

impl FileOps for BundleOps<'_> {
    fn backup(&mut self, _rel_path: &str, _backup_rel: &str) -> Result<()> {
        Err(no_file_ops())
    }

    fn install_new(&mut self, _rel_path: &str, _expected: &FileIdentity) -> Result<()> {
        Err(no_file_ops())
    }

    fn remove(&mut self, _rel_path: &str) -> Result<()> {
        Err(no_file_ops())
    }

    fn verify_placement(&self) -> Result<()> {
        self.check_placement()
    }

    fn verify_commit_state(&self) -> Result<()> {
        let Some(check) = &self.commit_check else {
            return Ok(());
        };
        let facts = read_bundle_facts(&self.live)?;
        check_bundle_facts(&facts, "the live bundle")?;
        if facts.short_version != check.version {
            return Err(UpdateError::new(
                ErrorCode::IdentityMismatch,
                "the live bundle is not the signed candidate version",
            ));
        }
        require_verified(self.host, &self.live, &check.policy)
    }

    /// Rolls back by exchanging again. The slot must still hold exactly the
    /// recorded old bundle, and afterwards the live path must hold it.
    fn restore(&mut self, rel_path: &str, backup_rel: &str, expected: &FileIdentity) -> Result<()> {
        if rel_path != BUNDLE_REL || backup_rel != BACKUP_REL {
            return Err(unknown_rel(rel_path));
        }
        self.check_placement()?;
        if bundle_identity(&self.slot)? != Observed::File(expected.clone()) {
            return Err(UpdateError::new(
                ErrorCode::HashMismatch,
                "the backup bundle is missing or damaged",
            ));
        }
        self.exchange()?;
        if bundle_identity(&self.live)? != Observed::File(expected.clone()) {
            return Err(UpdateError::new(
                ErrorCode::HashMismatch,
                "the live bundle is not the old bundle after the rollback exchange",
            ));
        }
        Ok(())
    }
}

// ------------------------------------------------------------ verification

/// Everything the helper derives or holds when it applies a bundle.
pub struct BundleApplyInputs<'a> {
    pub coordination: &'a Coordination,
    pub transaction: &'a TransactionId,
    pub candidate_id: &'a str,
    pub placement: &'a Placement,
    pub policy: &'a TrustPolicy,
    pub clock: &'a dyn Clock,
    pub host: &'a dyn BundleHost,
    /// True only while the helper holds the exclusive `runtime` lease, which
    /// is the OS-observed proof that the parent process is gone.
    pub runtime_held: bool,
}

struct Verified {
    plan: ApplyPlan,
    slot: PathBuf,
}

fn require_local_apfs(host: &dyn BundleHost, path: &Path, what: &str) -> Result<VolumeFacts> {
    let volume = host.volume(path)?;
    if !volume.apfs || !volume.local {
        return Err(UpdateError::new(
            ErrorCode::UnsupportedInstall,
            format!("{what} is not on a local APFS volume"),
        ));
    }
    if volume.read_only {
        return Err(UpdateError::new(
            ErrorCode::UnsupportedInstall,
            format!("{what} is on a read-only volume"),
        ));
    }
    Ok(volume)
}

fn require_verified(host: &dyn BundleHost, bundle: &Path, policy: &TrustPolicy) -> Result<()> {
    host.verify_bundle_inventory(bundle, policy)
}

#[cfg(test)]
mod inventory_tests {
    use super::*;
    use crate::identity::{Inventory, InventoryFile};
    use crate::model::{INVENTORY_DOMAIN, INVENTORY_SCHEMA};
    use crate::testing::{TEST_KEY_ID, TEST_SOURCE_SHA, TempDir, sign, sign_with, test_policy};
    use crate::util::sha256_hex;

    const VERSION: &str = "0.26.0";

    fn plist() -> String {
        format!(
            "<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict>\n\
             <key>CFBundleIdentifier</key>\n<string>{MACOS_BUNDLE_ID}</string>\n\
             <key>CFBundleShortVersionString</key>\n<string>{VERSION}</string>\n\
             <key>CFBundleVersion</key>\n<string>{VERSION}</string>\n\
             <key>HaneSourceCommit</key>\n<string>{TEST_SOURCE_SHA}</string>\n\
             <key>HaneUpdaterProtocol</key>\n<integer>1</integer>\n\
             </dict></plist>\n"
        )
    }

    fn payload() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("Contents/Info.plist", plist().into_bytes()),
            ("Contents/MacOS/hane", b"hane".to_vec()),
            (HELPER_REL, b"updater".to_vec()),
        ]
    }

    /// Writes `files` plus a signed inventory over `listed` (by default the
    /// same files). No Apple signature, Team ID or notarization data exists in
    /// this bundle.
    fn write_signed_bundle(
        root: &Path,
        files: &[(&'static str, Vec<u8>)],
        listed: &[(&'static str, Vec<u8>)],
        signer_seed: u8,
        source_sha: &str,
    ) {
        for (rel, data) in files {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, data).unwrap();
        }
        let inventory = Inventory {
            schema: INVENTORY_SCHEMA,
            version: VERSION.to_owned(),
            target: Target::MacosArm64.triple().to_owned(),
            source_sha: source_sha.to_owned(),
            updater_protocol: UPDATER_PROTOCOL,
            layout_revision: Target::MacosArm64.layout_revision(),
            files: listed
                .iter()
                .map(|(path, data)| InventoryFile {
                    path: (*path).to_owned(),
                    size: data.len() as u64,
                    sha256: sha256_hex(data),
                })
                .collect(),
        };
        let raw = serde_json::to_vec(&inventory).unwrap();
        let signature = if signer_seed == 1 {
            sign(INVENTORY_DOMAIN, &raw)
        } else {
            sign_with(signer_seed, TEST_KEY_ID, INVENTORY_DOMAIN, &raw)
        };
        for (rel, data) in [
            (MACOS_INVENTORY_REL, raw),
            (MACOS_INVENTORY_SIGNATURE_REL, signature),
        ] {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, data).unwrap();
        }
    }

    fn bundle(dir: &TempDir) -> PathBuf {
        dir.path().join("Hane.app")
    }

    #[test]
    fn a_bundle_without_any_apple_identity_verifies_under_the_update_key() {
        let dir = TempDir::new("macos-inventory-ok");
        let files = payload();
        write_signed_bundle(&bundle(&dir), &files, &files, 1, TEST_SOURCE_SHA);
        let signed = verify_bundle_inventory(&bundle(&dir), &test_policy()).unwrap();
        assert!(signed.inventory().files.iter().any(|file| file.path == HELPER_REL));
    }

    #[test]
    fn an_unconfigured_or_foreign_update_key_is_refused() {
        let dir = TempDir::new("macos-inventory-key");
        let files = payload();
        write_signed_bundle(&bundle(&dir), &files, &files, 1, TEST_SOURCE_SHA);
        let err = verify_bundle_inventory(&bundle(&dir), &TrustPolicy::production()).unwrap_err();
        assert_eq!(err.code, ErrorCode::TrustUnconfigured);

        let other = TempDir::new("macos-inventory-forged");
        write_signed_bundle(&bundle(&other), &files, &files, 2, TEST_SOURCE_SHA);
        let err = verify_bundle_inventory(&bundle(&other), &test_policy()).unwrap_err();
        assert_eq!(err.code, ErrorCode::SignatureInvalid);
    }

    #[test]
    fn a_changed_extra_or_missing_file_is_refused() {
        let files = payload();

        let dir = TempDir::new("macos-inventory-changed");
        write_signed_bundle(&bundle(&dir), &files, &files, 1, TEST_SOURCE_SHA);
        fs::write(bundle(&dir).join(HELPER_REL), b"tampered").unwrap();
        let err = verify_bundle_inventory(&bundle(&dir), &test_policy()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);

        let dir = TempDir::new("macos-inventory-extra");
        write_signed_bundle(&bundle(&dir), &files, &files, 1, TEST_SOURCE_SHA);
        fs::write(bundle(&dir).join("Contents/Resources/extra.sh"), b"#!").unwrap();
        let err = verify_bundle_inventory(&bundle(&dir), &test_policy()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);

        let dir = TempDir::new("macos-inventory-missing");
        write_signed_bundle(&bundle(&dir), &files, &files, 1, TEST_SOURCE_SHA);
        fs::remove_file(bundle(&dir).join(MAIN_EXE)).unwrap();
        let err = verify_bundle_inventory(&bundle(&dir), &test_policy()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);
    }

    #[test]
    fn an_inventory_for_another_source_is_refused() {
        let dir = TempDir::new("macos-inventory-source");
        let files = payload();
        write_signed_bundle(&bundle(&dir), &files, &files, 1, &"a".repeat(40));
        let err = verify_bundle_inventory(&bundle(&dir), &test_policy()).unwrap_err();
        assert_eq!(err.code, ErrorCode::IdentityMismatch);
    }
}

/// Independently re-verifies placement, both bundles, the candidate and the
/// archive, stages the archive and builds the one-step plan. Nothing in the
/// install is changed.
fn verify_and_plan(inputs: &BundleApplyInputs<'_>) -> Result<Verified> {
    let coordination = inputs.coordination;
    let placement = inputs.placement;
    let host = inputs.host;
    if placement.target != Target::MacosArm64 {
        return Err(UpdateError::new(
            ErrorCode::UnsupportedInstall,
            "only the macOS arm64 bundle layout can be swapped",
        ));
    }
    if &placement.install_id != coordination.install_id() {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the install is not the one this coordination directory belongs to",
        ));
    }
    if !valid_candidate_id(inputs.candidate_id) {
        return Err(UpdateError::new(ErrorCode::IdentityMismatch, "malformed candidate id"));
    }
    let live = &placement.install_root;
    let install_volume = require_local_apfs(host, live, "the installed bundle")?;
    let current = read_bundle_facts(live)?;
    check_bundle_facts(&current, "the installed bundle")?;
    require_verified(host, live, inputs.policy)?;

    let txn_dir = coordination.transaction_dir(inputs.transaction);
    let candidate_dir = txn_dir.join(CANDIDATE_DIR);
    let missing = |what: &str| UpdateError::new(ErrorCode::NotFound, format!("{what} is missing"));
    let catalog = read_limited(&candidate_dir.join(CANDIDATE_CATALOG), MAX_CATALOG_BYTES)?
        .ok_or_else(|| missing("the candidate catalog"))?;
    let signature = read_limited(&candidate_dir.join(CANDIDATE_SIGNATURE), MAX_SIGNATURE_BYTES)?
        .ok_or_else(|| missing("the candidate signature"))?;
    // The floor is re-read now, after the runtime lease is held: a missing,
    // corrupt or unreadable receipt is a refusal, never "no floor".
    let seen = FileSeenStore::new(coordination.seen_catalog_path())
        .load()
        .map_err(|err| {
            UpdateError::new(
                ErrorCode::ReceiptUnwritable,
                format!("the catalog sequence receipt cannot be read: {err}"),
            )
        })?
        .ok_or_else(|| missing("the catalog sequence receipt"))?;
    let candidate = verify_catalog(
        &catalog,
        &signature,
        &VerifyContext {
            policy: inputs.policy,
            clock: inputs.clock,
            current_version: &current.short_version,
            target: Target::MacosArm64,
            seen: Some(&seen),
            expected_tag: None,
        },
    )?;
    if candidate.candidate_id().as_str() != inputs.candidate_id {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the staged candidate is not the requested one",
        ));
    }
    if !candidate.is_applicable() {
        return Err(UpdateError::new(
            ErrorCode::NotApplicable,
            "the candidate is not newer than the installed version",
        ));
    }

    let archive_path = candidate_dir.join(CANDIDATE_ARCHIVE);
    let entry = candidate.target_entry();
    let (size, sha256) =
        hash_regular_file(&archive_path)?.ok_or_else(|| missing("the candidate archive"))?;
    if size != entry.archive_size || sha256 != entry.archive_sha256 {
        return Err(UpdateError::new(
            ErrorCode::HashMismatch,
            "the staged archive differs from the signed catalog",
        ));
    }
    let stage = txn_dir.join(STAGE_DIR);
    match fs::create_dir(&stage) {
        Ok(()) => {}
        // An existing stage is accepted only if it is empty; the extractor
        // refuses anything else.
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err.into()),
    }
    let mut archive = File::open(&archive_path)?;
    stage_archive(&mut archive, &candidate, inputs.policy, &ArchiveLimits::default(), &stage)?;
    drop(archive);

    // The candidate is judged where it will be swapped in from, after it was
    // extracted by this process, never in the application's download area.
    let slot = slot_path(&txn_dir);
    let next = read_bundle_facts(&slot)?;
    check_bundle_facts(&next, "the candidate bundle")?;
    let version = candidate.version().to_string();
    if next.short_version != version || entry.bundle_version.as_deref() != Some(version.as_str()) {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the candidate bundle version differs from the signed catalog",
        ));
    }
    if next.source_commit != candidate.catalog().source_sha {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the candidate bundle was not built from the cataloged source",
        ));
    }
    require_verified(host, &slot, inputs.policy)?;

    // Same volume, and a volume that really swaps. A candidate somewhere else
    // is refused before anything is changed.
    let staged_volume = require_local_apfs(host, &slot, "the staged candidate")?;
    if staged_volume.id != install_volume.id {
        return Err(UpdateError::new(
            ErrorCode::UnsupportedInstall,
            "the staged candidate is not on the install volume",
        ));
    }
    let install_parent = live.parent().ok_or_else(|| {
        UpdateError::new(ErrorCode::UnsupportedInstall, "the bundle has no parent directory")
    })?;
    let slot_parent = slot.parent().ok_or_else(|| {
        UpdateError::new(ErrorCode::UnsupportedInstall, "the staged candidate has no parent")
    })?;
    if !host.supports_swap(install_parent, slot_parent)? {
        return Err(UpdateError::new(
            ErrorCode::UnsupportedInstall,
            "the install volume does not support swap renaming",
        ));
    }

    let (Observed::File(old), Observed::File(new)) =
        (bundle_identity(live)?, bundle_identity(&slot)?)
    else {
        return Err(UpdateError::new(
            ErrorCode::InstallModified,
            "a bundle to swap is missing",
        ));
    };
    if old == new {
        return Err(UpdateError::new(
            ErrorCode::NotApplicable,
            "the candidate bundle is the installed bundle",
        ));
    }
    let plan = ApplyPlan {
        install_id: coordination.install_id().clone(),
        transaction_id: inputs.transaction.clone(),
        source: ReleaseIdentity {
            target: Target::MacosArm64.triple().to_owned(),
            version: current.short_version,
            catalog_sha256: None,
            inventory_sha256: None,
            bundle_id: Some(MACOS_BUNDLE_ID.to_owned()),
            team_id: None,
        },
        destination: ReleaseIdentity::from_candidate(&candidate),
        steps: vec![PlanStep {
            rel_path: BUNDLE_REL.to_owned(),
            old: Some(old),
            new: Some(new),
            backup_rel: Some(BACKUP_REL.to_owned()),
        }],
        unchanged: Vec::new(),
    };
    Ok(Verified { plan, slot })
}

// ---------------------------------------------------------------- the swap

/// The bundle is exchanged and awaiting a health verdict.
pub struct AppliedBundle<'a> {
    journal: DurableJournal<FileJournalStore>,
    ops: BundleOps<'a>,
}

impl AppliedBundle<'_> {
    pub fn journal(&self) -> &UpdateJournal {
        self.journal.journal()
    }
}

fn holds(ops: &BundleOps<'_>, rel_path: &str, wanted: &Option<FileIdentity>) -> Result<bool> {
    Ok(identity_matches(&ops.observe(rel_path)?, wanted))
}

/// Refuses, with nothing changed, when the install volume (and the coordination
/// area) cannot hold [`JOURNAL_RESERVE_BYTES`]. A failed query is a refusal.
fn ensure_journal_space(inputs: &BundleApplyInputs<'_>) -> Result<()> {
    let txn_dir = inputs.coordination.transaction_dir(inputs.transaction);
    let available = inputs
        .host
        .available_bytes(&inputs.placement.install_root)?
        .min(inputs.host.available_bytes(&txn_dir)?);
    if available < JOURNAL_RESERVE_BYTES {
        return Err(UpdateError::new(
            ErrorCode::InsufficientSpace,
            format!(
                "the update needs {JOURNAL_RESERVE_BYTES} free bytes but only {available} are available"
            ),
        ));
    }
    Ok(())
}

/// Journal intent, exchange, observation, journal result. Any error leaves the
/// caller to recover from the disk.
fn swap_bundle(
    journal: &mut DurableJournal<FileJournalStore>,
    ops: &mut BundleOps<'_>,
) -> Result<()> {
    let step = journal.journal().steps.first().cloned().ok_or_else(|| {
        UpdateError::new(ErrorCode::JournalInvalid, "the bundle journal has no step")
    })?;
    ops.check_placement()?;
    journal.advance_phase(Phase::ExitAuthorized)?;
    journal.advance_phase(Phase::Applying)?;
    ops.check_placement()?;
    // The intent, with both identities, is durable before the exchange.
    journal.advance_step(0, StepState::Intent)?;
    if !holds(ops, BUNDLE_REL, &step.old)? || !holds(ops, BACKUP_REL, &step.new)? {
        return Err(UpdateError::new(
            ErrorCode::InstallModified,
            "a bundle changed during the update",
        ));
    }
    ops.exchange()?;
    // The result is observed, not assumed, and only then made durable.
    if !holds(ops, BUNDLE_REL, &step.new)? || !holds(ops, BACKUP_REL, &step.old)? {
        return Err(UpdateError::new(
            ErrorCode::HashMismatch,
            "the bundles are not exchanged after the swap",
        ));
    }
    ops.check_placement()?;
    journal.advance_step(0, StepState::Applied)?;
    ops.check_placement()?;
    journal.advance_phase(Phase::AwaitingHealth)
}

/// Verifies the relocation mark of the live bundle, if any, against its current
/// placement and removes it durably. Any failure to read, verify, remove or sync
/// it refuses the apply; nothing has been mutated yet.
fn clear_stale_live_mark(inputs: &BundleApplyInputs<'_>) -> Result<()> {
    let live = &inputs.placement.install_root;
    let Some(bytes) = inputs.host.bundle_mark(live)? else {
        return Ok(());
    };
    check_relocation(
        Some(&bytes),
        inputs.coordination.install_id(),
        &inputs.placement.coordination_parent,
        live,
    )?;
    inputs.host.clear_bundle_mark(live)?;
    inputs.host.sync_bundle_mark(live)
}

/// Verifies, plans, writes the journal and exchanges the bundle. On success the
/// journal is `AwaitingHealth`. A failure after the first journal write has
/// already been rolled back (or stopped as `RecoveryRequired`).
pub fn apply_bundle<'a>(inputs: &BundleApplyInputs<'a>) -> Result<AppliedBundle<'a>> {
    if !inputs.runtime_held {
        return Err(UpdateError::new(
            ErrorCode::NotApplicable,
            "the application process is still running",
        ));
    }
    let verified = verify_and_plan(inputs)?;
    let live = &inputs.placement.install_root;
    // The candidate is staged (its bytes are already consumed), so only the
    // journal updates and the rollback still need room. Checked before the
    // first mutation of the receipt, the journal or the install.
    ensure_journal_space(inputs)?;
    // The previous terminal receipt must still describe this install before
    // anything is mutated; otherwise it could not be retired safely.
    crate::startup::ensure_prior_receipts_current(inputs.coordination, &BundleView::new(live))?;

    // A mark left on the live bundle by the previous Committed update refers to
    // the receipt retired next. It is removed durably first, so a rollback that
    // restores this bundle cannot bring back a mark without its receipt.
    clear_stale_live_mark(inputs)?;

    // The previous receipt is retired (durably, or the apply is refused) before
    // the new journal exists, so the new receipt is the only terminal one and
    // no file time is needed to order them.
    crate::startup::retire_prior_receipts(inputs.coordination, inputs.transaction)?;

    // The new bundle is bound to this install and transaction before it can be
    // exchanged in: wherever it is launched from afterwards, it finds this
    // journal. If the mark cannot be written nothing has been exchanged and the
    // update is refused.
    let mark = RelocationMark {
        install_id: inputs.coordination.install_id().clone(),
        transaction: inputs.transaction.clone(),
        coordination_parent: inputs.placement.coordination_parent.clone(),
    };
    inputs.host.set_bundle_mark(&verified.slot, &mark.encode()?)?;
    // Only a mark that is durable lets the journal and the exchange begin; a
    // power loss after the exchange must not leave a swapped bundle unmarked.
    if let Err(err) = inputs.host.sync_bundle_mark(&verified.slot) {
        let _ = inputs.host.clear_bundle_mark(&verified.slot);
        return Err(err);
    }

    let journal = UpdateJournal::begin(&verified.plan, None)?;
    let store = FileJournalStore::new(inputs.coordination.journal_path(inputs.transaction));
    let mut journal = DurableJournal::create(store, journal)?;
    // The placement is fixed before the first mutation; a bundle that cannot be
    // pinned is refused here, with nothing written.
    let mut ops = BundleOps::pinned(inputs.host, live, &verified.slot)?
        .with_commit_check(verified.plan.destination.version.clone(), inputs.policy.clone());
    match swap_bundle(&mut journal, &mut ops) {
        Ok(()) => Ok(AppliedBundle { journal, ops }),
        Err(err) => match apply::recover(&mut journal, &mut ops, &mut NoChild) {
            Ok(RecoveryOutcome::Nothing | RecoveryOutcome::KeptOld | RecoveryOutcome::RolledBack) => {
                Err(err)
            }
            Ok(RecoveryOutcome::RecoveryRequired(why)) => Err(UpdateError::new(
                ErrorCode::InstallModified,
                format!("recovery is required: {why}"),
            )),
            Ok(RecoveryOutcome::KeptNew) => Err(UpdateError::new(
                ErrorCode::InstallModified,
                "recovery is required: the new bundle is live without a commit",
            )),
            Err(recover_err) => Err(UpdateError::new(
                ErrorCode::InstallModified,
                format!("recovery is required: {recover_err}"),
            )),
        },
    }
}

/// The executable the limited-health child starts from: the live bundle, which
/// must still be exactly the journaled new bundle and still verify against its
/// signed inventory after the exchange.
fn verified_child(inputs: &BundleApplyInputs<'_>, applied: &AppliedBundle<'_>) -> Result<(PathBuf, String)> {
    let live = &inputs.placement.install_root;
    applied.ops.check_placement()?;
    let journal = applied.journal();
    let step = journal.steps.first().ok_or_else(|| {
        UpdateError::new(ErrorCode::JournalInvalid, "the bundle journal has no step")
    })?;
    if !identity_matches(&bundle_identity(live)?, &step.new) {
        return Err(UpdateError::new(
            ErrorCode::HashMismatch,
            "the live bundle differs from the journaled new bundle",
        ));
    }
    require_verified(inputs.host, live, inputs.policy)?;
    let exe = live.join(MAIN_EXE);
    hash_regular_file(&exe)?
        .ok_or_else(|| UpdateError::new(ErrorCode::NotFound, "the bundle executable is missing"))?;
    Ok((exe, journal.destination.version.clone()))
}

/// The helper's whole apply step for a bundle. After the exchange the limited
/// child is started from the live bundle and judged: only a verified `Healthy`
/// from this transaction's own child lets the journal reach a durable
/// `Committed` (`Ok`). Every other end has stopped the child and exchanged the
/// old bundle back, or has stopped as `RecoveryRequired`; none of them is
/// reported as applied.
pub fn apply_and_settle(
    inputs: &BundleApplyInputs<'_>,
    launcher: &mut dyn HealthLauncher,
) -> Result<()> {
    let mut applied = apply_bundle(inputs)?;
    let verified = verified_child(inputs, &applied);
    let mut child = match &verified {
        Ok((exe, version)) => launcher.launch(&LaunchRequest {
            exe,
            version,
            candidate_id: inputs.candidate_id,
            coordination: inputs.coordination,
            transaction: inputs.transaction,
        }),
        Err(err) => launcher.failed(
            err.clone(),
            &LaunchFailure {
                candidate_id: inputs.candidate_id,
                coordination: inputs.coordination,
                transaction: inputs.transaction,
            },
        ),
    };
    let settled = conclude_with_child(
        &mut applied.journal,
        &mut applied.ops,
        &mut *child,
        launcher.health_timeout(),
    )?;
    let result = settled_result(settled);
    if result.is_ok() {
        // Committed: the mark has done its job. A leftover is harmless (a
        // terminal journal lets a moved bundle start).
        let _ = inputs.host.clear_bundle_mark(&inputs.placement.install_root);
    }
    result
}

/// Settles a pending bundle journal from the disk (startup / helper recovery).
/// The caller has already checked that the journal is this install's and this
/// transaction's. The helper holds the exclusive `runtime` lease, so no Hane
/// process is alive and no child needs stopping.
pub fn recover_bundle(
    coordination: &Coordination,
    transaction: &TransactionId,
    placement: &Placement,
    host: &dyn BundleHost,
    journal: &mut DurableJournal<FileJournalStore>,
) -> Result<RecoveryOutcome> {
    if placement.target != Target::MacosArm64 {
        return Ok(RecoveryOutcome::RecoveryRequired(
            "the journal is not for a macOS bundle".to_owned(),
        ));
    }
    let slot = slot_path(&coordination.transaction_dir(transaction));
    let mut ops = BundleOps::new(host, &placement.install_root, &slot);
    apply::recover(journal, &mut ops, &mut NoChild)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::time::Duration;

    use super::*;
    use crate::catalog::record_seen;
    use crate::health::LimitedChild;
    use crate::apply::HealthChild;
    use crate::identity::{InstallFacts, InstallId};
    use crate::journal::JournalStore;
    use crate::testing::{
        FixedClock, TEST_SOURCE_SHA, TempDir, ZipEntry, build_zip, catalog_doc, macos_entry,
        macos_inventory, sign_catalog, test_now, test_policy,
    };
    use crate::windows_apply::{CandidateSource, stage_candidate};

    const OLD: &str = "0.25.0";
    const NEW: &str = "0.26.0";

    fn plist(version: &str, commit: &str, bundle_id: &str, protocol: u64) -> String {
        format!(
            "<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict>\n\
             <key>CFBundleIdentifier</key>\n<string>{bundle_id}</string>\n\
             <key>CFBundleShortVersionString</key>\n<string>{version}</string>\n\
             <key>CFBundleVersion</key>\n<string>{version}</string>\n\
             <key>HaneSourceCommit</key>\n<string>{commit}</string>\n\
             <key>HaneUpdaterProtocol</key>\n<integer>{protocol}</integer>\n\
             </dict></plist>\n"
        )
    }

    fn good_plist(version: &str) -> String {
        plist(version, TEST_SOURCE_SHA, MACOS_BUNDLE_ID, 1)
    }

    fn bundle_files(version: &str, info: &str) -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("Contents/Info.plist", info.as_bytes().to_vec()),
            ("Contents/MacOS/hane", format!("hane {version}").into_bytes()),
            ("Contents/Helpers/hane-updater", format!("updater {version}").into_bytes()),
            ("Contents/_CodeSignature/CodeResources", format!("seal {version}").into_bytes()),
        ]
    }

    fn write_bundle(root: &Path, files: &[(&'static str, Vec<u8>)]) {
        for (rel, data) in files {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, data).unwrap();
        }
    }

    fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(base, &path, out);
                } else {
                    let rel = path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                    out.insert(rel, fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn files_tree(files: &[(&'static str, Vec<u8>)]) -> BTreeMap<String, Vec<u8>> {
        files.iter().map(|(rel, data)| ((*rel).to_owned(), data.clone())).collect()
    }

    fn io_error(what: &str) -> UpdateError {
        UpdateError::new(ErrorCode::Io, what)
    }

    /// Exchanges two directories by renames. The test double of `RENAME_SWAP`.
    fn rename_swap(first: &Path, second: &Path) {
        let parked = first.with_extension("parked");
        fs::rename(first, &parked).unwrap();
        fs::rename(second, first).unwrap();
        fs::rename(&parked, second).unwrap();
    }

    /// Journal phase and step state, when a journal was readable.
    type JournalSnapshot = Option<(Phase, StepState)>;

    struct FakeHost {
        swaps: Cell<usize>,
        fail_swap: Cell<bool>,
        /// The call "succeeds" but exchanges nothing.
        noop_swap: Cell<bool>,
        swap_supported: Cell<bool>,
        apfs: Cell<bool>,
        local: Cell<bool>,
        read_only: Cell<bool>,
        slot_on_other_volume: Cell<bool>,
        /// `None` makes the free-space query fail.
        available: Cell<Option<u64>>,
        bad_signature: RefCell<Vec<PathBuf>>,
        journal_path: RefCell<Option<PathBuf>>,
        /// Journal phase and step state seen at each exchange.
        at_swap: RefCell<Vec<(Phase, StepState)>>,
        /// Relocation marks by directory identity, like an extended attribute:
        /// they follow the directory through a rename.
        marks: RefCell<BTreeMap<String, Vec<u8>>>,
        fail_sync: Cell<bool>,
        fail_set_mark: Cell<bool>,
        fail_clear_mark: Cell<bool>,
        /// Number of mark syncs requested.
        syncs: Cell<usize>,
        fail_swap_sync: Cell<bool>,
        /// Exchanges done so far and the journal phase / step state seen at
        /// each parent-directory sync after an exchange.
        swap_syncs: RefCell<Vec<(usize, JournalSnapshot)>>,
        /// Runs right after a successful exchange, before anything observes it
        /// (the Finder moving the live bundle).
        after_swap: RefCell<Option<Box<dyn Fn()>>>,
    }

    fn mark_key(path: &Path) -> Result<String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = fs::symlink_metadata(path)?;
            Ok(format!("{}:{}", meta.dev(), meta.ino()))
        }
        #[cfg(not(unix))]
        {
            Ok(path.to_string_lossy().into_owned())
        }
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                swaps: Cell::new(0),
                fail_swap: Cell::new(false),
                noop_swap: Cell::new(false),
                swap_supported: Cell::new(true),
                apfs: Cell::new(true),
                local: Cell::new(true),
                read_only: Cell::new(false),
                slot_on_other_volume: Cell::new(false),
                available: Cell::new(Some(u64::MAX)),
                bad_signature: RefCell::new(Vec::new()),
                journal_path: RefCell::new(None),
                at_swap: RefCell::new(Vec::new()),
                marks: RefCell::new(BTreeMap::new()),
                fail_sync: Cell::new(false),
                fail_set_mark: Cell::new(false),
                fail_clear_mark: Cell::new(false),
                syncs: Cell::new(0),
                fail_swap_sync: Cell::new(false),
                swap_syncs: RefCell::new(Vec::new()),
                after_swap: RefCell::new(None),
            }
        }
    }

    impl BundleHost for FakeHost {
        fn volume(&self, path: &Path) -> Result<VolumeFacts> {
            let other = self.slot_on_other_volume.get()
                && path.components().any(|c| c.as_os_str() == STAGE_DIR);
            Ok(VolumeFacts {
                id: if other { "2" } else { "1" }.to_owned(),
                apfs: self.apfs.get(),
                local: self.local.get(),
                read_only: self.read_only.get(),
            })
        }

        fn available_bytes(&self, _path: &Path) -> Result<u64> {
            self.available
                .get()
                .ok_or_else(|| UpdateError::new(ErrorCode::Io, "free space unavailable"))
        }

        fn supports_swap(&self, _first_dir: &Path, _second_dir: &Path) -> Result<bool> {
            Ok(self.swap_supported.get())
        }

        fn swap(&self, first: &Path, second: &Path) -> Result<()> {
            if let Some(path) = self.journal_path.borrow().as_ref()
                && let Some(journal) = FileJournalStore::new(path.clone()).load().unwrap()
            {
                self.at_swap.borrow_mut().push((journal.phase, journal.steps[0].state));
            }
            if self.fail_swap.get() {
                return Err(io_error("injected swap failure"));
            }
            self.swaps.set(self.swaps.get() + 1);
            if !self.noop_swap.get() {
                rename_swap(first, second);
            }
            if let Some(hook) = self.after_swap.borrow().as_ref() {
                hook();
            }
            Ok(())
        }

        fn sync_swap(&self, _first: &Path, _second: &Path) -> Result<()> {
            let journal = self
                .journal_path
                .borrow()
                .as_ref()
                .and_then(|path| FileJournalStore::new(path.clone()).load().unwrap())
                .map(|journal| (journal.phase, journal.steps[0].state));
            self.swap_syncs.borrow_mut().push((self.swaps.get(), journal));
            if self.fail_swap_sync.get() {
                return Err(io_error("injected swap directory sync failure"));
            }
            Ok(())
        }

        fn bundle_mark(&self, bundle: &Path) -> Result<Option<Vec<u8>>> {
            Ok(self.marks.borrow().get(&mark_key(bundle)?).cloned())
        }

        fn set_bundle_mark(&self, bundle: &Path, mark: &[u8]) -> Result<()> {
            if self.fail_set_mark.get() {
                return Err(io_error("injected mark write failure"));
            }
            self.marks.borrow_mut().insert(mark_key(bundle)?, mark.to_vec());
            Ok(())
        }

        fn sync_bundle_mark(&self, _bundle: &Path) -> Result<()> {
            self.syncs.set(self.syncs.get() + 1);
            if self.fail_sync.get() {
                return Err(io_error("injected mark sync failure"));
            }
            Ok(())
        }

        fn clear_bundle_mark(&self, bundle: &Path) -> Result<()> {
            if self.fail_clear_mark.get() {
                return Err(io_error("injected mark removal failure"));
            }
            self.marks.borrow_mut().remove(&mark_key(bundle)?);
            Ok(())
        }

        fn verify_bundle_inventory(&self, bundle: &Path, _policy: &TrustPolicy) -> Result<()> {
            if self.bad_signature.borrow().iter().any(|bad| bundle.starts_with(bad)) {
                return Err(UpdateError::new(ErrorCode::SignatureInvalid, "bad signature"));
            }
            Ok(())
        }
    }

    struct Fixture {
        _dir: TempDir,
        coordination: Coordination,
        transaction: TransactionId,
        placement: Placement,
        candidate_id: String,
        policy: TrustPolicy,
        clock: FixedClock,
        host: FakeHost,
        old_files: Vec<(&'static str, Vec<u8>)>,
        new_files: Vec<(&'static str, Vec<u8>)>,
    }

    fn fixture() -> Fixture {
        fixture_with(&good_plist(NEW))
    }

    /// `candidate_info` is the Info.plist inside the candidate archive.
    fn fixture_with(candidate_info: &str) -> Fixture {
        let dir = TempDir::new("macos-apply");
        let live = dir.path().join("Hane.app");
        let old_files = bundle_files(OLD, &good_plist(OLD));
        write_bundle(&live, &old_files);

        let install_id = InstallId::derive(&InstallFacts {
            os: "test",
            volume_id: "v",
            install_path: "/x",
            owner: "u",
        })
        .unwrap();
        let coordination = Coordination::create(dir.path(), &install_id).unwrap();
        let transaction = TransactionId::generate().unwrap();
        coordination.ensure_helper_dir(&transaction).unwrap();

        let mut new_files = bundle_files(NEW, candidate_info);
        // The archive carries the inventory signed with the update key, as the
        // release does; there is no Apple signature, Team ID or notarization.
        let listed: Vec<(String, Vec<u8>)> =
            new_files.iter().map(|(rel, data)| ((*rel).to_owned(), data.clone())).collect();
        let (inventory_raw, inventory_sig) = macos_inventory(NEW, &listed);
        new_files.push((MACOS_INVENTORY_REL, inventory_raw.clone()));
        new_files.push((MACOS_INVENTORY_SIGNATURE_REL, inventory_sig));
        let mut entries = vec![
            ZipEntry::dir("hane-macos-arm64/"),
            ZipEntry::dir("hane-macos-arm64/Hane.app/"),
        ];
        for (rel, data) in &new_files {
            let name = format!("hane-macos-arm64/Hane.app/{rel}");
            let entry = ZipEntry::file(&name, data);
            entries.push(if rel.starts_with("Contents/MacOS/") || rel.starts_with("Contents/Helpers/") {
                entry.executable()
            } else {
                entry
            });
        }
        entries.push(ZipEntry::file("hane-macos-arm64/README.md", b"readme"));
        let unpacked: u64 = entries.iter().map(ZipEntry::declared).sum();
        let zip = build_zip(&entries);
        let doc = catalog_doc(NEW, 2, Target::MacosArm64, macos_entry(NEW, &zip, unpacked, &inventory_raw));
        let (catalog, catalog_sig) = sign_catalog(&doc);

        let ready = dir.path().join("ready");
        fs::create_dir_all(&ready).unwrap();
        fs::write(ready.join("c"), &catalog).unwrap();
        fs::write(ready.join("s"), &catalog_sig).unwrap();
        fs::write(ready.join("a"), &zip).unwrap();
        stage_candidate(
            &coordination,
            &transaction,
            &CandidateSource {
                catalog: &ready.join("c"),
                signature: &ready.join("s"),
                archive: &ready.join("a"),
            },
        )
        .unwrap();
        let verified = verify_catalog(
            &catalog,
            &catalog_sig,
            &VerifyContext {
                policy: &test_policy(),
                clock: &FixedClock::new(test_now()),
                current_version: OLD,
                target: Target::MacosArm64,
                seen: None,
                expected_tag: None,
            },
        )
        .unwrap();
        let candidate_id = verified.candidate_id().as_str().to_owned();
        fs::create_dir_all(coordination.dir().join(crate::platform::lease::RECEIPTS_DIR)).unwrap();
        record_seen(&mut FileSeenStore::new(coordination.seen_catalog_path()), &verified).unwrap();

        let placement = Placement {
            target: Target::MacosArm64,
            install_root: live,
            coordination_parent: dir.path().to_path_buf(),
            install_id,
        };
        let host = FakeHost::new();
        *host.journal_path.borrow_mut() = Some(coordination.journal_path(&transaction));
        Fixture {
            _dir: dir,
            coordination,
            transaction,
            placement,
            candidate_id,
            policy: test_policy(),
            clock: FixedClock::new(test_now()),
            host,
            old_files,
            new_files,
        }
    }

    impl Fixture {
        fn inputs(&self) -> BundleApplyInputs<'_> {
            BundleApplyInputs {
                coordination: &self.coordination,
                transaction: &self.transaction,
                candidate_id: &self.candidate_id,
                placement: &self.placement,
                policy: &self.policy,
                clock: &self.clock,
                host: &self.host,
                runtime_held: true,
            }
        }

        fn live(&self) -> &Path {
            &self.placement.install_root
        }

        fn slot(&self) -> PathBuf {
            slot_path(&self.coordination.transaction_dir(&self.transaction))
        }

        fn journal_path(&self) -> PathBuf {
            self.coordination.journal_path(&self.transaction)
        }

        fn stored(&self) -> UpdateJournal {
            FileJournalStore::new(self.journal_path()).load().unwrap().unwrap()
        }

        fn live_is_old(&self) -> bool {
            tree(self.live()) == files_tree(&self.old_files)
        }

        fn live_is_new(&self) -> bool {
            tree(self.live()) == files_tree(&self.new_files)
        }

        /// Nothing was changed: no journal, no exchange, the old bundle live.
        fn untouched(&self) -> bool {
            self.host.swaps.get() == 0 && !self.journal_path().exists() && self.live_is_old()
        }
    }

    // ------------------------------------------------------ fake child

    #[derive(Default)]
    struct Probe {
        stops: Cell<usize>,
        commits: Cell<usize>,
        phase_at_commit: RefCell<Option<Phase>>,
        launched: RefCell<Option<(PathBuf, String)>>,
    }

    struct FakeChild {
        health: Result<()>,
        stop: Result<()>,
        journal_path: PathBuf,
        probe: Rc<Probe>,
        /// Runs just before the verdict, i.e. after the exchange.
        before_verdict: Option<Rc<dyn Fn()>>,
    }

    impl LimitedChild for FakeChild {
        fn await_healthy(&mut self, _timeout: Duration) -> Result<()> {
            if let Some(hook) = &self.before_verdict {
                hook();
            }
            self.health.clone()
        }

        fn send_commit(&mut self) -> Result<()> {
            self.probe.commits.set(self.probe.commits.get() + 1);
            let stored = FileJournalStore::new(self.journal_path.clone()).load().unwrap().unwrap();
            *self.probe.phase_at_commit.borrow_mut() = Some(stored.phase);
            Ok(())
        }

        fn detach(&mut self) {}

        fn as_health_child(&mut self) -> &mut dyn HealthChild {
            self
        }
    }

    impl HealthChild for FakeChild {
        fn stop_and_release_lease(&mut self) -> Result<()> {
            self.probe.stops.set(self.probe.stops.get() + 1);
            self.stop.clone()
        }
    }

    struct FakeLauncher {
        health: Result<()>,
        stop: Result<()>,
        journal_path: PathBuf,
        probe: Rc<Probe>,
        before_verdict: Option<Rc<dyn Fn()>>,
    }

    impl FakeLauncher {
        fn new(fx: &Fixture, health: Result<()>) -> Self {
            Self {
                health,
                stop: Ok(()),
                journal_path: fx.journal_path(),
                probe: Rc::new(Probe::default()),
                before_verdict: None,
            }
        }

        fn child(&self, health: Result<()>) -> Box<dyn LimitedChild> {
            Box::new(FakeChild {
                health,
                stop: self.stop.clone(),
                journal_path: self.journal_path.clone(),
                probe: Rc::clone(&self.probe),
                before_verdict: self.before_verdict.clone(),
            })
        }
    }

    impl HealthLauncher for FakeLauncher {
        fn launch(&mut self, request: &LaunchRequest<'_>) -> Box<dyn LimitedChild> {
            *self.probe.launched.borrow_mut() =
                Some((request.exe.to_path_buf(), request.version.to_owned()));
            self.child(self.health.clone())
        }

        fn failed(&mut self, error: UpdateError, _request: &LaunchFailure<'_>) -> Box<dyn LimitedChild> {
            self.child(Err(error))
        }

        fn health_timeout(&self) -> Duration {
            Duration::from_millis(10)
        }
    }

    // ----------------------------------------------- identity and parsing

    #[test]
    fn plist_values_are_read_from_xml_only_and_only_for_their_own_key() {
        let info = good_plist(NEW);
        assert_eq!(plist_string(&info, "CFBundleIdentifier").as_deref(), Some(MACOS_BUNDLE_ID));
        assert_eq!(plist_integer(&info, "HaneUpdaterProtocol"), Some(1));
        // An integer is not a string and a string is not an integer.
        assert_eq!(plist_string(&info, "HaneUpdaterProtocol"), None);
        assert_eq!(plist_integer(&info, "CFBundleIdentifier"), None);
        // The next key's value is never borrowed.
        assert_eq!(plist_string("<key>A</key><true/><key>B</key><string>x</string>", "A"), None);
        assert_eq!(plist_string("bplist00\u{1}", "CFBundleIdentifier"), None);
    }

    #[test]
    fn the_bundle_identity_follows_the_executable_the_seal_and_the_plist() {
        let dir = TempDir::new("macos-identity");
        let bundle = dir.path().join("Hane.app");
        assert_eq!(bundle_identity(&bundle).unwrap(), Observed::Missing);
        write_bundle(&bundle, &bundle_files(OLD, &good_plist(OLD)));
        let base = bundle_identity(&bundle).unwrap();
        assert!(matches!(base, Observed::File(_)));
        assert_eq!(bundle_identity(&bundle).unwrap(), base);
        for rel in IDENTITY_FILES {
            let path = bundle.join(rel);
            let original = fs::read(&path).unwrap();
            fs::write(&path, b"changed").unwrap();
            assert_ne!(bundle_identity(&bundle).unwrap(), base, "{rel}");
            fs::write(&path, original).unwrap();
        }
        // An incomplete bundle is an error, not an identity.
        fs::remove_file(bundle.join("Contents/_CodeSignature/CodeResources")).unwrap();
        assert!(bundle_identity(&bundle).is_err());
    }

    #[test]
    fn the_bundle_identity_covers_resources_and_nested_code_not_just_the_seal() {
        let dir = TempDir::new("macos-identity-deep");
        let bundle = dir.path().join("Hane.app");
        write_bundle(&bundle, &bundle_files(OLD, &good_plist(OLD)));
        let resource = bundle.join("Contents/Resources/icon.icns");
        let nested = bundle.join("Contents/Frameworks/X.framework/Versions/A/X");
        for path in [&resource, &nested] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"original").unwrap();
        }
        let base = bundle_identity(&bundle).unwrap();
        for path in [&resource, &nested] {
            fs::write(path, b"tampered").unwrap();
            assert_ne!(bundle_identity(&bundle).unwrap(), base, "{}", path.display());
            fs::write(path, b"original").unwrap();
            assert_eq!(bundle_identity(&bundle).unwrap(), base);
        }
        // An extra entry changes it as well.
        fs::write(bundle.join("Contents/Resources/extra"), b"x").unwrap();
        assert_ne!(bundle_identity(&bundle).unwrap(), base);
    }

    #[cfg(unix)]
    #[test]
    fn the_bundle_identity_follows_symlink_targets_without_following_them() {
        let dir = TempDir::new("macos-identity-symlink");
        let bundle = dir.path().join("Hane.app");
        write_bundle(&bundle, &bundle_files(OLD, &good_plist(OLD)));
        let outside = dir.path().join("outside");
        fs::write(&outside, b"one").unwrap();
        let link = bundle.join("Contents/Frameworks-link");
        std::os::unix::fs::symlink("Info.plist", &link).unwrap();
        let base = bundle_identity(&bundle).unwrap();
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink("MacOS/hane", &link).unwrap();
        let retargeted = bundle_identity(&bundle).unwrap();
        assert_ne!(retargeted, base);
        // A target outside the bundle is recorded, and its bytes are not read.
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let before = bundle_identity(&bundle).unwrap();
        fs::write(&outside, b"two").unwrap();
        assert_eq!(bundle_identity(&bundle).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_bundle_is_never_an_identity() {
        let dir = TempDir::new("macos-link");
        let real = dir.path().join("real.app");
        write_bundle(&real, &bundle_files(OLD, &good_plist(OLD)));
        let link = dir.path().join("Hane.app");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(bundle_identity(&link).is_err());
        assert!(read_bundle_facts(&link).is_err());
    }

    // --------------------------------------------------------- apply

    #[test]
    fn a_healthy_child_commits_and_the_new_bundle_stays() {
        let fx = fixture();
        let mut launcher = FakeLauncher::new(&fx, Ok(()));
        apply_and_settle(&fx.inputs(), &mut launcher).unwrap();

        assert!(fx.live_is_new());
        assert_eq!(fx.stored().phase, Phase::Committed);
        assert_eq!(fx.stored().steps[0].state, StepState::Applied);
        assert_eq!(fx.host.swaps.get(), 1, "exactly one exchange");
        assert_eq!(launcher.probe.commits.get(), 1);
        assert_eq!(
            *launcher.probe.phase_at_commit.borrow(),
            Some(Phase::Committed),
            "Commit is sent only after the journal is durably Committed"
        );
        assert_eq!(launcher.probe.stops.get(), 0);
        let launched = launcher.probe.launched.borrow().clone().unwrap();
        assert_eq!(launched.0, fx.live().join("Contents/MacOS/hane"));
        assert_eq!(launched.1, NEW);
        // The old bundle rests in the slot as the backup.
        assert_eq!(tree(&fx.slot()), files_tree(&fx.old_files));
    }

    #[test]
    fn the_intent_is_durable_before_the_exchange_and_the_result_after_it() {
        let fx = fixture();
        let applied = apply_bundle(&fx.inputs()).unwrap();
        assert_eq!(*fx.host.at_swap.borrow(), vec![(Phase::Applying, StepState::Intent)]);
        let journal = applied.journal();
        assert_eq!(journal.phase, Phase::AwaitingHealth);
        assert_eq!(journal.steps.len(), 1);
        let step = &journal.steps[0];
        assert_eq!(step.state, StepState::Applied);
        assert_eq!(step.rel_path, BUNDLE_REL);
        assert_eq!(step.backup_rel.as_deref(), Some(BACKUP_REL));
        assert_eq!(Some(Observed::File(step.new.clone().unwrap())), Some(bundle_identity(fx.live()).unwrap()));
        assert_eq!(Observed::File(step.old.clone().unwrap()), bundle_identity(&fx.slot()).unwrap());
        assert_eq!(journal.source.team_id, None);
        assert_eq!(journal.destination.team_id, None);
        assert_eq!(journal.source.version, OLD);
        assert_eq!(journal.destination.version, NEW);
    }

    #[test]
    fn an_unhealthy_child_is_stopped_and_the_old_bundle_comes_back_by_the_same_exchange() {
        for code in [ErrorCode::Timeout, ErrorCode::Truncated, ErrorCode::IdentityMismatch] {
            let fx = fixture();
            let mut launcher = FakeLauncher::new(&fx, Err(UpdateError::new(code, "no health")));
            let err = apply_and_settle(&fx.inputs(), &mut launcher).unwrap_err();
            assert_eq!(err.code, code);
            assert!(fx.live_is_old(), "{code:?}: the old bundle must be back byte for byte");
            assert_eq!(tree(&fx.slot()), files_tree(&fx.new_files), "{code:?}");
            assert_eq!(fx.stored().phase, Phase::RolledBack, "{code:?}");
            assert_eq!(fx.host.swaps.get(), 2, "{code:?}: one exchange and one rollback exchange");
            assert_eq!(launcher.probe.stops.get(), 1, "{code:?}");
            assert_eq!(launcher.probe.commits.get(), 0, "{code:?}");
        }
    }

    #[test]
    fn a_child_that_cannot_be_stopped_blocks_the_rollback_exchange() {
        let fx = fixture();
        let mut launcher =
            FakeLauncher::new(&fx, Err(UpdateError::new(ErrorCode::Timeout, "no health")));
        launcher.stop = Err(UpdateError::new(ErrorCode::Timeout, "the runtime lease is still held"));
        let err = apply_and_settle(&fx.inputs(), &mut launcher).unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);
        assert_eq!(fx.stored().phase, Phase::RecoveryRequired);
        assert_eq!(fx.host.swaps.get(), 1, "no second exchange while the child may run");
        assert!(fx.live_is_new());
        assert_eq!(launcher.probe.commits.get(), 0);
    }

    /// The bundle is moved (or replaced) after the exchange and before the
    /// health verdict is accepted: never Committed, nothing else is written.
    #[cfg(unix)]
    #[test]
    fn a_bundle_moved_or_replaced_before_the_health_commit_is_never_committed() {
        for replace in [false, true] {
            for healthy in [true, false] {
                let fx = fixture();
                let live = fx.live().to_path_buf();
                let moved = live.with_file_name("Moved.app");
                let hook_live = live.clone();
                let hook_moved = moved.clone();
                let mut launcher = FakeLauncher::new(
                    &fx,
                    if healthy {
                        Ok(())
                    } else {
                        Err(UpdateError::new(ErrorCode::Timeout, "no health"))
                    },
                );
                launcher.before_verdict = Some(Rc::new(move || {
                    fs::rename(&hook_live, &hook_moved).unwrap();
                    if replace {
                        write_bundle(&hook_live, &[("Contents/unknown.txt", b"user".to_vec())]);
                    }
                }));
                let label = format!("replace={replace} healthy={healthy}");

                let err = apply_and_settle(&fx.inputs(), &mut launcher).unwrap_err();
                assert_eq!(err.code, ErrorCode::InstallModified, "{label}");
                assert_eq!(fx.stored().phase, Phase::RecoveryRequired, "{label}");
                assert_eq!(launcher.probe.commits.get(), 0, "{label}");
                assert_eq!(fx.host.swaps.get(), 1, "{label}: no rollback exchange by pathname");
                // The moved bundle is untouched; the slot still holds the old one.
                assert_eq!(tree(&moved), files_tree(&fx.new_files), "{label}");
                assert_eq!(tree(&fx.slot()), files_tree(&fx.old_files), "{label}");
                if replace {
                    assert_eq!(
                        tree(&live),
                        files_tree(&[("Contents/unknown.txt", b"user".to_vec())]),
                        "{label}: unknown content is not overwritten"
                    );
                } else {
                    assert!(!live.exists(), "{label}");
                }
            }
        }
    }

    #[cfg(unix)]
    fn moved_install_id() -> InstallId {
        InstallId::derive(&InstallFacts {
            os: "test",
            volume_id: "v",
            install_path: "/moved/Hane.app",
            owner: "u",
        })
        .unwrap()
    }

    /// The Finder moves the live bundle right after the exchange, before the
    /// helper can look at it. The moved bundle runs under another `InstallId`
    /// (the path is part of it), yet it must still meet the pending journal.
    #[cfg(unix)]
    #[test]
    fn a_bundle_moved_right_after_the_exchange_cannot_bypass_the_pending_journal() {
        let fx = fixture();
        let live = fx.live().to_path_buf();
        let moved = live.with_file_name("Moved.app");
        let (hook_live, hook_moved) = (live.clone(), moved.clone());
        *fx.host.after_swap.borrow_mut() =
            Some(Box::new(move || fs::rename(&hook_live, &hook_moved).unwrap()));

        let err = apply_bundle(&fx.inputs()).err().expect("the move is detected");
        assert_eq!(err.code, ErrorCode::InstallModified);
        assert_eq!(fx.host.swaps.get(), 1);
        assert_eq!(fx.stored().phase, Phase::RecoveryRequired);
        assert_eq!(tree(&moved), files_tree(&fx.new_files));

        // The moved bundle carries the mark written before the exchange.
        let mark = fx.host.bundle_mark(&moved).unwrap();
        assert!(mark.is_some(), "the mark follows the bundle directory");
        assert_ne!(&moved_install_id(), fx.coordination.install_id());
        let parent = fx.placement.coordination_parent.clone();
        let err =
            check_relocation(mark.as_deref(), &moved_install_id(), &parent, &moved).unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified, "the moved bundle must stop");
        // At the original location the ordinary gate handles the same journal.
        check_relocation(mark.as_deref(), fx.coordination.install_id(), &parent, &moved).unwrap();
        // An unreadable mark is never ignored.
        let err =
            check_relocation(Some(b"garbage"), &moved_install_id(), &parent, &moved).unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);
    }

    /// The parent directory moves together with the bundle, so the coordination
    /// parent recorded in the mark is gone. The transaction is found under the
    /// current parent: pending stops, a terminal state starts, and a missing
    /// journal fails closed.
    #[cfg(unix)]
    #[test]
    fn a_relocated_parent_cannot_bypass_the_pending_journal() {
        let fx = fixture();
        let live = fx.live().to_path_buf();
        let moved = live.with_file_name("Moved.app");
        let (hook_live, hook_moved) = (live.clone(), moved.clone());
        *fx.host.after_swap.borrow_mut() =
            Some(Box::new(move || fs::rename(&hook_live, &hook_moved).unwrap()));
        apply_bundle(&fx.inputs()).err().expect("the move is detected");
        assert_eq!(fx.stored().phase, Phase::RecoveryRequired);

        let current_parent = fx.placement.coordination_parent.clone();
        let old_parent = current_parent.join("old-parent-that-no-longer-exists");
        let mark = RelocationMark {
            install_id: fx.coordination.install_id().clone(),
            transaction: fx.transaction.clone(),
            coordination_parent: old_parent.clone(),
        }
        .encode()
        .unwrap();
        assert!(!old_parent.exists());

        let err = check_relocation(Some(&mark), &moved_install_id(), &current_parent, &moved)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified, "the pending journal must stop it");

        // Nothing verifiable anywhere: refuse instead of treating it as safe.
        let empty = TempDir::new("relocation-empty");
        let err = check_relocation(Some(&mark), &moved_install_id(), empty.path(), &moved)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);
    }

    /// A marked bundle moved to a volume the updater does not support must meet
    /// the original transaction before it can count as unmanaged.
    #[cfg(unix)]
    #[test]
    fn a_marked_bundle_on_an_unsupported_volume_cannot_bypass_the_pending_journal() {
        let fx = fixture();
        let mut launcher = FakeLauncher::new(&fx, Ok(()));
        let parent = fx.placement.coordination_parent.clone();
        let mark = RelocationMark {
            install_id: fx.coordination.install_id().clone(),
            transaction: fx.transaction.clone(),
            coordination_parent: parent.clone(),
        }
        .encode()
        .unwrap();

        // The journal is still pending (prepared, not terminal).
        let live = fx.live().to_path_buf();
        let err = check_relocation_on_unsupported_volume(Some(&mark), &parent, &live).unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified, "pending must stop");
        // An unreadable mark and an unfindable journal fail closed too.
        let err =
            check_relocation_on_unsupported_volume(Some(b"garbage"), &parent, &live).unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);
        let empty = TempDir::new("relocation-unsupported-empty");
        let err =
            check_relocation_on_unsupported_volume(Some(&mark), empty.path(), &live).unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);
        // No mark: a plainly unsupported placement is left to the caller.
        check_relocation_on_unsupported_volume(None, &parent, &live).unwrap();

        // Once the transaction is terminal the marked bundle is no longer held.
        apply_and_settle(&fx.inputs(), &mut launcher).unwrap();
        check_relocation_on_unsupported_volume(Some(&mark), &parent, &live).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_relocated_parent_with_a_terminal_journal_starts_normally() {
        let fx = fixture();
        let mut launcher = FakeLauncher::new(&fx, Ok(()));
        apply_and_settle(&fx.inputs(), &mut launcher).unwrap();
        let current_parent = fx.placement.coordination_parent.clone();
        let mark = RelocationMark {
            install_id: fx.coordination.install_id().clone(),
            transaction: fx.transaction.clone(),
            coordination_parent: current_parent.join("old-parent-that-no-longer-exists"),
        }
        .encode()
        .unwrap();
        check_relocation(Some(&mark), &moved_install_id(), &current_parent, fx.live()).unwrap();
    }

    /// Builds the mark of the fixture's transaction for a committed update.
    #[cfg(unix)]
    fn committed_mark(fx: &Fixture) -> Vec<u8> {
        RelocationMark {
            install_id: fx.coordination.install_id().clone(),
            transaction: fx.transaction.clone(),
            coordination_parent: fx.placement.coordination_parent.clone(),
        }
        .encode()
        .unwrap()
    }

    /// A leftover mark on a committed update lets a moved bundle start only
    /// while it still is the bundle the journal recorded.
    #[cfg(unix)]
    #[test]
    fn a_moved_bundle_with_a_committed_journal_must_match_the_journaled_identity() {
        let fx = fixture();
        let mut launcher = FakeLauncher::new(&fx, Ok(()));
        apply_and_settle(&fx.inputs(), &mut launcher).unwrap();
        assert_eq!(fx.stored().phase, Phase::Committed);
        let parent = fx.placement.coordination_parent.clone();
        let mark = committed_mark(&fx);

        // The correct committed bundle starts.
        check_relocation(Some(&mark), &moved_install_id(), &parent, fx.live()).unwrap();

        // Damaged content (here: a changed seal file) after the move fails closed.
        fs::write(fx.live().join(MAIN_EXE), b"tampered").unwrap();
        let err = check_relocation(Some(&mark), &moved_install_id(), &parent, fx.live())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);

        // A bundle whose identity cannot be computed fails closed too.
        let missing = fx.live().with_file_name("Missing.app");
        let err = check_relocation(Some(&mark), &moved_install_id(), &parent, &missing)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);
    }

    /// A committed journal without exactly one `Hane.app` step authorizes nothing.
    #[cfg(unix)]
    #[test]
    fn a_committed_journal_without_a_unique_bundle_step_fails_closed_after_relocation() {
        let fx = fixture();
        let mut launcher = FakeLauncher::new(&fx, Ok(()));
        apply_and_settle(&fx.inputs(), &mut launcher).unwrap();
        let mark = committed_mark(&fx);
        let parent = fx.placement.coordination_parent.clone();
        let original = fx.stored();

        let rewrite = |steps: Vec<crate::journal::JournalStep>| {
            let mut journal = original.clone();
            journal.steps = steps;
            fs::write(fx.journal_path(), serde_json::to_vec(&journal).unwrap()).unwrap();
            check_relocation(Some(&mark), &moved_install_id(), &parent, fx.live())
        };

        // Whether the journal itself or the relocation check refuses, it stops.
        assert!(rewrite(Vec::new()).is_err(), "missing step");
        let step = original.steps[0].clone();
        assert!(rewrite(vec![step.clone(), step.clone()]).is_err(), "duplicate step");
        let mut other = step.clone();
        other.new = Some(crate::identity::FileIdentity::of(b"another bundle"));
        assert!(rewrite(vec![other]).is_err(), "identity mismatch");
        let mut none = step;
        none.new = None;
        assert!(rewrite(vec![none]).is_err(), "no new identity");
    }

    /// A rolled-back transaction was rejected by the health check; its marked
    /// bundle must not start from another location, while Committed may.
    #[cfg(unix)]
    #[test]
    fn a_marked_bundle_with_a_rolled_back_journal_fails_closed_after_relocation() {
        let fx = fixture();
        fx.host.fail_swap.set(true);
        apply_bundle(&fx.inputs()).err().expect("the exchange fails");
        assert_eq!(fx.stored().phase, Phase::RolledBack);
        let parent = fx.placement.coordination_parent.clone();
        let mark = RelocationMark {
            install_id: fx.coordination.install_id().clone(),
            transaction: fx.transaction.clone(),
            coordination_parent: parent.clone(),
        }
        .encode()
        .unwrap();
        let err =
            check_relocation(Some(&mark), &moved_install_id(), &parent, fx.live()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);
        let err = check_relocation_on_unsupported_volume(Some(&mark), &parent, fx.live())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);
    }

    #[cfg(unix)]
    #[test]
    fn a_normal_swap_clears_the_mark_and_a_settled_journal_never_blocks_a_moved_bundle() {
        let fx = fixture();
        let mut launcher = FakeLauncher::new(&fx, Ok(()));
        apply_and_settle(&fx.inputs(), &mut launcher).unwrap();
        assert!(fx.live_is_new());
        assert_eq!(fx.host.bundle_mark(fx.live()).unwrap(), None, "cleared once committed");

        let mark = RelocationMark {
            install_id: fx.coordination.install_id().clone(),
            transaction: fx.transaction.clone(),
            coordination_parent: fx.placement.coordination_parent.clone(),
        };
        let bytes = mark.encode().unwrap();
        assert_eq!(RelocationMark::parse(&bytes).unwrap(), mark);
        // A leftover mark of a Committed transaction lets a moved bundle start.
        let parent = fx.placement.coordination_parent.clone();
        check_relocation(Some(&bytes), &moved_install_id(), &parent, fx.live()).unwrap();
        check_relocation(None, &moved_install_id(), &parent, fx.live()).unwrap();
    }

    /// The mark sits on the candidate directory, so after the exchange it is the
    /// new live bundle that carries it.
    #[cfg(unix)]
    #[test]
    fn the_mark_is_written_to_the_candidate_before_the_exchange() {
        let fx = fixture();
        let applied = apply_bundle(&fx.inputs()).unwrap();
        drop(applied);
        let mark = fx.host.bundle_mark(fx.live()).unwrap().expect("marked");
        let parsed = RelocationMark::parse(&mark).unwrap();
        assert_eq!(&parsed.install_id, fx.coordination.install_id());
        assert_eq!(parsed.transaction, fx.transaction);
        assert_eq!(fx.host.bundle_mark(&fx.slot()).unwrap(), None, "the old bundle is unmarked");
    }

    /// The mark is made durable after it is written and before the journal
    /// exists or anything is exchanged.
    #[test]
    fn the_mark_is_synced_before_the_journal_and_the_exchange() {
        let fx = fixture();
        let applied = apply_bundle(&fx.inputs()).unwrap();
        drop(applied);
        assert_eq!(fx.host.syncs.get(), 1);
    }

    #[test]
    fn a_mark_that_cannot_be_synced_refuses_before_any_journal_or_exchange() {
        let fx = fixture();
        fx.host.fail_sync.set(true);
        let err = apply_bundle(&fx.inputs()).err().unwrap();
        assert_eq!(err.code, ErrorCode::Io);
        assert_eq!(fx.host.syncs.get(), 1);
        assert_eq!(fx.host.swaps.get(), 0);
        assert!(fx.untouched());
        assert!(!fx.journal_path().exists(), "no journal was created");
        assert_eq!(fx.host.bundle_mark(&fx.slot()).unwrap(), None, "the unsynced mark is withdrawn");
    }

    /// The parent directories are synced right after the exchange, while the
    /// journal still records only the intent.
    #[test]
    fn the_swap_parents_are_synced_before_the_journal_records_applied() {
        let fx = fixture();
        *fx.host.journal_path.borrow_mut() = Some(fx.journal_path());
        let applied = apply_bundle(&fx.inputs()).unwrap();
        assert_eq!(
            *fx.host.swap_syncs.borrow(),
            vec![(1, Some((Phase::Applying, StepState::Intent)))]
        );
        assert_eq!(applied.journal().steps[0].state, StepState::Applied);
    }

    #[test]
    fn a_swap_directory_sync_failure_does_not_advance_the_journal_or_claim_success() {
        let fx = fixture();
        *fx.host.journal_path.borrow_mut() = Some(fx.journal_path());
        fx.host.fail_swap_sync.set(true);
        // The rollback exchange cannot be made durable either, so recovery
        // stops as required instead of reporting a settled outcome.
        assert!(apply_bundle(&fx.inputs()).is_err());
        let journal = FileJournalStore::new(fx.journal_path()).load().unwrap().unwrap();
        assert_ne!(journal.steps[0].state, StepState::Applied);
        assert_ne!(journal.phase, Phase::AwaitingHealth);
        assert!(
            fx.host
                .swap_syncs
                .borrow()
                .iter()
                .all(|(_, seen)| *seen != Some((Phase::Applying, StepState::Applied))),
            "no sync ran after the journal was advanced"
        );
    }

    /// An earlier Committed update as it is left behind: a valid terminal
    /// receipt whose recorded new bundle is the one that is live now, and the
    /// relocation mark of that transaction on the live bundle. Returns the
    /// earlier transaction and the mark that was set.
    #[cfg(unix)]
    fn earlier_committed_update(fx: &Fixture) -> (TransactionId, Vec<u8>) {
        let earlier = TransactionId::generate().unwrap();
        fx.coordination.ensure_helper_dir(&earlier).unwrap();

        // A Committed journal of the same install from a separate fixture; only
        // the transaction and the bundle step are rewritten to describe `fx`.
        let template = fixture();
        let _ = journal_at(&template, Reached::Committed);
        let mut journal = template.stored();
        let Observed::File(live_identity) = bundle_identity(fx.live()).unwrap() else {
            panic!("the live bundle is missing");
        };
        journal.transaction_id = earlier.clone();
        journal.steps[0].old = Some(crate::identity::FileIdentity::of(b"the bundle before"));
        journal.steps[0].new = Some(live_identity);
        fs::write(fx.coordination.journal_path(&earlier), serde_json::to_vec(&journal).unwrap())
            .unwrap();

        let mark = RelocationMark {
            install_id: fx.coordination.install_id().clone(),
            transaction: earlier.clone(),
            coordination_parent: fx.placement.coordination_parent.clone(),
        }
        .encode()
        .unwrap();
        fx.host.set_bundle_mark(fx.live(), &mark).unwrap();

        // The receipt really is current, and it lets a moved bundle start.
        let parent = fx.placement.coordination_parent.clone();
        crate::startup::ensure_prior_receipts_current(&fx.coordination, &BundleView::new(fx.live()))
            .unwrap();
        check_relocation(Some(&mark), &moved_install_id(), &parent, fx.live()).unwrap();
        (earlier, mark)
    }

    /// The earlier Committed receipt is still there and still Committed.
    #[cfg(unix)]
    fn earlier_receipt_is_kept(fx: &Fixture, earlier: &TransactionId) -> bool {
        FileJournalStore::new(fx.coordination.journal_path(earlier))
            .load()
            .ok()
            .flatten()
            .is_some_and(|journal| journal.phase == Phase::Committed)
    }

    /// The mark of an earlier Committed update still sits on the live bundle
    /// when the next update is applied and rolled back. The old bundle that
    /// comes back must not carry it, or a move would depend on a receipt that
    /// was retired.
    #[cfg(unix)]
    #[test]
    fn a_stale_mark_is_removed_before_an_update_that_rolls_back() {
        let fx = fixture();
        let (earlier, _) = earlier_committed_update(&fx);
        assert!(earlier_receipt_is_kept(&fx, &earlier));
        let mut launcher =
            FakeLauncher::new(&fx, Err(UpdateError::new(ErrorCode::Timeout, "no health")));
        apply_and_settle(&fx.inputs(), &mut launcher).unwrap_err();
        assert!(fx.live_is_old());
        assert_eq!(fx.stored().phase, Phase::RolledBack);
        assert!(
            !fx.coordination.journal_path(&earlier).exists(),
            "the earlier Committed receipt was retired"
        );
        assert_eq!(fx.host.bundle_mark(fx.live()).unwrap(), None, "the restored bundle is unmarked");
        assert_eq!(fx.host.syncs.get(), 2, "the removal and the new mark are both made durable");

        // The restored bundle moves and starts without any receipt of the past.
        let parent = fx.placement.coordination_parent.clone();
        let mark = fx.host.bundle_mark(fx.live()).unwrap();
        check_relocation(mark.as_deref(), &moved_install_id(), &parent, fx.live()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_mark_that_cannot_be_removed_or_synced_refuses_before_any_mutation() {
        for sync in [false, true] {
            let fx = fixture();
            let (earlier, _) = earlier_committed_update(&fx);
            if sync {
                fx.host.fail_sync.set(true);
            } else {
                fx.host.fail_clear_mark.set(true);
            }
            let err = apply_bundle(&fx.inputs()).err().unwrap();
            assert_eq!(err.code, ErrorCode::Io, "sync={sync}");
            assert_eq!(fx.host.swaps.get(), 0, "sync={sync}");
            assert!(fx.untouched(), "sync={sync}");
            assert!(!fx.journal_path().exists(), "no journal was created (sync={sync})");
            assert_eq!(fx.host.bundle_mark(&fx.slot()).unwrap(), None, "sync={sync}");
            assert!(earlier_receipt_is_kept(&fx, &earlier), "the receipt is kept (sync={sync})");
        }
    }

    /// A mark that does not verify against the current placement stops the
    /// apply; it is neither ignored nor removed.
    #[cfg(unix)]
    #[test]
    fn an_unverifiable_stale_mark_refuses_before_any_mutation() {
        for bytes in [b"garbage".to_vec(), {
            RelocationMark {
                install_id: moved_install_id(),
                transaction: TransactionId::generate().unwrap(),
                coordination_parent: TempDir::new("stale-parent").path().to_path_buf(),
            }
            .encode()
            .unwrap()
        }] {
            let fx = fixture();
            fx.host.set_bundle_mark(fx.live(), &bytes).unwrap();
            let err = apply_bundle(&fx.inputs()).err().unwrap();
            assert_eq!(err.code, ErrorCode::InstallModified);
            assert!(fx.untouched());
            assert!(!fx.journal_path().exists(), "no journal was created");
            assert_eq!(fx.host.bundle_mark(fx.live()).unwrap(), Some(bytes));
        }
    }

    #[test]
    fn a_bundle_without_a_mark_is_applied_without_an_extra_sync() {
        let fx = fixture();
        let applied = apply_bundle(&fx.inputs()).unwrap();
        drop(applied);
        assert_eq!(fx.host.syncs.get(), 1, "only the new mark is synced");
    }

    #[test]
    fn a_mark_that_cannot_be_written_refuses_before_any_journal_or_exchange() {
        let fx = fixture();
        fx.host.fail_set_mark.set(true);
        let err = apply_bundle(&fx.inputs()).err().unwrap();
        assert_eq!(err.code, ErrorCode::Io);
        assert_eq!(fx.host.syncs.get(), 0, "nothing is synced for a mark that was not written");
        assert_eq!(fx.host.swaps.get(), 0);
        assert!(fx.untouched());
        assert!(!fx.journal_path().exists(), "no journal was created");
    }

    /// A crash after the exchange finds the durable mark on the new live bundle,
    /// so a relocated bundle cannot start as an uncommitted new version.
    #[cfg(unix)]
    #[test]
    fn a_crash_after_the_exchange_leaves_a_durable_mark_that_blocks_a_moved_bundle() {
        let fx = fixture();
        let applied = apply_bundle(&fx.inputs()).unwrap();
        drop(applied);
        assert_eq!(fx.host.syncs.get(), 1);
        let mark = fx.host.bundle_mark(fx.live()).unwrap();
        let parent = fx.placement.coordination_parent.clone();
        let err = check_relocation(mark.as_deref(), &moved_install_id(), &parent, fx.live())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);
    }

    #[test]
    fn a_failed_exchange_leaves_the_old_bundle_and_settles_the_journal() {
        let fx = fixture();
        fx.host.fail_swap.set(true);
        let err = apply_bundle(&fx.inputs()).err().unwrap();
        assert_eq!(err.code, ErrorCode::Io);
        assert!(fx.live_is_old());
        assert_eq!(fx.host.swaps.get(), 0);
        assert_eq!(fx.stored().phase, Phase::RolledBack);
    }

    #[test]
    fn an_exchange_that_did_not_take_effect_is_never_reported_as_applied() {
        let fx = fixture();
        fx.host.noop_swap.set(true);
        let err = apply_bundle(&fx.inputs()).err().unwrap();
        assert_eq!(err.code, ErrorCode::HashMismatch);
        assert!(fx.live_is_old());
        assert_eq!(fx.stored().phase, Phase::RolledBack);
    }

    #[test]
    fn a_running_application_prevents_every_change() {
        let fx = fixture();
        let mut inputs = fx.inputs();
        inputs.runtime_held = false;
        assert_eq!(apply_bundle(&inputs).err().unwrap().code, ErrorCode::NotApplicable);
        assert!(fx.untouched());
        assert!(!fx.slot().exists(), "nothing was even staged");
    }

    // ------------------------------------------------- refused before mutation

    #[test]
    fn a_build_without_an_update_key_cannot_apply() {
        let mut fx = fixture();
        fx.policy = TrustPolicy::production();
        let err = apply_bundle(&fx.inputs()).err().unwrap();
        assert_eq!(err.code, ErrorCode::TrustUnconfigured);
        assert!(fx.untouched());
    }

    /// The fixture has no Apple Developer ID, Team ID or notarization anywhere
    /// (the fake host asks for none), and the update still applies.
    #[test]
    fn an_update_applies_without_any_apple_identity() {
        let fx = fixture();
        let applied = apply_bundle(&fx.inputs()).ok().unwrap();
        assert_eq!(applied.journal().phase, Phase::AwaitingHealth);
        assert!(fx.live_is_new());
    }

    #[test]
    fn a_modified_archive_is_refused_before_any_mutation() {
        let fx = fixture();
        let archive = fx
            .coordination
            .transaction_dir(&fx.transaction)
            .join(CANDIDATE_DIR)
            .join(CANDIDATE_ARCHIVE);
        let mut bytes = fs::read(&archive).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
        fs::remove_file(&archive).unwrap();
        fs::write(&archive, bytes).unwrap();
        let err = apply_bundle(&fx.inputs()).err().unwrap();
        assert_eq!(err.code, ErrorCode::HashMismatch);
        assert!(fx.untouched());
    }

    #[test]
    fn too_little_or_unknown_free_space_refuses_before_any_receipt_journal_or_exchange() {
        for available in [Some(0), Some(JOURNAL_RESERVE_BYTES - 1), None] {
            let fx = fixture();
            fx.host.available.set(available);
            let err = apply_bundle(&fx.inputs()).err().unwrap();
            let expected = if available.is_some() { ErrorCode::InsufficientSpace } else { ErrorCode::Io };
            assert_eq!(err.code, expected, "{available:?}");
            assert!(fx.untouched(), "the install changed for {available:?}");
        }
    }

    #[test]
    fn exactly_the_journal_reserve_is_enough_to_apply() {
        let fx = fixture();
        fx.host.available.set(Some(JOURNAL_RESERVE_BYTES));
        let applied = apply_bundle(&fx.inputs()).ok().unwrap();
        assert_eq!(applied.journal().phase, Phase::AwaitingHealth);
        assert!(fx.live_is_new());
    }

    #[test]
    fn a_candidate_with_an_invalid_signature_is_refused_before_any_journal_or_exchange() {
        let fx = fixture();
        fx.host.bad_signature.borrow_mut().push(fx.slot());
        let err = apply_bundle(&fx.inputs()).err().unwrap();
        assert_eq!(err.code, ErrorCode::SignatureInvalid);
        assert!(fx.untouched());
    }

    #[test]
    fn an_installed_bundle_with_an_invalid_signature_is_not_replaced() {
        let fx = fixture();
        fx.host.bad_signature.borrow_mut().push(fx.live().to_path_buf());
        let err = apply_bundle(&fx.inputs()).err().unwrap();
        assert_eq!(err.code, ErrorCode::SignatureInvalid);
        assert!(fx.untouched());
    }

    #[test]
    fn a_candidate_that_disagrees_with_the_catalog_is_refused() {
        let wrong_id = plist(NEW, TEST_SOURCE_SHA, "com.example.other", 1);
        let wrong_version = plist("0.27.0", TEST_SOURCE_SHA, MACOS_BUNDLE_ID, 1);
        let wrong_commit = plist(NEW, &"b".repeat(40), MACOS_BUNDLE_ID, 1);
        let wrong_protocol = plist(NEW, TEST_SOURCE_SHA, MACOS_BUNDLE_ID, 2);
        for (info, code) in [
            (wrong_id, ErrorCode::IdentityMismatch),
            (wrong_version, ErrorCode::IdentityMismatch),
            (wrong_commit, ErrorCode::IdentityMismatch),
            (wrong_protocol, ErrorCode::ProtocolUnsupported),
        ] {
            let fx = fixture_with(&info);
            let err = apply_bundle(&fx.inputs()).err().unwrap();
            assert_eq!(err.code, code, "{info}");
            assert!(fx.untouched(), "{info}");
        }
    }

    #[test]
    fn a_placement_that_cannot_swap_is_refused_before_any_change() {
        type PlacementCase = (&'static str, fn(&FakeHost));
        let cases: [PlacementCase; 5] = [
            ("not APFS", |h| h.apfs.set(false)),
            ("not local", |h| h.local.set(false)),
            ("read only", |h| h.read_only.set(true)),
            ("no swap renaming", |h| h.swap_supported.set(false)),
            ("candidate on another volume", |h| h.slot_on_other_volume.set(true)),
        ];
        for (name, arrange) in cases {
            let fx = fixture();
            arrange(&fx.host);
            let err = apply_bundle(&fx.inputs()).err().unwrap();
            assert_eq!(err.code, ErrorCode::UnsupportedInstall, "{name}");
            assert!(fx.untouched(), "{name}");
        }
    }

    #[test]
    fn the_bundle_path_is_closed_to_other_targets() {
        let mut fx = fixture();
        fx.placement.target = Target::WindowsX64;
        let err = apply_bundle(&fx.inputs()).err().unwrap();
        assert_eq!(err.code, ErrorCode::UnsupportedInstall);
        assert!(fx.untouched());

        // Recovery of a bundle journal is refused for any other layout too.
        let mut fx = fixture();
        let mut journal = journal_at(&fx, Reached::Prepared);
        fx.placement.target = Target::WindowsX64;
        let outcome = recover(&fx, &mut journal);
        assert!(matches!(outcome, RecoveryOutcome::RecoveryRequired(_)), "{outcome:?}");
        assert_eq!(fx.host.swaps.get(), 0);
        assert!(fx.live_is_old());
    }

    // -------------------------------------------------------- recovery

    /// How far the journal got before the simulated interruption.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Reached {
        Prepared,
        /// `Applying`, step still `Planned`.
        Applying,
        /// `Applying`, intent durable.
        Intent,
        /// Exchange observed, `Applied` durable, still `Applying`.
        Applied,
        AwaitingHealth,
        Committed,
        RollingBack,
    }

    /// Stages the candidate through the real verification, then builds a
    /// journal that reached `reached`. The disk is left unswapped.
    fn journal_at(fx: &Fixture, reached: Reached) -> DurableJournal<FileJournalStore> {
        let verified = verify_and_plan(&fx.inputs()).unwrap();
        let journal = UpdateJournal::begin(&verified.plan, None).unwrap();
        let mut journal =
            DurableJournal::create(FileJournalStore::new(fx.journal_path()), journal).unwrap();
        if reached == Reached::Prepared {
            return journal;
        }
        journal.advance_phase(Phase::ExitAuthorized).unwrap();
        journal.advance_phase(Phase::Applying).unwrap();
        if reached == Reached::Applying {
            return journal;
        }
        journal.advance_step(0, StepState::Intent).unwrap();
        if reached == Reached::Intent {
            return journal;
        }
        journal.advance_step(0, StepState::Applied).unwrap();
        if reached == Reached::Applied {
            return journal;
        }
        journal.advance_phase(Phase::AwaitingHealth).unwrap();
        match reached {
            Reached::Committed => journal.advance_phase(Phase::Committed).unwrap(),
            Reached::RollingBack => journal.advance_phase(Phase::RollingBack).unwrap(),
            _ => {}
        }
        journal
    }

    fn recover(fx: &Fixture, journal: &mut DurableJournal<FileJournalStore>) -> RecoveryOutcome {
        recover_bundle(&fx.coordination, &fx.transaction, &fx.placement, &fx.host, journal).unwrap()
    }

    /// Exchanges live and slot directly, as an interrupted run would have.
    fn exchanged_before_the_interruption(fx: &Fixture) {
        rename_swap(fx.live(), &fx.slot());
    }

    #[test]
    fn an_interruption_before_the_exchange_keeps_the_old_bundle_without_exchanging() {
        for reached in [Reached::Prepared, Reached::Applying, Reached::Intent] {
            let fx = fixture();
            let mut journal = journal_at(&fx, reached);
            let outcome = recover(&fx, &mut journal);
            assert!(
                matches!(outcome, RecoveryOutcome::KeptOld | RecoveryOutcome::RolledBack),
                "{reached:?}: {outcome:?}"
            );
            assert!(fx.live_is_old(), "{reached:?}");
            assert_eq!(fx.host.swaps.get(), 0, "{reached:?}");
            assert_eq!(fx.stored().phase, Phase::RolledBack, "{reached:?}");
        }
    }

    #[test]
    fn an_interruption_after_the_exchange_exchanges_back_exactly_once() {
        // The journal may not have caught up with the disk at any of these points.
        for reached in [Reached::Intent, Reached::Applied, Reached::AwaitingHealth, Reached::RollingBack] {
            let fx = fixture();
            let mut journal = journal_at(&fx, reached);
            exchanged_before_the_interruption(&fx);
            assert_eq!(recover(&fx, &mut journal), RecoveryOutcome::RolledBack, "{reached:?}");
            assert!(fx.live_is_old(), "{reached:?}");
            assert_eq!(tree(&fx.slot()), files_tree(&fx.new_files), "{reached:?}");
            assert_eq!(fx.host.swaps.get(), 1, "{reached:?}: one rollback exchange");
            assert_eq!(fx.stored().phase, Phase::RolledBack, "{reached:?}");
            // A second recovery finds a settled journal and exchanges nothing.
            let mut again = DurableJournal::open(FileJournalStore::new(fx.journal_path())).unwrap().unwrap();
            let outcome = recover(&fx, &mut again);
            assert!(!matches!(outcome, RecoveryOutcome::RecoveryRequired(_)), "{reached:?}: {outcome:?}");
            assert_eq!(fx.host.swaps.get(), 1, "{reached:?}: never a double exchange");
        }
    }

    #[test]
    fn a_rollback_interrupted_after_its_exchange_does_not_exchange_again() {
        let fx = fixture();
        let mut journal = journal_at(&fx, Reached::RollingBack);
        // The disk is already back at the old bundle: only the journal lags.
        let outcome = recover(&fx, &mut journal);
        assert!(
            matches!(outcome, RecoveryOutcome::KeptOld | RecoveryOutcome::RolledBack),
            "{outcome:?}"
        );
        assert_eq!(fx.host.swaps.get(), 0);
        assert!(fx.live_is_old());
        assert_eq!(fx.stored().phase, Phase::RolledBack);
    }

    #[test]
    fn a_durable_commit_keeps_the_new_bundle_and_a_disk_that_disagrees_stops() {
        let fx = fixture();
        let mut journal = journal_at(&fx, Reached::Committed);
        exchanged_before_the_interruption(&fx);
        assert_eq!(recover(&fx, &mut journal), RecoveryOutcome::KeptNew);
        assert!(fx.live_is_new());
        assert_eq!(fx.host.swaps.get(), 0);

        // Committed, yet the old bundle is live: nothing is guessed or swapped.
        let fx = fixture();
        let mut journal = journal_at(&fx, Reached::Committed);
        assert!(matches!(recover(&fx, &mut journal), RecoveryOutcome::RecoveryRequired(_)));
        assert_eq!(fx.host.swaps.get(), 0);
        assert!(fx.live_is_old());
    }

    #[test]
    fn a_committed_receipt_rejects_a_bundle_changed_below_the_unchanged_seal() {
        let fx = fixture();
        let mut journal = journal_at(&fx, Reached::Committed);
        exchanged_before_the_interruption(&fx);
        let seal = fx.live().join("Contents/_CodeSignature/CodeResources");
        let seal_before = fs::read(&seal).unwrap();
        let resource = fx.live().join("Contents/Resources/data.bin");
        fs::create_dir_all(resource.parent().unwrap()).unwrap();
        fs::write(&resource, b"tampered resource").unwrap();
        assert_eq!(fs::read(&seal).unwrap(), seal_before);
        let outcome = recover(&fx, &mut journal);
        assert!(matches!(outcome, RecoveryOutcome::RecoveryRequired(_)), "{outcome:?}");
        assert_eq!(fx.host.swaps.get(), 0);
    }

    #[test]
    fn an_unknown_live_bundle_is_never_touched() {
        for reached in [Reached::Intent, Reached::AwaitingHealth] {
            for swapped in [false, true] {
                let fx = fixture();
                let mut journal = journal_at(&fx, reached);
                if swapped {
                    exchanged_before_the_interruption(&fx);
                }
                let seal = fx.live().join("Contents/_CodeSignature/CodeResources");
                fs::write(&seal, b"tampered").unwrap();
                let outcome = recover(&fx, &mut journal);
                assert!(
                    matches!(outcome, RecoveryOutcome::RecoveryRequired(_)),
                    "{reached:?} swapped={swapped}: {outcome:?}"
                );
                assert_eq!(fx.host.swaps.get(), 0, "{reached:?} swapped={swapped}");
                assert_eq!(fs::read(&seal).unwrap(), b"tampered", "{reached:?} swapped={swapped}");
                assert_eq!(fx.stored().phase, Phase::RecoveryRequired);
            }
        }
    }

    #[test]
    fn a_damaged_or_missing_backup_stops_the_rollback_without_an_exchange() {
        for remove in [false, true] {
            let fx = fixture();
            let mut journal = journal_at(&fx, Reached::AwaitingHealth);
            exchanged_before_the_interruption(&fx);
            if remove {
                fs::remove_dir_all(fx.slot()).unwrap();
            } else {
                fs::write(fx.slot().join("Contents/MacOS/hane"), b"damaged").unwrap();
            }
            let outcome = recover(&fx, &mut journal);
            assert!(matches!(outcome, RecoveryOutcome::RecoveryRequired(_)), "remove={remove}: {outcome:?}");
            assert_eq!(fx.host.swaps.get(), 0, "remove={remove}");
            assert!(fx.live_is_new(), "remove={remove}");
        }
    }

    #[test]
    fn an_exchange_without_a_recorded_intent_is_not_rolled_back_on_a_guess() {
        let fx = fixture();
        let mut journal = journal_at(&fx, Reached::Applying);
        exchanged_before_the_interruption(&fx);
        let outcome = recover(&fx, &mut journal);
        assert!(matches!(outcome, RecoveryOutcome::RecoveryRequired(_)), "{outcome:?}");
        assert_eq!(fx.host.swaps.get(), 0);
        assert!(fx.live_is_new());
    }

    #[test]
    fn a_journal_for_other_paths_is_rejected_by_the_adapter() {
        let fx = fixture();
        let mut ops = BundleOps::new(&fx.host, fx.live(), &fx.slot());
        assert!(ops.observe("other.app").is_err());
        assert!(ops.observe("backup/other.app").is_err());
        assert!(ops.remove(BUNDLE_REL).is_err());
        assert!(ops.backup(BUNDLE_REL, BACKUP_REL).is_err());
        assert!(ops.install_new(BUNDLE_REL, &FileIdentity::of(b"x")).is_err());
        assert!(ops.restore("other.app", BACKUP_REL, &FileIdentity::of(b"x")).is_err());
        assert_eq!(fx.host.swaps.get(), 0);
    }

    #[test]
    fn a_later_update_waits_for_the_terminal_receipt_to_match_the_live_bundle() {
        let fx = fixture();
        let mut launcher = FakeLauncher::new(&fx, Ok(()));
        apply_and_settle(&fx.inputs(), &mut launcher).unwrap();
        // Committed, but the live bundle changed afterwards: no new mutation may
        // build on a receipt that no longer describes the install.
        let view = BundleView::new(fx.live());
        assert!(crate::startup::ensure_prior_receipts_current(&fx.coordination, &view).is_ok());
        fs::write(fx.live().join("Contents/MacOS/hane"), b"changed later").unwrap();
        assert!(crate::startup::ensure_prior_receipts_current(&fx.coordination, &view).is_err());
    }
}

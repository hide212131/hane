use semver::Version;

use crate::error::{ErrorCode, Result, UpdateError};

/// Parses `MAJOR.MINOR.PATCH` without pre-release or build metadata.
pub fn parse_stable_version(text: &str) -> Result<Version> {
    let version = Version::parse(text).map_err(|err| {
        UpdateError::new(ErrorCode::NotStable, format!("invalid version {text:?}: {err}"))
    })?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err(UpdateError::new(
            ErrorCode::NotStable,
            format!("version {text:?} is not a plain stable version"),
        ));
    }
    Ok(version)
}

/// Parses a release tag of the form `vMAJOR.MINOR.PATCH`.
pub fn parse_release_tag(tag: &str) -> Result<Version> {
    let rest = tag.strip_prefix('v').ok_or_else(|| {
        UpdateError::new(ErrorCode::NotStable, format!("tag {tag:?} lacks the v prefix"))
    })?;
    parse_stable_version(rest)
}

/// The running version. Development builds and build-metadata versions are
/// not eligible for in-app update.
pub fn parse_current_version(text: &str) -> Result<Version> {
    let version = Version::parse(text).map_err(|err| {
        UpdateError::new(
            ErrorCode::UnsupportedInstall,
            format!("current version {text:?} is invalid: {err}"),
        )
    })?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err(UpdateError::new(
            ErrorCode::UnsupportedInstall,
            format!("current version {text:?} is a development build"),
        ));
    }
    Ok(version)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionRelation {
    Newer,
    Same,
    Older,
}

/// Relation of `candidate` to `current` by SemVer precedence (never by string
/// order, so `0.10.0` is newer than `0.9.0`).
pub fn relation(current: &Version, candidate: &Version) -> VersionRelation {
    match candidate.cmp(current) {
        std::cmp::Ordering::Greater => VersionRelation::Newer,
        std::cmp::Ordering::Equal => VersionRelation::Same,
        std::cmp::Ordering::Less => VersionRelation::Older,
    }
}

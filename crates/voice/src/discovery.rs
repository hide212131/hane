//! Finding the installed whisparrot.
//!
//! Hane does not ship the engine. The resident whisparrot app registers its own
//! absolute path and version at start in
//! `%LOCALAPPDATA%\whisparrot\engine-registration.json`, so a portable copy
//! works too:
//!
//! ```json
//! {"exePath":"C:\\...\\whisparrot.exe","version":"0.4.0","engine":true,"updatedAt":"..."}
//! ```
//!
//! A setting can name the executable explicitly instead (development and
//! verification). When nothing usable is found nothing is recorded: the reason
//! and where to go are shown in the status line.

use crate::transport::EngineSpec;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The oldest whisparrot release that speaks `voice/1`. The registration's
/// `engine` flag is what actually says the engine mode exists; this only
/// refuses builds older than the first one that could.
pub const MINIMUM_VERSION: (u64, u64, u64) = (0, 4, 0);

/// Where the registration is. `None` when the profile directory is unknown.
pub fn registration_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("whisparrot").join("engine-registration.json"))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Unavailable {
    /// No registration: whisparrot is not installed (or never started).
    NotInstalled,
    /// The registration exists but cannot be read as the agreed JSON.
    Unreadable,
    /// The registered executable is gone (moved or uninstalled).
    ExecutableMissing,
    /// The installed whisparrot has no engine mode.
    NoEngineMode,
    /// The installed whisparrot is older than the first engine release.
    OldVersion(String),
    /// The explicitly set executable does not exist.
    OverrideMissing,
}

impl Unavailable {
    /// Why recording cannot start and what to do, for the status line.
    pub fn message(&self) -> String {
        let help = "whisparrot をインストールまたは更新し、一度起動してから、もう一度お試しください";
        match self {
            Self::NotInstalled => format!("whisparrot が見つかりません。{help}"),
            Self::Unreadable => format!("whisparrot の登録情報を読めません。{help}"),
            Self::ExecutableMissing => {
                format!("登録された whisparrot の実行ファイルがありません。{help}")
            }
            Self::NoEngineMode => format!("この whisparrot は音声コンポジションに対応していません。{help}"),
            Self::OldVersion(version) => {
                format!("whisparrot {version} は古く、音声コンポジションに対応していません。{help}")
            }
            Self::OverrideMissing => {
                "設定 voice_engine_path の実行ファイルが見つかりません。パスを確認するか、設定を消して登録情報による検出に戻してください".to_owned()
            }
        }
    }
}

/// A version like `1.2.3` as numbers; a pre-release suffix is ignored.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let version = (parts.next()??, parts.next().unwrap_or(Some(0))?, parts.next().unwrap_or(Some(0))?);
    parts.next().is_none().then_some(version)
}

/// The engine to start: the explicit executable when one is set, else the
/// registered one after its checks.
pub fn discover(
    registration: Option<&Path>,
    explicit: Option<&Path>,
) -> Result<EngineSpec, Unavailable> {
    if let Some(path) = explicit {
        return if path.is_file() {
            Ok(EngineSpec::new(path))
        } else {
            Err(Unavailable::OverrideMissing)
        };
    }
    let path = registration.ok_or(Unavailable::NotInstalled)?;
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Unavailable::NotInstalled);
        }
        Err(_) => return Err(Unavailable::Unreadable),
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&contents) else {
        return Err(Unavailable::Unreadable);
    };
    let exe = map
        .get("exePath")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(Unavailable::Unreadable)?;
    let version = map
        .get("version")
        .and_then(Value::as_str)
        .ok_or(Unavailable::Unreadable)?;
    if map.get("engine").and_then(Value::as_bool) != Some(true) {
        return Err(Unavailable::NoEngineMode);
    }
    match parse_version(version) {
        Some(parsed) if parsed >= MINIMUM_VERSION => {}
        _ => return Err(Unavailable::OldVersion(version.chars().take(32).collect())),
    }
    if !exe.is_file() {
        return Err(Unavailable::ExecutableMissing);
    }
    Ok(EngineSpec::new(exe))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "hane-voice-discovery-{tag}-{}",
                hane_link::auth::random_hex(8).unwrap()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn registration(scratch: &Scratch, exe: &Path, version: &str, engine: bool) -> PathBuf {
        let body = serde_json::json!({
            "exePath": exe, "version": version, "engine": engine, "updatedAt": "2026-11-26T00:00:00Z"
        });
        scratch.file("engine-registration.json", &body.to_string())
    }

    #[test]
    fn a_valid_registration_names_the_executable() {
        let scratch = Scratch::new("ok");
        let exe = scratch.file("whisparrot.exe", "");
        let reg = registration(&scratch, &exe, "0.4.1", true);
        assert_eq!(discover(Some(&reg), None).unwrap().exe, exe);
        // A pre-release suffix and a newer major are accepted.
        let reg = registration(&scratch, &exe, "1.0.0-preview.2", true);
        assert!(discover(Some(&reg), None).is_ok());
    }

    #[test]
    fn each_unusable_state_has_its_own_reason_and_a_pointer_to_install_or_update() {
        let scratch = Scratch::new("bad");
        let exe = scratch.file("whisparrot.exe", "");
        assert_eq!(discover(None, None).unwrap_err(), Unavailable::NotInstalled);
        assert_eq!(
            discover(Some(&scratch.0.join("missing.json")), None).unwrap_err(),
            Unavailable::NotInstalled
        );
        let garbled = scratch.file("garbled.json", "{ not json");
        assert_eq!(discover(Some(&garbled), None).unwrap_err(), Unavailable::Unreadable);
        let relative = scratch.file("rel.json", r#"{"exePath":"whisparrot.exe","version":"0.4.0","engine":true}"#);
        assert_eq!(discover(Some(&relative), None).unwrap_err(), Unavailable::Unreadable);
        let reg = registration(&scratch, &exe, "0.4.0", false);
        assert_eq!(discover(Some(&reg), None).unwrap_err(), Unavailable::NoEngineMode);
        let reg = registration(&scratch, &exe, "0.3.9", true);
        assert_eq!(
            discover(Some(&reg), None).unwrap_err(),
            Unavailable::OldVersion("0.3.9".into())
        );
        let reg = registration(&scratch, &exe, "bogus", true);
        assert!(matches!(discover(Some(&reg), None), Err(Unavailable::OldVersion(_))));
        let reg = registration(&scratch, &scratch.0.join("gone.exe"), "0.4.0", true);
        assert_eq!(discover(Some(&reg), None).unwrap_err(), Unavailable::ExecutableMissing);
        for reason in [
            Unavailable::NotInstalled,
            Unavailable::Unreadable,
            Unavailable::ExecutableMissing,
            Unavailable::NoEngineMode,
            Unavailable::OldVersion("0.3.9".into()),
        ] {
            assert!(reason.message().contains("インストール"), "{reason:?}");
        }
    }

    #[test]
    fn an_explicit_executable_wins_over_the_registration() {
        let scratch = Scratch::new("override");
        let exe = scratch.file("whisparrot.exe", "");
        let other = scratch.file("dev-engine", "");
        let reg = registration(&scratch, &exe, "0.4.0", true);
        assert_eq!(discover(Some(&reg), Some(&other)).unwrap().exe, other);
        assert_eq!(
            discover(Some(&reg), Some(&scratch.0.join("nope"))).unwrap_err(),
            Unavailable::OverrideMissing
        );
        // Even with no registration at all.
        assert!(discover(None, Some(&other)).is_ok());
    }
}

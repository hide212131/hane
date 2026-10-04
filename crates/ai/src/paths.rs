//! Layout of the AI-specific state Hane keeps under its app data directory,
//! per `docs/adr/0032-embedded-codex-app-server-ai-foundation.md` section 8:
//!
//! ```text
//! <Hane app data>/ai/
//!   ai-settings.json
//!   ai-settings.lock
//!   credential-journal.json
//!   runtime-owner.lock
//!   codex-chatgpt/    <- ChatGPT connection's CODEX_HOME
//!   codex-custom/     <- Custom Provider connection's CODEX_HOME
//!   probe-workspace/  <- connectivity-probe-only empty working directory
//! ```
//!
//! Both connections' `CODEX_HOME` are kept separate so ChatGPT credentials
//! and Custom Provider state never mix, and neither ever runs with the
//! user's document folder as its working directory (`probe_workspace`, not a
//! document path, is the connectivity probe's `cwd`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
pub struct AiPaths {
    root: PathBuf,
}

impl AiPaths {
    pub fn new(app_data_root: impl AsRef<Path>) -> Self {
        AiPaths {
            root: app_data_root.as_ref().join("ai"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn settings_path(&self) -> PathBuf {
        self.root.join("ai-settings.json")
    }

    pub fn settings_lock_path(&self) -> PathBuf {
        self.root.join("ai-settings.lock")
    }

    pub fn credential_journal_path(&self) -> PathBuf {
        self.root.join("credential-journal.json")
    }

    pub fn runtime_owner_lock_path(&self) -> PathBuf {
        self.root.join("runtime-owner.lock")
    }

    pub fn chatgpt_codex_home(&self) -> PathBuf {
        self.root.join("codex-chatgpt")
    }

    pub fn custom_codex_home(&self) -> PathBuf {
        self.root.join("codex-custom")
    }

    /// The connectivity probe's dedicated, otherwise-empty working
    /// directory. Never the user's document folder: the probe must not see
    /// (and Codex must not be able to read/modify) any open document.
    pub fn probe_workspace(&self) -> PathBuf {
        self.root.join("probe-workspace")
    }

    /// Creates a fresh, empty, per-runtime workspace. A unique directory
    /// keeps an earlier run's files or an adjacent user's project from
    /// becoming the next App Server's cwd. The directory is under Hane app
    /// data and is never a document path.
    pub fn create_probe_workspace(&self) -> std::io::Result<PathBuf> {
        static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);
        let base = self.probe_workspace();
        std::fs::create_dir_all(&base)?;
        loop {
            let sequence = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
            let candidate = base.join(format!("runtime-{}-{sequence}", std::process::id()));
            match std::fs::create_dir(&candidate) {
                Ok(()) => return Ok(candidate),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    /// Removes runtime workspaces left by a prior process after an abnormal
    /// exit. The caller must hold the runtime owner lock and have no active
    /// child, which proves that no App Server can still be using these paths.
    pub fn cleanup_probe_workspaces(&self) -> std::io::Result<()> {
        let base = self.probe_workspace();
        let entries = match std::fs::read_dir(&base) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && entry.file_name().to_string_lossy().starts_with("runtime-")
            {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_scoped_under_an_ai_subdirectory_and_kept_distinct() {
        let paths = AiPaths::new("/tmp/hane-app-data");
        assert_eq!(paths.root(), Path::new("/tmp/hane-app-data/ai"));
        assert_eq!(
            paths.settings_path(),
            Path::new("/tmp/hane-app-data/ai/ai-settings.json")
        );
        assert_ne!(paths.chatgpt_codex_home(), paths.custom_codex_home());
        assert_ne!(paths.probe_workspace(), paths.chatgpt_codex_home());
        assert_ne!(paths.probe_workspace(), paths.custom_codex_home());
    }

    #[test]
    fn each_runtime_workspace_is_new_and_empty() {
        let root = std::env::temp_dir().join(format!("hane-ai-paths-{}", std::process::id()));
        let paths = AiPaths::new(&root);
        let first = paths.create_probe_workspace().unwrap();
        let second = paths.create_probe_workspace().unwrap();

        assert_ne!(first, second);
        assert!(first.starts_with(paths.probe_workspace()));
        assert!(std::fs::read_dir(&first).unwrap().next().is_none());
        assert!(std::fs::read_dir(&second).unwrap().next().is_none());
    }

    #[test]
    fn stale_runtime_workspaces_are_removed_without_touching_other_entries() {
        let root =
            std::env::temp_dir().join(format!("hane-ai-path-cleanup-{}", std::process::id()));
        let paths = AiPaths::new(&root);
        let stale = paths.create_probe_workspace().unwrap();
        std::fs::write(stale.join("stale.txt"), "fixture").unwrap();
        let unrelated = paths.probe_workspace().join("keep-me");
        std::fs::create_dir_all(&unrelated).unwrap();

        paths.cleanup_probe_workspaces().unwrap();

        assert!(!stale.exists());
        assert!(unrelated.is_dir());
    }
}

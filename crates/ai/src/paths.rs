//! Layout of the AI-specific state Hane keeps under its app data directory,
//! per `docs/adr/0032-embedded-codex-app-server-ai-foundation.md` section 8:
//!
//! ```text
//! <Hane app data>/ai/
//!   ai-settings.json
//!   ai-settings.lock
//!   credential-journal.json
//!   runtime-owner.lock
//!   user-prompts.json          <- saved selection-action prompts (crate::prompts)
//!   user-prompts.lock
//!   codex-chatgpt/    <- ChatGPT connection's CODEX_HOME
//!   codex-custom/     <- Custom Provider connection's CODEX_HOME
//!   probe-workspace/           <- connectivity-probe-only empty working directory
//!   text-transform-workspace/  <- selection text-transform-only empty working directory
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

    /// Saved selection-action prompts (`crate::prompts`), independent of
    /// `AiSettings`: a connection/credential change must never touch this
    /// file, and this file must never affect `settings_generation` or
    /// runtime restart decisions.
    pub fn user_prompts_path(&self) -> PathBuf {
        self.root.join("user-prompts.json")
    }

    pub fn user_prompts_lock_path(&self) -> PathBuf {
        self.root.join("user-prompts.lock")
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
        create_prefixed_workspace(&self.probe_workspace(), "runtime")
    }

    /// Removes runtime workspaces left by a prior process after an abnormal
    /// exit. The caller must hold the runtime owner lock and have no active
    /// child, which proves that no App Server can still be using these paths.
    pub fn cleanup_probe_workspaces(&self) -> std::io::Result<()> {
        cleanup_prefixed_workspaces(&self.probe_workspace())
    }

    /// A dedicated, otherwise-empty working directory for one selection text
    /// transform request (`crate::text_transform`). Separate from
    /// `probe_workspace` so the fixed connectivity Probe's own workspace
    /// directory and cleanup sweep are never shared with (or affected by) a
    /// capability that, unlike Probe, sends user-selected document text.
    /// Never the user's document folder or current working directory.
    pub fn text_transform_workspace(&self) -> PathBuf {
        self.root.join("text-transform-workspace")
    }

    /// Creates a fresh, empty, per-request workspace for one text transform
    /// turn. See `create_probe_workspace` for why a unique directory matters.
    pub fn create_text_transform_workspace(&self) -> std::io::Result<PathBuf> {
        create_prefixed_workspace(&self.text_transform_workspace(), "run")
    }

    /// Removes text transform workspaces left by a prior process after an
    /// abnormal exit. Same precondition as `cleanup_probe_workspaces`.
    pub fn cleanup_text_transform_workspaces(&self) -> std::io::Result<()> {
        cleanup_prefixed_workspaces(&self.text_transform_workspace())
    }
}

fn create_prefixed_workspace(base: &Path, prefix: &str) -> std::io::Result<PathBuf> {
    static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(base)?;
    loop {
        let sequence = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
        let candidate = base.join(format!("{prefix}-{}-{sequence}", std::process::id()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

fn cleanup_prefixed_workspaces(base: &Path) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(base) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("runtime-") || name.starts_with("run-") {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
    }
    Ok(())
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

    #[test]
    fn text_transform_workspace_is_distinct_from_the_probe_workspace_and_cleans_up_independently() {
        let root = std::env::temp_dir()
            .join(format!("hane-ai-text-transform-paths-{}", std::process::id()));
        let paths = AiPaths::new(&root);
        let probe = paths.create_probe_workspace().unwrap();
        let transform = paths.create_text_transform_workspace().unwrap();

        assert_ne!(paths.text_transform_workspace(), paths.probe_workspace());
        assert!(transform.starts_with(paths.text_transform_workspace()));
        assert!(std::fs::read_dir(&transform).unwrap().next().is_none());

        std::fs::write(transform.join("stale.txt"), "fixture").unwrap();
        paths.cleanup_text_transform_workspaces().unwrap();
        assert!(!transform.exists());
        // The unrelated probe workspace is untouched by the text transform
        // cleanup sweep.
        assert!(probe.exists());
    }
}

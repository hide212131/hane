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
}

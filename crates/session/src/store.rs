use crate::service::atomic_write_bytes;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const RECENT_LIMIT: usize = 10;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::System => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::System,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    pub autosave: bool,
    pub theme: ThemePreference,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            autosave: true,
            theme: ThemePreference::System,
        }
    }
}

/// Most-recently-opened files, newest first, deduplicated by path and bounded.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecentFiles {
    entries: Vec<PathBuf>,
}

impl RecentFiles {
    pub fn from_paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut recent = Self::default();
        for path in paths {
            recent.entries.retain(|entry| entry != &path);
            recent.entries.push(path);
        }
        recent.entries.truncate(RECENT_LIMIT);
        recent
    }

    pub fn remember(&mut self, path: &Path) {
        self.entries.retain(|entry| entry != path);
        self.entries.insert(0, path.to_path_buf());
        self.entries.truncate(RECENT_LIMIT);
    }

    /// Drops one entry after a delete, so a filer deletion does not leave a
    /// dangling item in the list until the next load.
    pub fn forget(&mut self, path: &Path) {
        self.entries.retain(|entry| entry != path);
    }

    /// Follows a rename in place, keeping the entry's position in the list.
    pub fn rename(&mut self, from: &Path, to: &Path) {
        for entry in &mut self.entries {
            if entry == from {
                *entry = to.to_path_buf();
            }
        }
    }

    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Persisted user settings, independent of any window or session lifetime.
pub trait SettingsRepository: Send + Sync + 'static {
    fn load(&self) -> Settings;
    fn store(&self, settings: &Settings) -> io::Result<()>;
}

/// Persisted recent-file list. Deliberately separate from settings and from any
/// future filer tree state so those can move or grow independently.
pub trait RecentFilesRepository: Send + Sync + 'static {
    fn load(&self) -> RecentFiles;
    fn store(&self, recent: &RecentFiles) -> io::Result<()>;
}

/// The persistent stores an editor window needs, as shared handles. Cloning is
/// cheap and the stores outlive any single view, so nothing here depends on a
/// window's lifecycle.
#[derive(Clone)]
pub struct StateStores {
    settings: Arc<dyn SettingsRepository>,
    recent: Arc<dyn RecentFilesRepository>,
}

impl StateStores {
    pub fn new(
        settings: Arc<dyn SettingsRepository>,
        recent: Arc<dyn RecentFilesRepository>,
    ) -> Self {
        Self { settings, recent }
    }

    /// Stores backed by the per-user state directory, or in-memory stores when
    /// that directory cannot be resolved.
    pub fn from_environment() -> Self {
        match FileStateStore::from_environment() {
            Ok(store) => {
                let store = Arc::new(store);
                Self::new(store.clone(), store)
            }
            Err(_) => Self::memory(),
        }
    }

    pub fn memory() -> Self {
        let store = Arc::new(MemoryStateStore::default());
        Self::new(store.clone(), store)
    }

    pub fn settings(&self) -> &dyn SettingsRepository {
        self.settings.as_ref()
    }

    pub fn recent_files(&self) -> &dyn RecentFilesRepository {
        self.recent.as_ref()
    }
}

impl std::fmt::Debug for StateStores {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("StateStores")
    }
}

/// Settings and recent files as small text files under the state directory.
#[derive(Clone, Debug)]
pub struct FileStateStore {
    root: PathBuf,
}

impl FileStateStore {
    pub fn from_environment() -> io::Result<Self> {
        state_directory().map(Self::at)
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl SettingsRepository for FileStateStore {
    fn load(&self) -> Settings {
        let mut settings = Settings::default();
        if let Ok(contents) = fs::read_to_string(self.root.join("settings.conf")) {
            for line in contents.lines() {
                if let Some(value) = line.strip_prefix("autosave=") {
                    settings.autosave = value != "false";
                } else if let Some(value) = line.strip_prefix("theme=") {
                    settings.theme = ThemePreference::parse(value);
                }
            }
        }
        settings
    }

    fn store(&self, settings: &Settings) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        atomic_write_bytes(
            &self.root.join("settings.conf"),
            format!(
                "autosave={}\ntheme={}\n",
                settings.autosave,
                settings.theme.as_str()
            )
            .as_bytes(),
        )
    }
}

impl RecentFilesRepository for FileStateStore {
    fn load(&self) -> RecentFiles {
        let Ok(contents) = fs::read_to_string(self.root.join("recent-files")) else {
            return RecentFiles::default();
        };
        RecentFiles::from_paths(
            contents
                .lines()
                .map(unescape_line)
                .map(PathBuf::from)
                .filter(|path| path.is_file()),
        )
    }

    fn store(&self, recent: &RecentFiles) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let contents = recent
            .entries()
            .iter()
            .map(|path| escape_line(&path.to_string_lossy()))
            .collect::<Vec<_>>()
            .join("\n");
        atomic_write_bytes(&self.root.join("recent-files"), contents.as_bytes())
    }
}

/// In-memory stores for tests and for the case where no state directory exists.
#[derive(Debug, Default)]
pub struct MemoryStateStore {
    settings: std::sync::Mutex<Settings>,
    recent: std::sync::Mutex<RecentFiles>,
}

impl SettingsRepository for MemoryStateStore {
    fn load(&self) -> Settings {
        self.settings.lock().expect("settings lock").clone()
    }

    fn store(&self, settings: &Settings) -> io::Result<()> {
        *self.settings.lock().expect("settings lock") = settings.clone();
        Ok(())
    }
}

impl RecentFilesRepository for MemoryStateStore {
    fn load(&self) -> RecentFiles {
        self.recent.lock().expect("recent lock").clone()
    }

    fn store(&self, recent: &RecentFiles) -> io::Result<()> {
        *self.recent.lock().expect("recent lock") = recent.clone();
        Ok(())
    }
}

fn state_directory() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("HANE_STATE_DIR") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    #[cfg(target_os = "macos")]
    return Ok(home.join("Library/Application Support/Hane"));
    #[cfg(not(target_os = "macos"))]
    Ok(std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("hane"))
}

fn escape_line(path: &str) -> String {
    path.replace('\\', "\\\\").replace('\n', "\\n")
}

fn unescape_line(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            match chars.next() {
                Some('n') => output.push('\n'),
                Some(next) => output.push(next),
                None => output.push('\\'),
            }
        } else {
            output.push(character);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::temporary_directory;

    #[test]
    fn recent_files_are_deduplicated_and_bounded() {
        let mut recent = RecentFiles::default();
        for index in 0..12 {
            recent.remember(Path::new(&format!("/{index}.md")));
        }
        recent.remember(Path::new("/5.md"));
        assert_eq!(recent.entries().len(), RECENT_LIMIT);
        assert_eq!(recent.entries()[0], Path::new("/5.md"));
        assert_eq!(
            recent
                .entries()
                .iter()
                .filter(|path| *path == Path::new("/5.md"))
                .count(),
            1
        );
    }

    #[test]
    fn recent_files_follow_renames_and_drop_deletions() {
        let mut recent = RecentFiles::default();
        recent.remember(Path::new("/b.md"));
        recent.remember(Path::new("/a.md"));
        recent.rename(Path::new("/b.md"), Path::new("/c.md"));
        assert_eq!(recent.entries()[1], Path::new("/c.md"));
        recent.forget(Path::new("/a.md"));
        assert_eq!(recent.entries(), [PathBuf::from("/c.md")]);
    }

    #[test]
    fn recent_path_escaping_round_trips() {
        let path = "folder\\name\nnotes.md";
        assert_eq!(unescape_line(&escape_line(path)), path);
    }

    #[test]
    fn settings_survive_a_store_reload() {
        let root = temporary_directory("state");
        let store = FileStateStore::at(&root);
        let settings = Settings {
            autosave: false,
            theme: ThemePreference::Dark,
        };
        SettingsRepository::store(&store, &settings).unwrap();
        assert_eq!(
            SettingsRepository::load(&FileStateStore::at(&root)),
            settings
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stores_outlive_the_component_that_wrote_them() {
        let stores = StateStores::memory();
        {
            let borrowed = stores.clone();
            borrowed
                .settings()
                .store(&Settings {
                    autosave: false,
                    theme: ThemePreference::Light,
                })
                .unwrap();
        }
        assert!(!stores.settings().load().autosave);
    }
}

use crate::pins::{FilePinStore, MemoryPinStore, PinRepository};
use crate::service::atomic_write_bytes;
use crate::workfolder::WorkFolderSortOrder;
use hane_network::{
    CredentialRef, ManualProxy, NetworkSettings, ProxyAuth, ProxyMode, SETTINGS_SCHEMA,
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

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
    /// The folder an ordinary launch (no path argument, e.g. no Explorer
    /// "Open with Hane" target) opens automatically. `None` until the
    /// first-run folder picker or a settings screen sets one; a launch with
    /// an explicit path argument never reads or writes this field, so it
    /// stays a one-off open rather than changing what "ordinary launch"
    /// means.
    pub default_folder: Option<PathBuf>,
    /// How the work-folder sidebar orders files and folders. Defaults to
    /// `UpdatedNewest`, so a settings file predating this setting — or
    /// simply missing the `sidebar_sort` line — gets that same order rather
    /// than an unspecified one.
    pub sidebar_sort: WorkFolderSortOrder,
    /// App-wide proxy settings (`network_*` keys). A file without those keys
    /// migrates to the Environment mode; a stored value that cannot be
    /// understood is kept as [`NetworkConfig::Invalid`] instead of becoming
    /// a working Direct setting.
    pub network: NetworkConfig,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            autosave: true,
            theme: ThemePreference::System,
            default_folder: None,
            sidebar_sort: WorkFolderSortOrder::default(),
            network: NetworkConfig::default(),
        }
    }
}

/// The persisted network settings as read from `settings.conf`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkConfig {
    Valid(NetworkSettings),
    /// Unknown schema or mode, a broken endpoint, ... The original lines are
    /// kept so that storing other settings does not erase them; only an
    /// explicit replacement from the settings screen overwrites them.
    Invalid(InvalidNetworkConfig),
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self::Valid(NetworkSettings::default())
    }
}

impl NetworkConfig {
    pub fn valid(&self) -> Option<&NetworkSettings> {
        match self {
            Self::Valid(settings) => Some(settings),
            Self::Invalid(_) => None,
        }
    }

    pub fn invalid_reason(&self) -> Option<&str> {
        match self {
            Self::Valid(_) => None,
            Self::Invalid(invalid) => Some(invalid.reason()),
        }
    }

    /// The settings a connection must use. An invalid stored configuration
    /// maps to settings whose schema is rejected, so every route resolution
    /// fails with an explicit error and never falls back to a direct or
    /// environment-derived connection.
    pub fn effective_settings(&self) -> NetworkSettings {
        match self {
            Self::Valid(settings) => settings.clone(),
            Self::Invalid(_) => NetworkSettings {
                schema: 0,
                ..NetworkSettings::default()
            },
        }
    }
}

/// Raw `network_*` lines that could not be understood, with a human readable
/// reason. The reason never contains the stored values.
#[derive(Clone, Eq, PartialEq)]
pub struct InvalidNetworkConfig {
    raw_lines: Vec<String>,
    reason: String,
    /// Set when `settings.conf` itself could not be read. The file content is
    /// unknown, so no store may replace it except an explicit valid network
    /// replacement.
    read_failure: Option<io::ErrorKind>,
}

impl InvalidNetworkConfig {
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// The error kind of the failed read of `settings.conf`, if that was the cause.
    pub fn read_failure(&self) -> Option<io::ErrorKind> {
        self.read_failure
    }
}

impl std::fmt::Debug for InvalidNetworkConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InvalidNetworkConfig")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

fn parse_network_config(raw_lines: Vec<String>) -> NetworkConfig {
    if raw_lines.is_empty() {
        return NetworkConfig::default();
    }
    match parse_network_lines(&raw_lines) {
        Ok(settings) => NetworkConfig::Valid(settings),
        Err(reason) => NetworkConfig::Invalid(InvalidNetworkConfig {
            raw_lines,
            reason,
            read_failure: None,
        }),
    }
}

/// The stored proxy settings could not be read at all. Storing any other
/// setting while this state is held fails without touching the file, so the
/// unreadable original is never replaced by defaults.
fn unreadable_network_config(kind: io::ErrorKind) -> NetworkConfig {
    let reason = if kind == io::ErrorKind::InvalidData {
        "保存済みの設定ファイルが正しい文字コードではないため、プロキシ設定を読み込めません"
    } else {
        "保存済みの設定ファイルを読み込めないため、プロキシ設定を確認できません"
    };
    NetworkConfig::Invalid(InvalidNetworkConfig {
        raw_lines: Vec::new(),
        reason: reason.to_owned(),
        read_failure: Some(kind),
    })
}

fn parse_network_lines(lines: &[String]) -> Result<NetworkSettings, String> {
    let mut schema = None;
    let mut mode = None;
    let mut url = None;
    let mut bypass = None;
    let mut auth = None;
    let mut credential = None;
    for line in lines {
        let Some((key, value)) = line.split_once('=') else {
            return Err("ネットワーク設定の行の形式が不正です".to_owned());
        };
        match key {
            "network_schema" => schema = Some(value),
            "network_mode" => mode = Some(value),
            "network_manual_url" => url = Some(value),
            "network_manual_bypass" => bypass = Some(value),
            "network_manual_auth" => auth = Some(value),
            "network_manual_credential_ref" => credential = Some(value),
            _ => {}
        }
    }
    let schema = match schema {
        None => SETTINGS_SCHEMA,
        Some(text) => text
            .parse::<u32>()
            .map_err(|_| "ネットワーク設定のschemaが不正です".to_owned())?,
    };
    if schema != SETTINGS_SCHEMA {
        return Err(format!("未対応のネットワーク設定schemaです（{schema}）"));
    }
    match mode {
        None if url.is_none() && bypass.is_none() && auth.is_none() && credential.is_none() => {
            Ok(NetworkSettings::default())
        }
        None => Err("ネットワーク設定にプロキシ方式がありません".to_owned()),
        Some("environment") => Ok(NetworkSettings::default()),
        Some("direct") => Ok(NetworkSettings::direct()),
        Some("manual") => {
            let url = url.ok_or_else(|| "手動プロキシのURLが保存されていません".to_owned())?;
            let auth = match auth.unwrap_or("none") {
                "none" => ProxyAuth::None,
                "basic" => {
                    let reference = credential
                        .ok_or_else(|| "Basic認証の資格情報参照が保存されていません".to_owned())?;
                    ProxyAuth::Basic(
                        CredentialRef::new(reference).map_err(|error| error.to_string())?,
                    )
                }
                _ => return Err("未知のプロキシ認証方式です".to_owned()),
            };
            ManualProxy::new(url, bypass.unwrap_or(""), auth)
                .map(NetworkSettings::manual)
                .map_err(|error| error.to_string())
        }
        Some(_) => Err("未知のプロキシ方式です".to_owned()),
    }
}

fn network_lines(config: &NetworkConfig) -> Vec<String> {
    let settings = match config {
        NetworkConfig::Invalid(invalid) => return invalid.raw_lines.clone(),
        NetworkConfig::Valid(settings) => settings,
    };
    let mut lines = vec![format!("network_schema={}", settings.schema)];
    match (settings.mode, &settings.manual) {
        (ProxyMode::Environment, _) => lines.push("network_mode=environment".to_owned()),
        (ProxyMode::Direct, _) => lines.push("network_mode=direct".to_owned()),
        (ProxyMode::Manual, Some(manual)) => {
            lines.push("network_mode=manual".to_owned());
            lines.push(format!("network_manual_url={}", manual.endpoint()));
            lines.push(format!("network_manual_bypass={}", manual.bypass().to_text()));
            match manual.auth() {
                ProxyAuth::None => lines.push("network_manual_auth=none".to_owned()),
                ProxyAuth::Basic(reference) => {
                    lines.push("network_manual_auth=basic".to_owned());
                    lines.push(format!(
                        "network_manual_credential_ref={}",
                        reference.as_str()
                    ));
                }
            }
        }
        // Manual without an endpoint cannot be reloaded; the screen never
        // produces it, and a missing mode line would silently mean
        // Environment, so the incomplete value is stored as an explicit
        // manual mode that loads back as a settings error.
        (ProxyMode::Manual, None) => lines.push("network_mode=manual".to_owned()),
    }
    lines
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

    /// Follows a folder rename, moving every entry at or under `from` to the
    /// same relative position under `to` in place. An entry outside `from` is
    /// left untouched.
    pub fn rename_folder(&mut self, from: &Path, to: &Path) {
        for entry in &mut self.entries {
            if let Ok(relative) = entry.strip_prefix(from) {
                let moved = if relative.as_os_str().is_empty() {
                    to.to_path_buf()
                } else {
                    to.join(relative)
                };
                *entry = moved;
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
    pins: Arc<dyn PinRepository>,
}

impl StateStores {
    /// Settings and recent files over the given repositories, with pins kept
    /// in memory. Use [`StateStores::with_pins`] to persist them as well.
    pub fn new(
        settings: Arc<dyn SettingsRepository>,
        recent: Arc<dyn RecentFilesRepository>,
    ) -> Self {
        Self::with_pins(settings, recent, Arc::new(MemoryPinStore::default()))
    }

    pub fn with_pins(
        settings: Arc<dyn SettingsRepository>,
        recent: Arc<dyn RecentFilesRepository>,
        pins: Arc<dyn PinRepository>,
    ) -> Self {
        Self {
            settings,
            recent,
            pins,
        }
    }

    /// Stores backed by the per-user state directory, or in-memory stores when
    /// that directory cannot be resolved.
    pub fn from_environment() -> Self {
        match FileStateStore::from_environment() {
            Ok(store) => {
                let pins = Arc::new(FilePinStore::at(store.root()));
                let store = Arc::new(store);
                Self::with_pins(store.clone(), store, pins)
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

    /// A shared handle, so a background task can load or update pins without
    /// borrowing the view.
    pub fn pins(&self) -> Arc<dyn PinRepository> {
        self.pins.clone()
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

    /// Directory used for per-user Hane state. The application uses the same
    /// root for state owned by other process-level services.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SettingsRepository for FileStateStore {
    fn load(&self) -> Settings {
        settings_from_read(fs::read_to_string(self.root.join("settings.conf")))
    }

    fn store(&self, settings: &Settings) -> io::Result<()> {
        if let NetworkConfig::Invalid(invalid) = &settings.network {
            if let Some(kind) = invalid.read_failure() {
                return Err(io::Error::new(kind, invalid.reason().to_owned()));
            }
        }
        fs::create_dir_all(&self.root)?;
        let mut contents = format!(
            "autosave={}\ntheme={}\nsidebar_sort={}\n",
            settings.autosave,
            settings.theme.as_str(),
            settings.sidebar_sort.as_str()
        );
        if let Some(folder) = &settings.default_folder {
            contents.push_str("default_folder=");
            contents.push_str(&escape_line(&folder.to_string_lossy()));
            contents.push('\n');
        }
        for line in network_lines(&settings.network) {
            contents.push_str(&line);
            contents.push('\n');
        }
        atomic_write_bytes(&self.root.join("settings.conf"), contents.as_bytes())
    }
}

/// Builds settings from the result of reading `settings.conf`. A missing file
/// is a first launch and uses the defaults; any other read failure keeps the
/// network configuration invalid instead of falling back to Environment.
fn settings_from_read(read: io::Result<String>) -> Settings {
    let mut settings = Settings::default();
    match read {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            settings.network = unreadable_network_config(error.kind());
        }
        Ok(contents) => {
            let mut network = Vec::new();
            for line in contents.lines() {
                if line.starts_with("network_") {
                    network.push(line.to_owned());
                    continue;
                }
                if let Some(value) = line.strip_prefix("autosave=") {
                    settings.autosave = value != "false";
                } else if let Some(value) = line.strip_prefix("theme=") {
                    settings.theme = ThemePreference::parse(value);
                } else if let Some(value) = line.strip_prefix("default_folder=") {
                    settings.default_folder = Some(PathBuf::from(unescape_line(value)));
                } else if let Some(value) = line.strip_prefix("sidebar_sort=") {
                    settings.sidebar_sort = WorkFolderSortOrder::parse(value);
                }
            }
            settings.network = parse_network_config(network);
        }
    }
    settings
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

static PINNED_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Fixes the state directory of this process to a root verified elsewhere
/// (the update helper's limited child, which has no environment to resolve one
/// from). It must be called before any store is opened; once pinned, neither
/// `HANE_STATE_DIR` nor the per-user default is consulted. Returns `false` and
/// changes nothing when a root was already pinned.
pub fn pin_state_root(root: PathBuf) -> bool {
    PINNED_ROOT.set(root).is_ok()
}

fn state_directory() -> io::Result<PathBuf> {
    if let Some(root) = PINNED_ROOT.get() {
        return Ok(root.clone());
    }
    if let Some(path) = std::env::var_os("HANE_STATE_DIR") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        Ok(home.join("Library/Application Support/Hane"))
    }
    #[cfg(target_os = "windows")]
    {
        let root = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join("AppData/Local"))
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set"))?;
        Ok(root.join("Hane"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
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
    fn recent_files_follow_a_folder_rename_for_descendants_only() {
        let mut recent = RecentFiles::default();
        recent.remember(Path::new("/notes/dev/nested/b.md"));
        recent.remember(Path::new("/notes/dev/a.md"));
        recent.remember(Path::new("/notes/other.md"));

        recent.rename_folder(Path::new("/notes/dev"), Path::new("/notes/work"));

        assert_eq!(
            recent.entries(),
            [
                PathBuf::from("/notes/other.md"),
                PathBuf::from("/notes/work/a.md"),
                PathBuf::from("/notes/work/nested/b.md"),
            ]
        );
    }

    #[test]
    fn recent_path_escaping_round_trips() {
        let path = "folder\\name\nnotes.md";
        assert_eq!(unescape_line(&escape_line(path)), path);
    }

    #[test]
    fn missing_settings_file_uses_the_environment_default() {
        let settings = settings_from_read(Err(io::Error::from(io::ErrorKind::NotFound)));
        assert_eq!(settings.network, NetworkConfig::default());
        assert!(settings.network.valid().is_some());
    }

    #[test]
    fn unreadable_settings_file_is_an_invalid_network_config() {
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidData,
            io::ErrorKind::Other,
        ] {
            let settings = settings_from_read(Err(io::Error::from(kind)));
            assert!(settings.network.valid().is_none(), "{kind:?}");
            assert!(settings.network.invalid_reason().is_some(), "{kind:?}");
            assert_eq!(settings.network.effective_settings().schema, 0);
        }
    }

    #[test]
    fn invalid_utf8_settings_file_is_kept_when_storing_other_settings() {
        let root = temporary_directory("state-unreadable");
        fs::create_dir_all(&root).unwrap();
        let original = [0xff, 0xfe, b'\n'];
        fs::write(root.join("settings.conf"), original).unwrap();
        let store = FileStateStore::at(&root);
        let mut settings = SettingsRepository::load(&store);
        let reason = settings.network.invalid_reason().unwrap().to_owned();

        settings.autosave = false;
        settings.theme = ThemePreference::Dark;
        let error = SettingsRepository::store(&store, &settings).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), reason);

        assert_eq!(fs::read(root.join("settings.conf")).unwrap(), original);
        let reloaded = SettingsRepository::load(&FileStateStore::at(&root));
        assert_eq!(reloaded.network.invalid_reason(), Some(reason.as_str()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_failure_state_rejects_store_without_touching_the_file() {
        let root = temporary_directory("state-read-failure");
        fs::create_dir_all(&root).unwrap();
        let original = b"autosave=false\ntheme=dark\nnetwork_mode=manual\n";
        fs::write(root.join("settings.conf"), original).unwrap();
        let store = FileStateStore::at(&root);
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Other] {
            let settings = settings_from_read(Err(io::Error::from(kind)));
            let error = SettingsRepository::store(&store, &settings).unwrap_err();
            assert_eq!(error.kind(), kind);
            assert_eq!(fs::read(root.join("settings.conf")).unwrap(), original);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_valid_network_replaces_an_unreadable_settings_file() {
        let root = temporary_directory("state-unreadable-replace");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("settings.conf"), [0xff, 0xfe, b'\n']).unwrap();
        let store = FileStateStore::at(&root);
        let mut settings = SettingsRepository::load(&store);
        assert!(settings.network.valid().is_none());

        settings.network = NetworkConfig::Valid(NetworkSettings::direct());
        SettingsRepository::store(&store, &settings).unwrap();

        let reloaded = SettingsRepository::load(&FileStateStore::at(&root));
        assert_eq!(reloaded.network.valid().unwrap().mode, ProxyMode::Direct);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn settings_survive_a_store_reload() {
        let root = temporary_directory("state");
        let store = FileStateStore::at(&root);
        let settings = Settings {
            autosave: false,
            theme: ThemePreference::Dark,
            default_folder: Some(PathBuf::from("C:\\notes")),
            sidebar_sort: WorkFolderSortOrder::UpdatedOldest,
            network: NetworkConfig::default(),
        };
        SettingsRepository::store(&store, &settings).unwrap();
        assert_eq!(
            SettingsRepository::load(&FileStateStore::at(&root)),
            settings
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Every one of the four `WorkFolderSortOrder` values round-trips
    /// through a real store/reload cycle, not just the one value the
    /// adjacent `settings_survive_a_store_reload` test happens to pick.
    #[test]
    fn every_sidebar_sort_order_round_trips_through_a_store_reload() {
        for order in WorkFolderSortOrder::all() {
            let root = temporary_directory("state-sidebar-sort-round-trip");
            let store = FileStateStore::at(&root);
            let settings = Settings {
                sidebar_sort: order,
                ..Settings::default()
            };
            SettingsRepository::store(&store, &settings).unwrap();
            assert_eq!(
                SettingsRepository::load(&FileStateStore::at(&root)).sidebar_sort,
                order,
                "order {order:?} did not round-trip"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn default_folder_is_absent_until_set() {
        let root = temporary_directory("state");
        let store = FileStateStore::at(&root);
        SettingsRepository::store(&store, &Settings::default()).unwrap();
        assert_eq!(SettingsRepository::load(&store).default_folder, None);
        fs::remove_dir_all(root).unwrap();
    }

    // A settings file written before `sidebar_sort` existed has no such
    // line at all; it must load as the current default order rather than
    // fail to parse or silently switch the user's sidebar to some other
    // value.
    #[test]
    fn a_settings_file_predating_sidebar_sort_loads_the_default_order() {
        let root = temporary_directory("state");
        let store = FileStateStore::at(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("settings.conf"), "autosave=true\ntheme=dark\n").unwrap();
        assert_eq!(
            SettingsRepository::load(&store).sidebar_sort,
            WorkFolderSortOrder::default()
        );
        fs::remove_dir_all(root).unwrap();
    }

    // A value written by an older or newer version that this version does
    // not recognize must reset only `sidebar_sort` to the default, leaving
    // every other persisted setting intact.
    #[test]
    fn an_unrecognized_sidebar_sort_value_resets_only_that_setting() {
        let root = temporary_directory("state");
        let store = FileStateStore::at(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("settings.conf"),
            "autosave=false\ntheme=dark\nsidebar_sort=unknown_future_value\ndefault_folder=/notes\n",
        )
        .unwrap();
        let loaded = SettingsRepository::load(&store);
        assert_eq!(loaded.sidebar_sort, WorkFolderSortOrder::default());
        assert!(!loaded.autosave);
        assert_eq!(loaded.theme, ThemePreference::Dark);
        assert_eq!(loaded.default_folder, Some(PathBuf::from("/notes")));
        fs::remove_dir_all(root).unwrap();
    }

    fn manual_settings(auth: ProxyAuth) -> NetworkSettings {
        NetworkSettings::manual(
            ManualProxy::new("http://proxy.example:3128", "localhost, .corp.example", auth)
                .unwrap(),
        )
    }

    fn load_after_writing(contents: &str) -> Settings {
        let root = temporary_directory("state-network");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("settings.conf"), contents).unwrap();
        let loaded = SettingsRepository::load(&FileStateStore::at(&root));
        fs::remove_dir_all(root).unwrap();
        loaded
    }

    #[test]
    fn a_settings_file_without_network_keys_migrates_to_environment() {
        let loaded = load_after_writing("autosave=false\ntheme=dark\ndefault_folder=/notes\n");
        assert_eq!(loaded.network, NetworkConfig::default());
        assert_eq!(loaded.network.valid().unwrap().mode, ProxyMode::Environment);
        assert!(!loaded.autosave);
        assert_eq!(loaded.default_folder, Some(PathBuf::from("/notes")));
    }

    #[test]
    fn every_network_mode_round_trips_through_a_store_reload() {
        let reference = CredentialRef::new("proxy-1").unwrap();
        for network in [
            NetworkSettings::default(),
            NetworkSettings::direct(),
            manual_settings(ProxyAuth::None),
            manual_settings(ProxyAuth::Basic(reference)),
        ] {
            let root = temporary_directory("state-network-round-trip");
            let store = FileStateStore::at(&root);
            let settings = Settings {
                autosave: false,
                default_folder: Some(PathBuf::from("/notes")),
                network: NetworkConfig::Valid(network),
                ..Settings::default()
            };
            SettingsRepository::store(&store, &settings).unwrap();
            assert_eq!(SettingsRepository::load(&store), settings);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn the_credential_reference_is_stored_but_no_secret_value_is_written() {
        let root = temporary_directory("state-network-secret");
        let store = FileStateStore::at(&root);
        let reference = CredentialRef::new("proxy-ref-1").unwrap();
        let settings = Settings {
            network: NetworkConfig::Valid(manual_settings(ProxyAuth::Basic(reference))),
            ..Settings::default()
        };
        SettingsRepository::store(&store, &settings).unwrap();
        let text = fs::read_to_string(root.join("settings.conf")).unwrap();
        assert!(text.contains("network_manual_credential_ref=proxy-ref-1"));
        assert!(!text.contains("password"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn broken_network_values_are_a_settings_error_not_direct() {
        for contents in [
            "network_schema=99\nnetwork_mode=direct\n",
            "network_schema=abc\n",
            "network_mode=socks\n",
            "network_manual_url=http://p:1\n",
            "network_mode=manual\n",
            "network_mode=manual\nnetwork_manual_url=https://p:1\n",
            "network_mode=manual\nnetwork_manual_url=http://u:p@proxy:1\n",
            "network_mode=manual\nnetwork_manual_url=http://p:1\nnetwork_manual_bypass=*.a.com\n",
            "network_mode=manual\nnetwork_manual_url=http://p:1\nnetwork_manual_auth=basic\n",
            "network_mode=manual\nnetwork_manual_url=http://p:1\nnetwork_manual_auth=digest\n",
            "network_mode=manual\nnetwork_manual_url=http://p:1\nnetwork_manual_auth=basic\nnetwork_manual_credential_ref=a b\n",
        ] {
            let loaded = load_after_writing(contents);
            assert!(loaded.network.valid().is_none(), "{contents}");
            assert!(loaded.network.invalid_reason().is_some(), "{contents}");
            // Connections must fail explicitly instead of using Direct.
            assert_ne!(loaded.network.effective_settings().schema, SETTINGS_SCHEMA);
        }
    }

    #[test]
    fn the_settings_error_does_not_echo_stored_values() {
        let loaded = load_after_writing(
            "network_mode=manual\nnetwork_manual_url=http://user:topsecret@proxy:1\n",
        );
        let reason = loaded.network.invalid_reason().unwrap();
        assert!(!reason.contains("topsecret"));
        assert!(!format!("{:?}", loaded.network).contains("topsecret"));
    }

    #[test]
    fn storing_other_settings_keeps_an_unreadable_network_section() {
        let root = temporary_directory("state-network-keep");
        fs::create_dir_all(&root).unwrap();
        let original = "network_schema=99\nnetwork_mode=future\n";
        fs::write(root.join("settings.conf"), format!("autosave=true\n{original}")).unwrap();
        let store = FileStateStore::at(&root);
        let mut settings = SettingsRepository::load(&store);
        settings.autosave = false;
        SettingsRepository::store(&store, &settings).unwrap();
        let text = fs::read_to_string(root.join("settings.conf")).unwrap();
        assert!(text.contains(original));
        let reloaded = SettingsRepository::load(&store);
        assert!(!reloaded.autosave);
        assert!(reloaded.network.invalid_reason().is_some());
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
                    default_folder: None,
                    sidebar_sort: WorkFolderSortOrder::NameAscending,
                    network: NetworkConfig::default(),
                })
                .unwrap();
        }
        assert!(!stores.settings().load().autosave);
    }
}

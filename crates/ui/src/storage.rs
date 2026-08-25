use hane_document::RopeBuffer;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const RECENT_LIMIT: usize = 10;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::System => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::System,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Settings {
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PersistentState {
    pub settings: Settings,
    pub recent_files: Vec<PathBuf>,
}

impl PersistentState {
    pub(crate) fn load_default() -> Self {
        state_directory().map_or_else(|_| Self::default(), |root| Self::load(&root))
    }

    fn load(root: &Path) -> Self {
        let mut state = Self::default();
        if let Ok(contents) = fs::read_to_string(root.join("settings.conf")) {
            for line in contents.lines() {
                if let Some(value) = line.strip_prefix("autosave=") {
                    state.settings.autosave = value != "false";
                } else if let Some(value) = line.strip_prefix("theme=") {
                    state.settings.theme = ThemePreference::parse(value);
                }
            }
        }
        if let Ok(contents) = fs::read_to_string(root.join("recent-files")) {
            state.recent_files = contents
                .lines()
                .map(unescape_line)
                .map(PathBuf::from)
                .filter(|path| path.is_file())
                .take(RECENT_LIMIT)
                .collect();
        }
        state
    }

    pub(crate) fn remember(&mut self, path: &Path) {
        self.recent_files.retain(|recent| recent != path);
        self.recent_files.insert(0, path.to_path_buf());
        self.recent_files.truncate(RECENT_LIMIT);
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        let root = state_directory()?;
        fs::create_dir_all(&root)?;
        atomic_write_bytes(
            &root.join("settings.conf"),
            format!(
                "autosave={}\ntheme={}\n",
                self.settings.autosave,
                self.settings.theme.as_str()
            )
            .as_bytes(),
        )?;
        let recent = self
            .recent_files
            .iter()
            .map(|path| escape_line(&path.to_string_lossy()))
            .collect::<Vec<_>>()
            .join("\n");
        atomic_write_bytes(&root.join("recent-files"), recent.as_bytes())
    }
}

pub(crate) fn atomic_save_document(path: &Path, document: &RopeBuffer) -> io::Result<()> {
    atomic_write(path, |writer| document.write_to(writer))
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write(path, |writer| writer.write_all(bytes))
}

fn atomic_write(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<fs::File>) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{stem}.hane-{}-{sequence}.tmp",
        std::process::id()
    ));
    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let mut writer = BufWriter::new(file);
        write(&mut writer)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
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

    fn temporary_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hane-{name}-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn atomic_document_save_preserves_markdown_bytes() {
        let root = temporary_directory("save");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("document.md");
        let source = "# 羽\n\n![画像](assets/羽.png)\n\n| A | B |\n|---|---|\n";
        atomic_save_document(&path, &RopeBuffer::from_text(source)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recent_files_are_deduplicated_and_bounded() {
        let mut state = PersistentState::default();
        for index in 0..12 {
            state.remember(Path::new(&format!("/{index}.md")));
        }
        state.remember(Path::new("/5.md"));
        assert_eq!(state.recent_files.len(), RECENT_LIMIT);
        assert_eq!(state.recent_files[0], Path::new("/5.md"));
        assert_eq!(
            state
                .recent_files
                .iter()
                .filter(|path| *path == Path::new("/5.md"))
                .count(),
            1
        );
    }

    #[test]
    fn recent_path_escaping_round_trips() {
        let path = "folder\\name\nnotes.md";
        assert_eq!(unescape_line(&escape_line(path)), path);
    }
}

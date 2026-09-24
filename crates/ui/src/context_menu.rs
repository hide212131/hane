//! Windows Explorer integration owned by the UI's platform boundary.
//!
//! The existing folder verb and the settings-controlled file verb deliberately
//! use separate registry keys. The UI can therefore manage only the file verb
//! without removing or changing the folder integration.

use std::io;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileContextMenuState {
    NotChecked,
    Unsupported,
    Unregistered,
    Registered,
    Stale,
    Conflict,
    Unknown(String),
}

impl FileContextMenuState {
    pub fn is_checked(&self) -> bool {
        matches!(self, Self::Registered)
    }
}

pub fn current_file_context_menu_state() -> FileContextMenuState {
    #[cfg(windows)]
    {
        let Ok(exe) = std::env::current_exe() else {
            return FileContextMenuState::Unknown("現在のHane.exeを確認できません".to_owned());
        };
        inspect(&exe)
    }
    #[cfg(not(windows))]
    {
        FileContextMenuState::Unsupported
    }
}

pub fn set_file_context_menu(enabled: bool) -> io::Result<()> {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe()?;
        if enabled {
            register_file(&exe)
        } else {
            unregister_file()
        }
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "ファイル右クリック登録はWindowsでのみ利用できます",
        ))
    }
}

#[cfg(windows)]
mod windows {
    use super::{FileContextMenuState, Path};
    use std::fs::{self, OpenOptions};
    use std::io::{self, Write};
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    pub(super) const FOLDER_MENU_KEY: &str = r"Software\Classes\Directory\shell\Hane";
    const FILE_MENU_KEY: &str = r"Software\Classes\*\shell\Hane.OpenFile";
    const MENU_LABEL: &str = "Haneで開く";
    const OWNER_ID: &str = "92b8fc05-dc63-44ed-9afc-fb10a36bee8a";
    const PACKAGE_NAME: &str = "Hane.ShellIntegration";
    const PACKAGE_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), ".0");
    const FILE_ENABLED_MARKER: &str = "Hane.ShellIntegration.enabled";
    const FOLDER_ENABLED_MARKER: &str = "Hane.ShellIntegration.folder.enabled";
    const ENABLED_MARKER_CONTENTS: &str = concat!("92b8fc05-dc63-44ed-9afc-fb10a36bee8a", "\n");
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn register(exe: &Path) -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        match hkcu.open_subkey(FOLDER_MENU_KEY) {
            Ok(existing) if !is_owned(&existing) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "フォルダー用Hane登録は別の内容を持っているため変更しません",
                ));
            }
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        if is_windows_11() {
            check_marker_conflict(exe, FOLDER_ENABLED_MARKER)?;
        }
        ensure_explorer_package(exe)?;
        let (shell_key, _) = hkcu.create_subkey(FOLDER_MENU_KEY)?;
        shell_key.set_value("MUIVerb", &MENU_LABEL)?;
        shell_key.set_value("Icon", &format!("\"{}\"", exe.display()))?;
        shell_key.set_value("HaneOwner", &OWNER_ID)?;
        shell_key.set_value("HaneExePath", &exe.to_string_lossy().as_ref())?;
        let (command_key, _) = shell_key.create_subkey("command")?;
        command_key.set_value("", &command_line(exe))?;
        if is_windows_11() {
            write_marker(exe, FOLDER_ENABLED_MARKER)?;
        }
        notify_shell();
        Ok(())
    }

    pub fn unregister() -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        match hkcu.open_subkey(FOLDER_MENU_KEY) {
            Ok(existing) if is_owned(&existing) => {
                if is_windows_11() {
                    remove_marker(&std::env::current_exe()?, FOLDER_ENABLED_MARKER)?;
                }
                hkcu.delete_subkey_all(FOLDER_MENU_KEY)?;
                notify_shell();
                Ok(())
            }
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "フォルダー用Hane登録は別の内容を持っているため削除しません",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if is_windows_11() {
                    remove_marker(&std::env::current_exe()?, FOLDER_ENABLED_MARKER)?;
                }
                notify_shell();
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub fn inspect(exe: &Path) -> FileContextMenuState {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if is_windows_11() {
            if let Ok(key) = hkcu.open_subkey(FILE_MENU_KEY) {
                if !is_owned(&key) {
                    return FileContextMenuState::Conflict;
                }
            }
            return match marker_enabled(exe, FILE_ENABLED_MARKER) {
                Ok(true) if package_installed() => FileContextMenuState::Registered,
                Ok(true) => FileContextMenuState::Stale,
                Ok(false) => FileContextMenuState::Unregistered,
                Err(error) => FileContextMenuState::Unknown(error.to_string()),
            };
        }
        inspect_at(&hkcu, exe)
    }

    fn inspect_at(root: &RegKey, exe: &Path) -> FileContextMenuState {
        let key = match root.open_subkey(FILE_MENU_KEY) {
            Ok(key) => key,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return FileContextMenuState::Unregistered;
            }
            Err(error) => return FileContextMenuState::Unknown(error.to_string()),
        };
        let label = match key.get_value::<String, _>("MUIVerb") {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return FileContextMenuState::Unknown(error.to_string()),
        };
        if !is_owned(&key) {
            return FileContextMenuState::Conflict;
        }
        let icon = key.get_value::<String, _>("Icon").unwrap_or_default();
        let multi_select = key
            .get_value::<String, _>("MultiSelectModel")
            .unwrap_or_default();
        let command_key = match key.open_subkey("command") {
            Ok(key) => key,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return FileContextMenuState::Stale;
            }
            Err(error) => return FileContextMenuState::Unknown(error.to_string()),
        };
        let command = match command_key.get_value::<String, _>("") {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return FileContextMenuState::Stale;
            }
            Err(error) => return FileContextMenuState::Unknown(error.to_string()),
        };
        let expected_icon = format!("\"{}\"", exe.display());
        let expected_command = command_line(exe);
        if label == MENU_LABEL
            && icon.eq_ignore_ascii_case(&expected_icon)
            && multi_select == "Single"
            && command.eq_ignore_ascii_case(&expected_command)
            && (!is_windows_11()
                || (key.get_value::<String, _>("HaneOwner").ok().as_deref() == Some(OWNER_ID)
                    && key
                        .get_value::<String, _>("HaneExePath")
                        .is_ok_and(|path| path.eq_ignore_ascii_case(&exe.to_string_lossy()))
                    && marker_enabled(exe, FILE_ENABLED_MARKER).unwrap_or(false)
                    && package_installed()))
        {
            FileContextMenuState::Registered
        } else if label == MENU_LABEL {
            FileContextMenuState::Stale
        } else {
            FileContextMenuState::Conflict
        }
    }

    fn is_owned(key: &RegKey) -> bool {
        if key.get_value::<String, _>("MUIVerb").ok().as_deref() != Some(MENU_LABEL) {
            return false;
        }
        match key.get_value::<String, _>("HaneOwner") {
            Ok(owner) => owner == OWNER_ID,
            Err(_) => key
                .open_subkey("command")
                .and_then(|command| command.get_value::<String, _>(""))
                .map(|command| {
                    let lower = command.to_ascii_lowercase();
                    lower.starts_with('"')
                        && lower.ends_with("\" \"%1\"")
                        && lower.contains("\\hane.exe\"")
                })
                .unwrap_or(false),
        }
    }

    pub fn register_file(exe: &Path) -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        match hkcu.open_subkey(FILE_MENU_KEY) {
            Ok(existing) if !is_owned(&existing) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "Hane.OpenFile は別の登録内容を持っているため変更しません",
                ));
            }
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        if is_windows_11() {
            check_marker_conflict(exe, FILE_ENABLED_MARKER)?;
            ensure_explorer_package(exe)?;
            // Windows 11's packaged IExplorerCommand is the actual menu
            // registration. HKCU\Software\Classes is virtualized in the
            // packaged editor, so a legacy key cannot be its source of truth.
            remove_owned_legacy_file_key(&hkcu)?;
            write_marker(exe, FILE_ENABLED_MARKER)?;
            notify_shell();
            return Ok(());
        }
        ensure_explorer_package(exe)?;
        let (shell_key, _) = hkcu.create_subkey(FILE_MENU_KEY)?;
        shell_key.set_value("MUIVerb", &MENU_LABEL)?;
        shell_key.set_value("Icon", &format!("\"{}\"", exe.display()))?;
        shell_key.set_value("MultiSelectModel", &"Single")?;
        shell_key.set_value("HaneOwner", &OWNER_ID)?;
        shell_key.set_value("HaneExePath", &exe.to_string_lossy().as_ref())?;
        let (command_key, _) = shell_key.create_subkey("command")?;
        command_key.set_value("", &command_line(exe))?;
        if is_windows_11() {
            write_marker(exe, FILE_ENABLED_MARKER)?;
        }
        notify_shell();
        Ok(())
    }

    pub fn unregister_file() -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if is_windows_11() {
            remove_marker(&std::env::current_exe()?, FILE_ENABLED_MARKER)?;
            remove_owned_legacy_file_key(&hkcu)?;
            notify_shell();
            return Ok(());
        }
        match hkcu.open_subkey(FILE_MENU_KEY) {
            Ok(existing) if is_owned(&existing) => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "Hane.OpenFile は別の登録内容を持っているため削除しません",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if is_windows_11() {
            remove_marker(&std::env::current_exe()?, FILE_ENABLED_MARKER)?;
        }
        match hkcu.open_subkey(FILE_MENU_KEY) {
            Ok(_) => hkcu.delete_subkey_all(FILE_MENU_KEY)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        notify_shell();
        Ok(())
    }

    fn remove_owned_legacy_file_key(hkcu: &RegKey) -> io::Result<()> {
        match hkcu.open_subkey(FILE_MENU_KEY) {
            Ok(key) if is_owned(&key) => hkcu.delete_subkey_all(FILE_MENU_KEY),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn marker_path(exe: &Path, name: &str) -> io::Result<PathBuf> {
        Ok(exe
            .parent()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "Hane.exe の場所が不明です")
            })?
            .join(name))
    }

    fn marker_enabled(exe: &Path, name: &str) -> io::Result<bool> {
        match fs::read(marker_path(exe, name)?) {
            Ok(contents) => Ok(contents == ENABLED_MARKER_CONTENTS.as_bytes()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn check_marker_conflict(exe: &Path, name: &str) -> io::Result<()> {
        let path = marker_path(exe, name)?;
        match fs::read(&path) {
            Ok(contents) if contents == ENABLED_MARKER_CONTENTS.as_bytes() => Ok(()),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "別の内容の Explorer 拡張マーカーが存在します: {}",
                    path.display()
                ),
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn write_marker(exe: &Path, name: &str) -> io::Result<()> {
        check_marker_conflict(exe, name)?;
        let path = marker_path(exe, name)?;
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => file.write_all(ENABLED_MARKER_CONTENTS.as_bytes()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                check_marker_conflict(exe, name)
            }
            Err(error) => Err(error),
        }
    }

    fn remove_marker(exe: &Path, name: &str) -> io::Result<()> {
        check_marker_conflict(exe, name)?;
        match fs::remove_file(marker_path(exe, name)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn command_line(exe: &Path) -> String {
        format!("\"{}\" \"%1\"", exe.display())
    }

    fn is_windows_11() -> bool {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
            .and_then(|key| key.get_value::<String, _>("CurrentBuildNumber"))
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .is_none_or(|build| build >= 22000)
    }

    fn quote_ps(value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }

    fn powershell(script: &str) -> io::Result<std::process::Output> {
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    }

    fn package_installed() -> bool {
        let script = format!(
            "$p = Get-AppxPackage -Name {} -ErrorAction SilentlyContinue | Where-Object {{ $_.Version.ToString() -eq {} }}; if ($p) {{ exit 0 }} else {{ exit 3 }}",
            quote_ps(PACKAGE_NAME),
            quote_ps(PACKAGE_VERSION),
        );
        powershell(&script).is_ok_and(|output| output.status.success())
    }

    fn ensure_explorer_package(exe: &Path) -> io::Result<()> {
        if !is_windows_11() || package_installed() {
            return Ok(());
        }
        let directory = exe.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Hane.exe のディレクトリを確認できません",
            )
        })?;
        let package = directory.join("Hane.ShellIntegration.msix");
        if !package.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "Windows 11 用の署名済み Explorer 拡張が見つかりません: {}",
                    package.display()
                ),
            ));
        }
        let script = format!(
            "Add-AppxPackage -Path {} -ExternalLocation {} -ForceUpdateFromAnyVersion -ErrorAction Stop",
            quote_ps(&package.to_string_lossy()),
            quote_ps(&directory.to_string_lossy()),
        );
        let output = powershell(&script)?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(io::Error::other(format!(
                "Explorer 拡張パッケージの登録に失敗しました: {message}"
            )));
        }
        if !package_installed() {
            return Err(io::Error::other(
                "Explorer 拡張パッケージの登録を確認できません",
            ));
        }
        Ok(())
    }

    fn notify_shell() {
        // The notification is best-effort: the registry write itself remains
        // the source of truth, and Explorer will observe it on its next menu
        // open even if the notification API is unavailable.
        unsafe {
            windows_sys::Win32::UI::Shell::SHChangeNotify(
                windows_sys::Win32::UI::Shell::SHCNE_ASSOCCHANGED as i32,
                windows_sys::Win32::UI::Shell::SHCNF_IDLIST
                    | windows_sys::Win32::UI::Shell::SHCNF_FLUSH,
                std::ptr::null(),
                std::ptr::null(),
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn command_line_quotes_the_exe_and_forwards_the_target_file() {
            let exe = Path::new(r"C:\Program Files\Hane\hane.exe");
            assert_eq!(
                command_line(exe),
                r#""C:\Program Files\Hane\hane.exe" "%1""#
            );
        }

        #[test]
        fn file_and_folder_keys_are_separate() {
            assert_ne!(FILE_MENU_KEY, FOLDER_MENU_KEY);
            assert!(FILE_MENU_KEY.contains(r"\*\shell\Hane.OpenFile"));
            assert!(FOLDER_MENU_KEY.contains(r"\Directory\shell\Hane"));
        }
    }
}

#[cfg(windows)]
pub use windows::{inspect, register, register_file, unregister, unregister_file};

#[cfg(not(windows))]
pub fn register(_exe: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "Windows only"))
}

#[cfg(not(windows))]
pub fn unregister() -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "Windows only"))
}

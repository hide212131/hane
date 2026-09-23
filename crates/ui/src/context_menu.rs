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
    use std::io;
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    pub(super) const FOLDER_MENU_KEY: &str = r"Software\Classes\Directory\shell\Hane";
    const FILE_MENU_KEY: &str = r"Software\Classes\*\shell\Hane.OpenFile";
    const MENU_LABEL: &str = "Haneで開く";

    pub fn register(exe: &Path) -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (shell_key, _) = hkcu.create_subkey(FOLDER_MENU_KEY)?;
        shell_key.set_value("MUIVerb", &MENU_LABEL)?;
        shell_key.set_value("Icon", &format!("\"{}\"", exe.display()))?;
        let (command_key, _) = shell_key.create_subkey("command")?;
        command_key.set_value("", &command_line(exe))?;
        Ok(())
    }

    pub fn unregister() -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        match hkcu.delete_subkey_all(FOLDER_MENU_KEY) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn inspect(exe: &Path) -> FileContextMenuState {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
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
        if !label.is_empty() && label != MENU_LABEL {
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
            && icon == expected_icon
            && multi_select == "Single"
            && command == expected_command
        {
            FileContextMenuState::Registered
        } else if label == MENU_LABEL {
            FileContextMenuState::Stale
        } else {
            FileContextMenuState::Conflict
        }
    }

    fn is_owned(key: &RegKey) -> bool {
        key.get_value::<String, _>("MUIVerb")
            .map(|value| value == MENU_LABEL)
            .unwrap_or(false)
    }

    pub fn register_file(exe: &Path) -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(existing) = hkcu.open_subkey(FILE_MENU_KEY)
            && !is_owned(&existing)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Hane.OpenFile は別の登録内容を持っているため変更しません",
            ));
        }
        let (shell_key, _) = hkcu.create_subkey(FILE_MENU_KEY)?;
        shell_key.set_value("MUIVerb", &MENU_LABEL)?;
        shell_key.set_value("Icon", &format!("\"{}\"", exe.display()))?;
        shell_key.set_value("MultiSelectModel", &"Single")?;
        let (command_key, _) = shell_key.create_subkey("command")?;
        command_key.set_value("", &command_line(exe))?;
        notify_shell();
        Ok(())
    }

    pub fn unregister_file() -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        match hkcu.open_subkey(FILE_MENU_KEY) {
            Ok(existing) if is_owned(&existing) => {
                hkcu.delete_subkey_all(FILE_MENU_KEY)?;
                notify_shell();
                Ok(())
            }
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Hane.OpenFile は別の登録内容を持っているため削除しません",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn command_line(exe: &Path) -> String {
        format!("\"{}\" \"%1\"", exe.display())
    }

    fn notify_shell() {
        // The notification is best-effort: the registry write itself remains
        // the source of truth, and Explorer will observe it on its next menu
        // open even if the notification API is unavailable.
        unsafe {
            windows_sys::Win32::UI::Shell::SHChangeNotify(
                windows_sys::Win32::UI::Shell::SHCNE_ASSOCCHANGED as i32,
                windows_sys::Win32::UI::Shell::SHCNF_IDLIST,
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

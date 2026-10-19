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

/// The Explorer integration that is enabled for the current user: the owner and
/// package identity, and which verbs (file / folder) are ON. Read before an
/// update and carried to the new package version afterwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplorerRegistration {
    pub owner: String,
    pub package_name: String,
    pub publisher: String,
    pub file_enabled: bool,
    pub folder_enabled: bool,
}

/// Builds the registration to carry from what the trusted inspection read.
/// `Ok(None)` only when neither verb is ON (nothing is turned on that was off).
/// When a verb is ON but the package publisher is missing or empty, the
/// registration cannot be inspected and that is an error: it is never read as
/// "not registered".
pub fn registration_from_inspection(
    owner: &str,
    package_name: &str,
    publisher: Option<String>,
    file_enabled: bool,
    folder_enabled: bool,
) -> io::Result<Option<ExplorerRegistration>> {
    if !file_enabled && !folder_enabled {
        return Ok(None);
    }
    let publisher = publisher
        .map(|publisher| publisher.trim().to_owned())
        .filter(|publisher| !publisher.is_empty())
        .ok_or_else(|| {
            io::Error::other(
                "Explorer 連携が有効ですが、Explorer 拡張パッケージの所有者を確認できません",
            )
        })?;
    Ok(Some(ExplorerRegistration {
        owner: owner.to_owned(),
        package_name: package_name.to_owned(),
        publisher,
        file_enabled,
        folder_enabled,
    }))
}

/// Reads the enabled Explorer integration of the current user. `Ok(None)` when
/// it is off, was never registered, or this is not Windows 11.
pub fn capture_explorer_registration() -> io::Result<Option<ExplorerRegistration>> {
    #[cfg(windows)]
    {
        windows::capture_registration()
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

/// Carries `saved` to the newly installed version, after the update committed.
/// Only the verbs that were ON are carried.
pub fn migrate_explorer_registration(saved: &ExplorerRegistration) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::migrate_registration(saved)
    }
    #[cfg(not(windows))]
    {
        let _ = saved;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Explorer 拡張の登録はWindowsでのみ利用できます",
        ))
    }
}

/// The migration with its environment as seams. The ownership and package
/// identity are re-confirmed first; only then is the package registered. A
/// failed confirmation or registration is an error: success is never assumed,
/// and nothing is written for a verb that was OFF.
pub fn migrate_registration_with(
    saved: &ExplorerRegistration,
    expected_owner: &str,
    expected_package: &str,
    installed_publisher: impl FnOnce() -> io::Result<Option<String>>,
    marker_owned: impl Fn(ExplorerVerb) -> io::Result<bool>,
    register_package: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if !saved.file_enabled && !saved.folder_enabled {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "引き継ぐ Explorer 登録に有効な項目がありません",
        ));
    }
    if saved.owner != expected_owner || saved.package_name != expected_package {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "更新前の Explorer 登録の所有者が Hane のものと一致しません",
        ));
    }
    match installed_publisher()? {
        Some(publisher) if publisher == saved.publisher => {}
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Explorer 拡張パッケージの所有者が更新前と一致しないため登録を引き継ぎません",
            ));
        }
    }
    for (enabled, verb) in [
        (saved.file_enabled, ExplorerVerb::File),
        (saved.folder_enabled, ExplorerVerb::Folder),
    ] {
        if enabled && !marker_owned(verb)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "更新前に有効だった Explorer 登録のマーカーが変更されているため引き継ぎません",
            ));
        }
    }
    register_package()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplorerVerb {
    File,
    Folder,
}

#[cfg(windows)]
mod windows {
    use super::{ExplorerRegistration, ExplorerVerb, FileContextMenuState, Path};
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
            if let Ok(key) = hkcu.open_subkey(FILE_MENU_KEY)
                && !is_owned(&key)
            {
                return FileContextMenuState::Conflict;
            }
            return match marker_enabled(exe, FILE_ENABLED_MARKER) {
                Ok(true) if package_installed() && shell_extension_present(exe) => {
                    FileContextMenuState::Registered
                }
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
                    && package_installed()
                    && shell_extension_present(exe)))
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

    /// Versioned DLL location relative to the install root:
    /// `shell-integration/<cargo version>/hane_shell_extension.dll`.
    fn shell_extension_dll_path(root: &Path, version: &str) -> PathBuf {
        root.join("shell-integration")
            .join(version)
            .join("hane_shell_extension.dll")
    }

    fn shell_extension_present(exe: &Path) -> bool {
        exe.parent().is_some_and(|root| {
            shell_extension_dll_path(root, env!("CARGO_PKG_VERSION")).is_file()
        })
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

    /// Publisher of the installed package of this name for the current user,
    /// whatever its version. `None` when it is not installed.
    fn installed_package_publisher() -> io::Result<Option<String>> {
        let script = format!(
            "$p = Get-AppxPackage -Name {} -ErrorAction SilentlyContinue | Select-Object -First 1; if ($p) {{ [Console]::Out.Write($p.Publisher); exit 0 }} else {{ exit 3 }}",
            quote_ps(PACKAGE_NAME),
        );
        let output = powershell(&script)?;
        match output.status.code() {
            Some(0) => {
                let publisher = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                Ok((!publisher.is_empty()).then_some(publisher))
            }
            Some(3) => Ok(None),
            _ => Err(io::Error::other(
                "Explorer 拡張パッケージの所有者を確認できません",
            )),
        }
    }

    pub(super) fn capture_registration() -> io::Result<Option<ExplorerRegistration>> {
        if !is_windows_11() {
            return Ok(None);
        }
        let exe = std::env::current_exe()?;
        let file_enabled = marker_enabled(&exe, FILE_ENABLED_MARKER)?;
        let folder_enabled = marker_enabled(&exe, FOLDER_ENABLED_MARKER)?;
        if !file_enabled && !folder_enabled {
            return Ok(None);
        }
        super::registration_from_inspection(
            OWNER_ID,
            PACKAGE_NAME,
            installed_package_publisher()?,
            file_enabled,
            folder_enabled,
        )
    }

    pub(super) fn migrate_registration(saved: &ExplorerRegistration) -> io::Result<()> {
        if !is_windows_11() {
            return Ok(());
        }
        let exe = std::env::current_exe()?;
        super::migrate_registration_with(
            saved,
            OWNER_ID,
            PACKAGE_NAME,
            installed_package_publisher,
            |verb| {
                let marker = match verb {
                    ExplorerVerb::File => FILE_ENABLED_MARKER,
                    ExplorerVerb::Folder => FOLDER_ENABLED_MARKER,
                };
                marker_enabled(&exe, marker)
            },
            || {
                ensure_explorer_package(&exe)?;
                notify_shell();
                Ok(())
            },
        )
    }

    #[derive(Debug, PartialEq, Eq)]
    enum PackagePlan {
        /// Windows 10: no MSIX package is involved.
        NotApplicable,
        /// Current package registered and current-version DLL present.
        Satisfied,
        /// Current-version DLL is missing; never report success.
        DllMissing,
        /// DLL present but the current package is not registered.
        Register,
    }

    fn package_plan(is_windows_11: bool, installed: bool, dll_present: bool) -> PackagePlan {
        if !is_windows_11 {
            PackagePlan::NotApplicable
        } else if !dll_present {
            PackagePlan::DllMissing
        } else if installed {
            PackagePlan::Satisfied
        } else {
            PackagePlan::Register
        }
    }

    fn ensure_explorer_package(exe: &Path) -> io::Result<()> {
        let win11 = is_windows_11();
        let plan = package_plan(
            win11,
            win11 && package_installed(),
            shell_extension_present(exe),
        );
        let directory = exe.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Hane.exe のディレクトリを確認できません",
            )
        })?;
        match plan {
            PackagePlan::NotApplicable | PackagePlan::Satisfied => return Ok(()),
            PackagePlan::DllMissing => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "版別の Explorer 拡張 DLL が見つかりません: {}",
                        shell_extension_dll_path(directory, env!("CARGO_PKG_VERSION")).display()
                    ),
                ));
            }
            PackagePlan::Register => {}
        }
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
        fn shell_extension_dll_is_under_the_version_directory() {
            let root = Path::new(r"C:\Hane");
            assert_eq!(
                shell_extension_dll_path(root, "0.27.0"),
                Path::new(r"C:\Hane\shell-integration\0.27.0\hane_shell_extension.dll")
            );
            assert_ne!(
                shell_extension_dll_path(root, "0.26.0"),
                shell_extension_dll_path(root, "0.27.0")
            );
        }

        #[test]
        fn package_version_is_the_cargo_version_with_a_zero_revision() {
            assert_eq!(
                PACKAGE_VERSION,
                format!("{}.0", env!("CARGO_PKG_VERSION"))
            );
            assert_eq!(env!("CARGO_PKG_VERSION").split('.').count(), 3);
        }

        #[test]
        fn markers_stay_beside_the_exe_not_the_versioned_dll() {
            let exe = Path::new(r"C:\Hane\hane.exe");
            assert_eq!(
                marker_path(exe, FILE_ENABLED_MARKER).unwrap(),
                Path::new(r"C:\Hane\Hane.ShellIntegration.enabled")
            );
            assert_eq!(
                marker_path(exe, FOLDER_ENABLED_MARKER).unwrap(),
                Path::new(r"C:\Hane\Hane.ShellIntegration.folder.enabled")
            );
        }

        #[test]
        fn shell_extension_presence_requires_the_current_version_dll() {
            let dir = std::env::temp_dir().join(format!("hane-shell-dll-{}", std::process::id()));
            let exe = dir.join("hane.exe");
            let old = shell_extension_dll_path(&dir, "0.0.1");
            fs::create_dir_all(old.parent().unwrap()).unwrap();
            fs::write(&old, b"old").unwrap();
            assert!(!shell_extension_present(&exe));
            let current = shell_extension_dll_path(&dir, env!("CARGO_PKG_VERSION"));
            fs::create_dir_all(current.parent().unwrap()).unwrap();
            fs::write(&current, b"new").unwrap();
            assert!(shell_extension_present(&exe));
            assert!(old.is_file(), "old version DLL must coexist");
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn package_plan_distinguishes_registration_and_dll_states() {
            assert_eq!(package_plan(true, true, true), PackagePlan::Satisfied);
            assert_eq!(package_plan(true, true, false), PackagePlan::DllMissing);
            assert_eq!(package_plan(true, false, true), PackagePlan::Register);
            assert_eq!(package_plan(true, false, false), PackagePlan::DllMissing);
            assert_eq!(package_plan(false, false, false), PackagePlan::NotApplicable);
            assert_eq!(package_plan(false, false, true), PackagePlan::NotApplicable);
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

#[cfg(test)]
mod migration_tests {
    use super::*;
    use std::cell::Cell;

    const OWNER: &str = "owner-id";
    const PACKAGE: &str = "Hane.ShellIntegration";
    const PUBLISHER: &str = "CN=Hane";

    fn saved(file: bool, folder: bool) -> ExplorerRegistration {
        ExplorerRegistration {
            owner: OWNER.to_owned(),
            package_name: PACKAGE.to_owned(),
            publisher: PUBLISHER.to_owned(),
            file_enabled: file,
            folder_enabled: folder,
        }
    }

    fn migrate(
        saved: &ExplorerRegistration,
        publisher: Option<&str>,
        markers: (bool, bool),
        registered: &Cell<u32>,
        register_ok: bool,
    ) -> io::Result<()> {
        migrate_registration_with(
            saved,
            OWNER,
            PACKAGE,
            || Ok(publisher.map(str::to_owned)),
            |verb| {
                Ok(match verb {
                    ExplorerVerb::File => markers.0,
                    ExplorerVerb::Folder => markers.1,
                })
            },
            || {
                registered.set(registered.get() + 1);
                if register_ok {
                    Ok(())
                } else {
                    Err(io::Error::other("登録に失敗"))
                }
            },
        )
    }

    #[test]
    fn capture_keeps_each_file_and_folder_combination() {
        for (file, folder) in [(true, true), (true, false), (false, true)] {
            let captured = registration_from_inspection(
                OWNER,
                PACKAGE,
                Some(PUBLISHER.to_owned()),
                file,
                folder,
            )
            .unwrap()
            .expect("an enabled integration is captured");
            assert_eq!(captured, saved(file, folder));
        }
    }

    #[test]
    fn capture_is_none_for_a_user_without_the_integration() {
        // Both markers OFF is "not registered", whatever the publisher read.
        for publisher in [Some(PUBLISHER.to_owned()), Some(String::new()), None] {
            assert_eq!(
                registration_from_inspection(OWNER, PACKAGE, publisher, false, false).unwrap(),
                None
            );
        }
    }

    #[test]
    fn an_enabled_marker_without_a_publisher_is_an_error_not_unregistered() {
        for (file, folder) in [(true, true), (true, false), (false, true)] {
            for publisher in [None, Some(String::new()), Some("  ".to_owned())] {
                assert!(
                    registration_from_inspection(OWNER, PACKAGE, publisher, file, folder).is_err()
                );
            }
        }
    }

    #[test]
    fn every_enabled_combination_is_migrated_once() {
        for (file, folder) in [(true, true), (true, false), (false, true)] {
            let registered = Cell::new(0);
            migrate(&saved(file, folder), Some(PUBLISHER), (file, folder), &registered, true)
                .unwrap();
            assert_eq!(registered.get(), 1);
        }
    }

    #[test]
    fn a_verb_that_was_off_is_not_required_or_turned_on() {
        // The folder marker is absent and was OFF: the migration still succeeds
        // and never asks for it.
        let registered = Cell::new(0);
        migrate(&saved(true, false), Some(PUBLISHER), (true, false), &registered, true).unwrap();
        assert_eq!(registered.get(), 1);
    }

    #[test]
    fn a_changed_owner_or_publisher_is_not_migrated() {
        let registered = Cell::new(0);
        let mut other_owner = saved(true, true);
        other_owner.owner = "someone-else".to_owned();
        assert!(migrate(&other_owner, Some(PUBLISHER), (true, true), &registered, true).is_err());
        assert!(migrate(&saved(true, true), Some("CN=Other"), (true, true), &registered, true).is_err());
        assert!(migrate(&saved(true, true), None, (true, true), &registered, true).is_err());
        assert_eq!(registered.get(), 0, "no package is registered after a refusal");
    }

    #[test]
    fn a_marker_that_is_no_longer_owned_is_not_migrated() {
        let registered = Cell::new(0);
        assert!(migrate(&saved(true, true), Some(PUBLISHER), (true, false), &registered, true).is_err());
        assert!(migrate(&saved(true, false), Some(PUBLISHER), (false, true), &registered, true).is_err());
        assert_eq!(registered.get(), 0);
    }

    #[test]
    fn a_failed_registration_is_reported_not_swallowed() {
        let registered = Cell::new(0);
        let error = migrate(&saved(true, true), Some(PUBLISHER), (true, true), &registered, false)
            .unwrap_err();
        assert!(error.to_string().contains("登録に失敗"));
        assert_eq!(registered.get(), 1);
    }

    #[test]
    fn a_registration_with_nothing_enabled_is_refused() {
        let registered = Cell::new(0);
        assert!(migrate(&saved(false, false), Some(PUBLISHER), (false, false), &registered, true).is_err());
        assert_eq!(registered.get(), 0);
    }
}

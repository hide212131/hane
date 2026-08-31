//! Registers or removes the Windows Explorer folder context menu entry
//! ("Haneで開く") that launches Hane against the right-clicked folder.
//!
//! This writes only to `HKEY_CURRENT_USER`, so it needs no elevation and
//! affects only the current user's Explorer, matching the app's portable,
//! installer-less distribution.

use std::io;
use std::path::Path;
use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

const MENU_KEY: &str = r"Software\Classes\Directory\shell\Hane";
const MENU_LABEL: &str = "Haneで開く";

fn command_line(exe: &Path) -> String {
    format!("\"{}\" \"%1\"", exe.display())
}

pub fn register(exe: &Path) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (shell_key, _) = hkcu.create_subkey(MENU_KEY)?;
    shell_key.set_value("MUIVerb", &MENU_LABEL)?;
    shell_key.set_value("Icon", &format!("\"{}\"", exe.display()))?;
    let (command_key, _) = shell_key.create_subkey("command")?;
    command_key.set_value("", &command_line(exe))?;
    Ok(())
}

pub fn unregister() -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    match hkcu.delete_subkey_all(MENU_KEY) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quotes_the_exe_and_forwards_the_target_folder() {
        let exe = Path::new(r"C:\Program Files\Hane\hane.exe");
        assert_eq!(
            command_line(exe),
            r#""C:\Program Files\Hane\hane.exe" "%1""#
        );
    }
}

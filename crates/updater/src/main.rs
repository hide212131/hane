#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! `hane-updater`: the update helper. It accepts no arguments and no paths;
//! see `hane_update::helper` for the session it runs.

fn main() {
    std::process::exit(hane_update::helper::helper_main());
}

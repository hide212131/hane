use std::{
    env, fs,
    path::PathBuf,
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=../../assets/app-icon.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"),
    );
    let icon = manifest_dir.join("../../assets/app-icon.ico");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    let rc_path = out_dir.join("hane-icon.rc");
    let res_path = out_dir.join("hane-icon.res");

    let icon_for_rc = icon.to_string_lossy().replace('\\', "/");
    fs::write(&rc_path, format!("1 ICON \"{icon_for_rc}\"\r\n"))
        .expect("write Windows icon resource script");

    let rc = find_resource_compiler().unwrap_or_else(|| {
        panic!(
            "Windows Resource Compiler (rc.exe) was not found. Install the Windows SDK or Visual Studio C++ Build Tools."
        )
    });
    let status = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status()
        .unwrap_or_else(|error| panic!("run {}: {error}", rc.display()));
    assert!(status.success(), "rc.exe failed with status {status}");

    // GPUI's Windows backend loads icon resource ID 1 from the executable.
    // Passing the .res file to LINK makes the icon available to both Explorer
    // and GPUI's window class.
    println!(
        "cargo:rustc-link-arg-bin=hane={}",
        res_path.to_string_lossy()
    );
}

fn find_resource_compiler() -> Option<PathBuf> {
    find_on_path("rc.exe").or_else(find_windows_sdk_resource_compiler)
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn find_windows_sdk_resource_compiler() -> Option<PathBuf> {
    let mut roots = Vec::new();
    for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(root) = env::var_os(variable) {
            let root = PathBuf::from(root).join("Windows Kits/10/bin");
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
    }

    let host_arch = match env::var("PROCESSOR_ARCHITECTURE")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "amd64" | "x86_64" => "x64",
        "arm64" | "aarch64" => "arm64",
        "x86" | "i386" | "i686" => "x86",
        _ => match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
            Ok("aarch64") => "arm64",
            Ok("x86") => "x86",
            _ => "x64",
        },
    };

    for root in roots {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        let mut versions = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        versions.sort();
        for version in versions.into_iter().rev() {
            for arch in [host_arch, "x64", "arm64", "x86"] {
                let candidate = version.join(arch).join("rc.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

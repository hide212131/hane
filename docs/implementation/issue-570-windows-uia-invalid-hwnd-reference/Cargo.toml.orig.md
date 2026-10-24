# Verbatim upstream reference: Cargo.toml.orig

Source: AccessKit accesskit_windows 0.34.0, crate SHA-256 `106c2b961215864d1c2e703ee63269c25c4e80a577ffb2c1017b9c17dcdf83a1`. License files are from repository commit `c88605b96d04431f9c3c792464a0f2f253480e94`.

Original file bytes: 1110; SHA-256: `71d45e050aab4e968e634c93e161f7fd3609d8b783374ca07e52e3f4b619bd97`. This is reference data, not executable project source. Preserve the bytes inside the fence when materializing an unchanged upstream file. Final newline in the source: True.

````
[package]
name = "accesskit_windows"
version = "0.34.0"
authors.workspace = true
license.workspace = true
description = "AccessKit UI accessibility infrastructure: Windows adapter"
categories.workspace = true
keywords = ["gui", "ui", "accessibility"]
repository.workspace = true
readme = "README.md"
edition.workspace = true
rust-version.workspace = true

[package.metadata.docs.rs]
default-target = "x86_64-pc-windows-msvc"
targets = []

[dependencies]
accesskit = { version = "0.24.1", path = "../../common" }
accesskit_consumer = { version = "0.38.0", path = "../../consumer" }
hashbrown.workspace = true
static_assertions = "1.1.0"
windows-core = "0.62.0"

[dependencies.windows]
version = "0.62.0"
features = [
    "Win32_Foundation",
    "Win32_Globalization",
    "Win32_Graphics_Gdi",
    "Win32_System_Com",
    "Win32_System_LibraryLoader",
    "Win32_System_Ole",
    "Win32_System_Variant",
    "Win32_UI_Accessibility",
    "Win32_UI_Input_KeyboardAndMouse",
    "Win32_UI_WindowsAndMessaging",
]

[dev-dependencies]
once_cell = "1.13.0"
parking_lot = "0.12.4"
scopeguard = "1.1.0"
winit = "0.30"
````

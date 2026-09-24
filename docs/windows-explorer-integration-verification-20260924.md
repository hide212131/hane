# Windows Explorer context menu verification (2026-09-24)

Result: **PASS for the repaired working tree based on `6e29684`**. This is not a claim that the original, unmodified commit passed.

- Host: Windows 11 Home 25H2, ARM64, build 26200.9457.
- Executable: `C:\Users\hide2\source\Hane-Windows\hane\target\windows-arm64-1.93.1\release\hane.exe`.
- Package: locally signed sparse `Hane.ShellIntegration.msix` (`CN=Hane Local Test 2026`); installed for the current user.
- Before registration, neither `HKCU\Software\Classes\*\shell\Hane.OpenFile` nor the Hane folder key existed.
- Settings > General > Windows integration enabled the file command without error. Explorer's *actual* Windows 11 menu showed `Haneで開く`: [menu screenshot](windows-hane-context-menu-marker-only-present-20260924.jpg).
- After clicking the menu with Hane closed, it opened the selected `hane-context-menu-test-cold.md` containing `HANE-COLD-20260924-1845`: [cold-start screenshot](windows-hane-context-menu-marker-only-cold-20260924.jpg).
- After clicking the menu for a different file while Hane was running, the same Hane process/window opened `hane-context-menu-test-warm.md` containing `HANE-WARM-20260924-1845`: [running-instance screenshot](windows-hane-context-menu-marker-only-warm-20260924.jpg).
- Settings inside the Explorer-launched Hane showed the registration as enabled. Disabling it removed the command from both the modern and classic Explorer menus: [disabled modern menu](windows-hane-context-menu-marker-only-disabled-20260924.jpg), [disabled classic menu](windows-hane-context-menu-marker-only-disabled-classic-20260924.jpg).
- After restarting Hane, Settings still showed `未登録`: [restart screenshot](windows-hane-context-menu-marker-only-restart-off-20260924.jpg).
- The folder command was separately exercised through Explorer and then unregistered: [folder menu screenshot](windows-hane-context-menu-final-folder-present-20260924.jpg). At completion, the Hane file/folder registry keys and both enable markers were absent. Other applications' registrations were not changed.
- `cargo test -p hane-ui context_menu --release --locked` passed (4 tests), and the ARM64 release executable and signed package built successfully.

The signed package and its locally trusted self-signed certificate were left installed so the user can re-enable the integration later; the context-menu switches were left **off**. This report records the local verification build. A version-bump or other source change after this verification requires a fresh build and appropriate retest before claiming that exact later binary passed.

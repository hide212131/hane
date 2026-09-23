# RF0 初回記録の検証証拠（Issue #298 / PR #319）

## 対象と来歴

対象の製品・テストコードは `main` の `f82ecef0a64b9a728c97aa9048db4606f399d549`。2026年9月23日の初回調査で実行したコマンドのstdout/stderr原ログを、今回の補完で静的なファイルとして保存した。**この補完のためにテストを再実行していない。** 終了コードは各実行時のshellから観測した値で、元のリダイレクト先ログ自体には含まれない。ログが空の場合もそのまま残した。

環境: macOS 26.6.2 (25G83)、arm64、Rust/Cargo 1.93.1 (Homebrew)、Python 3.14.3。`all-features`と通常feature、並列と単一スレッド、vendorの隔離実行は別の条件である。以下はローカル実行の証拠で、[PRのCI run](https://github.com/hide212131/hane/actions/runs/35848315415)（当時のhead `2eeb94d92c85c99259c3becfab9902b79a34035d`、base `f82ecef0a64b9a728c97aa9048db4606f399d549`）とは区別する。このCI runではPython検査が成功し、文書変更のpath filterによりmacOS/WindowsのRust jobはskipされた。新しいheadのCI結果はPRのchecksで別途確認する。

## ログとコマンド

| 保存したログ | 実行したコマンド（対象は上記SHA） | 観測した終了コード・範囲 |
|---|---|---|
| [`cargo-test-all-features.log`](logs/cargo-test-all-features.log) | `cargo test --workspace --all-features` | **101**。`hane-ui --lib`中にSIGABRT。先行suiteの成功をworkspace全体の成功とはしない。 |
| [`hane-ui-parallel-repeat.log`](logs/hane-ui-parallel-repeat.log) | `cargo test -p hane-ui --lib --all-features` | **101**。別の並列実行でSIGABRTを再現。元の全workspace実行とは別結果。 |
| [`hane-ui-serial.log`](logs/hane-ui-serial.log) | `cargo test -p hane-ui --lib --all-features -- --test-threads=1` | **0**。`hane-ui --lib` 199件。 |
| [`cargo-test-all-features-serial.log`](logs/cargo-test-all-features-serial.log) | `cargo test --workspace --all-features -- --test-threads=1` | **0**。workspace全suiteとdoc tests、単一スレッドのみ。 |
| [`cargo-test-default-serial.log`](logs/cargo-test-default-serial.log) | `cargo test --workspace --locked -- --test-threads=1` | **0**。通常featureのworkspace全suiteとdoc tests、単一スレッドのみ。 |
| [`cargo-clippy-all-features.log`](logs/cargo-clippy-all-features.log) | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | **0**。macOSでのworkspace静的検査。 |
| [`test_rust_ci_path_filter.log`](logs/test_rust_ci_path_filter.log) | `python3 .github/tests/test_rust_ci_path_filter.py` | **0**。現行CIのpath filter検査。 |
| [`test_aadw_gui_command.log`](logs/test_aadw_gui_command.log) | `python3 .github/tests/test_aadw_gui_command.py` | **0**。GUI command検査。 |
| [`test_hosted_code_block_gui.log`](logs/test_hosted_code_block_gui.log) | `python3 scripts/tests/test_hosted_code_block_gui.py` | **0**。code block GUI手順のunit検査。実GUI結果ではない。 |
| [`test_claude_failure_diagnostic.log`](logs/test_claude_failure_diagnostic.log) | `python3 .github/tests/test_claude_failure_diagnostic.py` | **0**。失敗分類のunit検査。 |
| [`test_codex_usage_limit_fallback.log`](logs/test_codex_usage_limit_fallback.log) | `python3 .github/tests/test_codex_usage_limit_fallback.py` | **0**。この原ログは**0 bytes**で、成功出力はない。終了コードは当時のshell結果で確認した。[PR CIのpath-filter job](https://github.com/hide212131/hane/actions/runs/35848315415/job/107139720924)もPython入口を実行するが、そのrunの対象headは上記のとおり。 |
| [`test_release_version.log`](logs/test_release_version.log) | `python3 .github/tests/test_release_version.py` | **0**。version policyのunit検査で、release buildではない。 |
| [`vendor-gpui-text-system.log`](logs/vendor-gpui-text-system.log) | `cargo test --manifest-path vendor/gpui/Cargo.toml --features runtime_shaders --lib platform::mac::text_system::tests` | **0**。macOS GPUI対象7件。実アプリのGUI/IMEではない。 |
| [`vendor-gpui-ime-source.log`](logs/vendor-gpui-ime-source.log) | `cargo test --manifest-path vendor/gpui/Cargo.toml --features runtime_shaders --lib platform::mac::platform::tests::keyboard_selection_change_reactivates_text_context_once_and_ignores_other_responders -- --exact` | **0**。macOS GPUI対象1件。 |
| [`vendor-pulldown-errors.log`](logs/vendor-pulldown-errors.log) | `cargo test --manifest-path vendor/pulldown-cmark/Cargo.toml --test errors` | **101**。repo内ではCargoのworkspace membership判定でテスト起動前に失敗。製品テストの失敗とはしない。 |
| [`vendor-pulldown-isolated-errors.log`](logs/vendor-pulldown-isolated-errors.log) | 同じvendor内容をrepo外の一時ディレクトリへコピー後、`cargo test --manifest-path ../pulldown-check/Cargo.toml --test errors` | **0**。隔離コピー内の`errors` 25件。repo内直接実行の成功へ読み替えない。 |

並列SIGABRTの対応するmacOS `.ips` から、例外・HIToolboxのabort文・faulting threadの先頭16 stack frameだけを抽出した: [全workspace実行](crash/workspace-all-features.json)、[UI単体の並列再現](crash/ui-parallel-repeat.json)。両方とも`TSMCurrentKeyboardInputSourceRefCreate`→GPUIの`mac_active_input_source_is_ascii_capable`→`EditorView::from_sessions`を含む。`source_report_sha256`は各元報告のSHA-256で、抽出後のファイルのhashではない。テスト並列性と実アプリ挙動の切り分けは未完。

## 除外した情報と範囲

- ログ中の元checkout絶対パスを`<repo>`、隔離vendorコピーの絶対パスを`<isolated-vendor-copy>`に置換した。これはホームディレクトリ名などの個人情報を除くための**唯一のログ本文置換**で、テスト結果の行は削っていない。該当しないログは原文のまま。
- `.ips`全体は端末固有情報を含むため保存しなかった。抽出時にPID/UID、端末・起動セッションの識別子、process絶対パス、全ロード画像、他thread、命令バイト列、診断用metadataを除外した。上のJSONは元報告の一部であり、報告全体を復元したものではない。
- 認証情報を示す文字列は元ログの点検では見つからず、認証情報を除く置換は発生していない。保存ファイルには絶対ホームパスと一般的なtoken/Authorization形式が残っていないことを確認した。
- release build、Windows実機/CI、実GUI・実IME、#23の性能測定は**未実施**。対応するログは存在しない。これらが必要な変更の前に、対象SHA・環境・fixture・コマンド・終了コードとrun/artifactを伴って取得する。

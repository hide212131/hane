# HANE-PATCH: accesskit_windows 0.34.0

## 出典

- crate: `accesskit_windows` 0.34.0（crates.io published archive）
- crate SHA-256: `106c2b961215864d1c2e703ee63269c25c4e80a577ffb2c1017b9c17dcdf83a1`
- upstream commit: `c88605b96d04431f9c3c792464a0f2f253480e94`（`platforms/windows`）
- ライセンス: MIT OR Apache-2.0（`LICENSE-MIT` / `LICENSE-APACHE` を保持）
- Chromium 由来のライセンス: `src/node.rs` の Chromium 由来部分には、crate 全体の MIT OR Apache-2.0 に加えて BSD-style license（Copyright 2015 The Chromium Authors）が適用される。upstream commit `c88605b96d04431f9c3c792464a0f2f253480e94` の root `LICENSE.chromium` を `vendor/accesskit_windows/LICENSE.chromium` として同梱する。
- 入力の正本: `docs/implementation/issue-570-windows-uia-invalid-hwnd-reference/manifest.json.md`

## 変更理由（Issue #570）

upstream の `client_top_left` は `ClientToScreen` の失敗を `unwrap()` していた。無効な HWND（`ERROR_INVALID_WINDOW_HANDLE` = 0x80070578）では、`Result` を返す COM callback の内側で panic する。この変更は、その失敗だけを既存の `UIA_E_ELEMENTNOTAVAILABLE` に写像し、既存の `Result` 境界へ伝播させる。

panic の通常の発生契機、#564 起因かどうか、旧 abort の扱いは確定していない。この変更では決めない。

## 意味的な変更

| ファイル | 変更 |
| --- | --- |
| `src/util.rs` | `client_to_screen_error` を追加し、`ERROR_INVALID_WINDOW_HANDLE` のみ `element_not_available()` に写像する。他の OS error / HRESULT はそのまま返す。`client_top_left` は `Result<Point>` を返す。直下に単体テストを追加。 |
| `src/context.rs` | `Context::client_top_left` が `windows::core::Result<Point>` を返す。 |
| `src/node.rs` | `BoundingRectangle` / `ElementProviderFromPoint` / `RangeFromPoint`（`ITextProvider`）で `?` により伝播する。`BoundingRectangle` は bounding box がある場合だけ `client_top_left` を呼ぶ（upstream と同じ）。 |
| `src/text.rs` | `GetBoundingRectangles` で `?` により伝播する。bounding box が空なら従来どおり null を返す。 |
| `src/tests/mod.rs` | `mod invalid_hwnd;` を 1 行追加。 |
| `src/tests/invalid_hwnd.rs` | 新規。COM interface を直接呼び、null / bogus / 破棄済み HWND の HRESULT と、有効 HWND の座標・hit-test・text range を検証する。 |

上記以外のファイルは upstream と同一の内容を意図している。

## 省略したファイル

次の upstream ファイルは、手作業で bytes/hash を検証できないため複製していない。ビルド・テストには使われない。

- `CHANGELOG.md`
- `Cargo.lock`

## 検証

この環境ではコマンドを実行していない。次の確認が必要。

- 未変更ファイルの bytes / SHA-256 と manifest の照合
- `cargo test --manifest-path vendor/accesskit_windows/Cargo.toml`（Windows）
- workspace の default / all-features の test と Clippy、Windows CI
- 通常 Release の Windows 実機確認（Issue #570 の受入条件）

Windows 上の source だけから OS 挙動の同等性やリリース安全性は主張しない。

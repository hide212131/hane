# Windows P3 実画面再確認 — PR #491

- 実施日: 2026-10-17 JST
- PR: [hide212131/hane#491](https://github.com/hide212131/hane/pull/491)
- 検証対象commit: `0a9bc16ae2bdef04ec0dbe2f4f72ecad0af61aa7`
- 実行開始時に取得したPR head: `0a9bc16ae2bdef04ec0dbe2f4f72ecad0af61aa7`。再開手順コメントが示す旧対象 `e6c6e3e26fea621bafcb60cc3b04d656c33a2194` からPR headが進んでいたため、このheadへ対象を取り直した。
- PR base: `a0eef8800b91aefec083d8a5cc97359da191c802`
- 本記録はWindowsの不足していた手動検証証拠。設計・製品コードは変更していない。

## 実行環境とビルド

- Windows 11 Home 10.0.26200 (build 26200)、ARM64。CPU情報はApple silicon。
- Rust `1.98.1 (48a229cea 2026-09-01)`、target `aarch64-pc-windows-msvc`。Cargo `1.98.1 (797e8a9bc 2026-08-05)`。
- Japanese (Japan) Microsoft IME。表示スケール200%（LogPixels=192）。アプリ画面のキャプチャは962×790。
- `CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_INCREMENTAL=0` を設定して、`rustup run 1.98.1-aarch64-pc-windows-msvc cargo build --locked -p hane -j 1` が成功。自動テストは実行していない。
- 起動した実行ファイル: `C:\Users\hide2\source\Hane-Windows\hane-pr491-p3-validation-20261016\target\debug\hane.exe`
- 実行ファイルSHA-256: `F6AB26D553F0395A887D9FF8919621CE9CD15A7C3012DA63FCDFBA4CFA420511`
- 起動プロセスのExecutablePathとコマンドラインを確認し、この実行ファイルと検証専用workspaceを指定していることを照合した。

## 隔離とfixture復元

アプリは検証専用workspace `p3-windows-validation-20261017-head-0a9bc16\workspace` を明示して起動した。設定ディレクトリは `HANE_STATE_DIR=<検証root>\state`、Codex設定ディレクトリは `CODEX_HOME=<検証root>\state\codex-home` に分離した。AI接続用環境変数は設定せず、AIサービスへの送信はない。

試験後、アプリを閉じてfixtureを開始時状態に復元した。開始時と終了時のSHA-256が一致する。

- `manual.md`: `F87B0A39BE53364965CF8717923ABE296C3A934752AB6283B0B18660CBFD1321`
- `rename-source.md`: `70404C3E5850CC40FE03510A7F7CE3B5BBE7937D97C8D46C18B9F674E3694891`
- 終了時のworkspaceにはこの2ファイルだけがある。

## 操作と結果

| 項目 | 実操作と画面上の結果 | 判定 | 証拠 |
|---|---|---|---|
| 本文IME・取消 | `本文 IME: SELECTION-本文-東京` 全体を選択し、Microsoft IMEで `ka` を入力。本文位置に「か」の候補が表示された。Escapeで候補を取り消すと、元の行が残り選択も維持された。 | 合格 | [合成中](screenshots/02-body-ime-composing-selection.jpg)、[取消後](screenshots/03-body-ime-cancel-restores-selection.jpg) |
| 本文IME・確定 | 選択状態で `nihongo` を入力し、候補一覧から「日本語」を選択してEnter。選択行が「日本語」に置き換わり、本文に確定した。 | 合格 | [候補一覧](screenshots/04-body-ime-japanese-candidates.jpg)、[確定後](screenshots/05-body-ime-committed.jpg) |
| Filter IME・確定/取消 | Filterに `ka` を入力して候補を表示し、Enterで「科」を確定。結果は「No matching files」。確定した「科」を選択して再度 `ka` を合成しEscapeすると、元の「科」と選択が戻った。本文の表示は変わらない。 | 合格 | [候補表示](screenshots/06-filter-ime-composing.jpg)、[確定後](screenshots/07-filter-ime-committed.jpg)、[選択上の取消後](screenshots/09-filter-ime-cancel-restores-selection.jpg) |
| 選択置換・両方向 | 本文行をHome+Shift+Endで順方向選択し、`選択置換後: 日本語-😀-é` に置換。次に同じ行をEnd+Shift+Homeで逆方向選択し、`逆方向選択置換: 日本語-😀-é` に置換。日本語、絵文字、`e`+U+0301の結合文字が表示された。 | 合格 | [順方向の置換後](screenshots/11-body-selection-replaced-unicode.jpg)、[逆方向選択](screenshots/12-body-reverse-selection.jpg)、[逆方向の置換後](screenshots/13-body-reverse-selection-replaced.jpg) |
| 本文Copy/Cut/Paste | 日本語・絵文字・結合文字を含む `Clipboard source` 行を全選択し、Ctrl+C、Ctrl+Xで行が消えること、Ctrl+Vで同じ行が復元されることを確認。 | 合格 | [選択とコピー](screenshots/15-body-copy.jpg)、[切取り後](screenshots/16-body-cut.jpg)、[貼付後](screenshots/17-body-paste-restores-unicode.jpg) |
| Filter貼付 | コピーした本文行をFilterへCtrl+Vで貼付。Filterに合成文字列が表示され「No matching files」となり、本文側は変わらない。 | 合格 | [Filterへの貼付](screenshots/18-filter-unicode-paste.jpg) |
| 名前欄IME・確定/取消 | F2でbasename欄を開き、`nihongo` の候補から「日本語」を選択してEnterで欄へ確定、もう一度Enterで `日本語.md` にrename。別にF2から `ka` の候補を表示してEscapeすると、元のbasename「日本語」と拡張子`.md`が戻り、もう一度Escapeで編集を取消。 | 合格 | [候補表示](screenshots/20-rename-ime-composing.jpg)、[日本語選択](screenshots/21-rename-ime-japanese-selected.jpg)、[欄への確定](screenshots/22-rename-ime-committed-pending.jpg)、[rename結果](screenshots/23-rename-submit-result.jpg)、[取消後](screenshots/26-rename-ime-cancel-restores-name.jpg) |
| 名前欄Copy/Cut/Paste | F2で選択されたbasenameをCtrl+C、Ctrl+X。拡張子`.md`を保ったままbasename欄が空になり、Ctrl+Vで「日本語」が戻った。Escapeで編集を取消し、ファイル名が保持された。 | 合格 | [コピー](screenshots/28-rename-field-copy.jpg)、[切取り](screenshots/29-rename-field-cut.jpg)、[貼付後](screenshots/30-rename-field-paste-restores-name.jpg)、[取消後](screenshots/31-rename-field-cancel.jpg) |
| rename処理待ち中の入力保護 | Enterでrenameを実行した後、最初に取得できた画面は既にstatus `Renamed`。`Renaming…`の待機状態を画面で捕捉できず、その状態で入力を試す条件を作れなかった。遅延入力を受けた時の保護は判定していない。 | **未検証** | [取得できたrename結果（Renamed）](screenshots/23-rename-submit-result.jpg) |
| 本文検索 | 「本文検索」で `SEARCH-ANCHOR` を検索し `1件・1文書` を表示。Enter後も検索結果を維持、Tabで結果行へフォーカス、Escapeで検索を閉じてファイル一覧に戻った。 | 合格 | [検索結果](screenshots/32-content-search-query.jpg)、[Enter後](screenshots/33-content-search-enter.jpg)、[Tab後](screenshots/34-content-search-tab.jpg)、[Escape後](screenshots/35-content-search-escape.jpg) |
| 文書内検索 | Ctrl+Fで `RENAME-BODY-ANCHOR` を検索し1/1を表示。Enterは「1/1（先頭に戻りました）」、Tab後も本文への入力はなく、EscapeでFindバーが閉じて一致部分が本文で選択された。 | 合格 | [検索結果](screenshots/36-document-find-query.jpg)、[Enter後](screenshots/37-document-find-enter.jpg)、[Tab後](screenshots/38-document-find-tab.jpg)、[Escape後](screenshots/39-document-find-escape.jpg) |
| AI画面 | Alt+IでAIパネルを開いた。接続設定なしで送信ボタンは無効。Enterでは送信されず、Tabで無効ボタンにフォーカス、Escapeで本文へ戻った。サービスへの送信はしていない。 | 合格 | [AI画面](screenshots/40-ai-screen.jpg)、[Enter後](screenshots/41-ai-enter-no-send.jpg)、[Tab後](screenshots/42-ai-tab-send-disabled.jpg)、[Escape後](screenshots/43-ai-escape.jpg) |
| Undo/Redo・保存・再読込 | `BODY-ANCHOR`の行末へ `-EDITED` を入力。Ctrl+Zで削除、Ctrl+Shift+Zで再適用、Ctrl+S後に `Saved` を確認。アプリを閉じ、同じバイナリ・workspaceで再起動すると `-EDITED` が表示された。 | 合格 | [編集後](screenshots/44-body-edit-for-undo-redo.jpg)、[Undo](screenshots/45-body-undo.jpg)、[Redo](screenshots/46-body-redo.jpg)、[保存](screenshots/47-body-save.jpg)、[再読込](screenshots/48-after-reload.jpg) |

## 受入状態

- Windowsの画面で確認した上記項目は合格。ただしrename処理待ち中の入力保護だけは再現できず未検証。
- macOSの残りの必須確認が揃うまで、PR #491はmergeせず、Issue #304もcloseしない。今回それらの状態変更は行っていない。
- 全画面証拠はこのフォルダーの `screenshots/` に保存した。初期の隔離workspace画面は[こちら](screenshots/00-start-state.jpg)。

# Issue #506: Finder / Explorer からファイルをドロップして開く

## 状態

単一ファイルの外部ドロップを既存の open 経路へ接続する実装を、PR #507 に反映済み。Draft は macOS Finder と Windows Explorer の native GUI 確認待ち。

- 基準 main: `e82b9a9fe42280bfa78497746bcd7e21c73430c5`
- 自動検証対象の実装 commit: `5e7ebc47e064e4765a8558a5f8054f7c0d5f02b2`
- 自動検証: [AADW verification run](https://github.com/hide212131/hane/actions/runs/39021003186) 成功。workspace regression tests と lint が成功。
- native GUI確認: 未実施。Finder / Explorer からの実操作、dirty session時の表示、drop後の本文入力・選択への影響は確認待ち。
- 受入条件の正本: [Issue #506](https://github.com/hide212131/hane/issues/506)

## 調査した現行経路と実装

current main には OS からの外部ファイルドロップを受け取る GPUI 基盤がある。

- `vendor/gpui/src/interactive.rs`: `ExternalPaths` と `FileDropEvent`
- `vendor/gpui/src/elements/div.rs`: 型付き `on_drop`
- `vendor/gpui/src/platform/mac/window.rs` と `vendor/gpui/src/platform/windows/window.rs`: native file-drop event
- `crates/ui/src/view.rs`: root view で `ExternalPaths` を受け、1件だけ `EditorView::open_path` に渡す。`open_path` の既存 dirty-session 保護を再利用する。

複数パスは開かず、一度に1項目と案内する。空のdropは無視する。フォルダー判定のために filesystem metadata を同期取得せず、folder drop で Work folder に切り替えない。root の `on_drop` がdropイベントを処理し、本文への mouse-up として扱わせない。

## 範囲と関連作業

- macOS Finder と Windows Explorer から Hane のウィンドウ全体へ単一ファイルをドロップして開く。
- 新しい読込・session・保存経路は作らず、既存 `open_path` を再利用する。
- GPUI vendor、OS file association、ファイル移動・コピーは変更しない。
- Issue #499 / Draft PR #500 の Finder「このアプリケーションで開く」とは別の画面内入力経路。
- Issue #22 のWindows全体対応に依存しない。この機能のExplorer経路にはnative GUI確認が必要。

## 実装と受け入れ確認

- [x] root view が単一の `ExternalPaths` drop を受け、既存 `open_path` を呼ぶ。
- [x] 複数項目を拒否し、1項目ずつと案内する。パス選別の自動テストを追加。
- [x] folder drop で Work folder に切り替えず、drop handler 内で metadata を取得しない。
- [x] root drop handler がdrop完了を本文の mouse-up / 選択として処理させない。
- [x] workspace regression tests と lint が成功。
- [ ] dirty session 保護、Finder / Explorer の単一ファイルdrop、Openダイアログ・CLI open・通常テキストドラッグ選択の回帰をnative GUIで確認し、結果を記録する。

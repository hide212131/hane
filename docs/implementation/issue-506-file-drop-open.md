# Issue #506: Finder / Explorer からファイルをドロップして開く

## 状態

Claude Code に製品コードとテストの実装を依頼するための準備文書。現在の差分はこの文書だけで、製品実装・テスト・GUI確認は未実施。

- 基準 main: `e82b9a9fe42280bfa78497746bcd7e21c73430c5`
- 受入条件の正本: [Issue #506](https://github.com/hide212131/hane/issues/506)

## 調査した現行経路

current main には OS からの外部ファイルドロップを受け取る GPUI 基盤がある。

- `vendor/gpui/src/interactive.rs`: `ExternalPaths` と `FileDropEvent`
- `vendor/gpui/src/elements/div.rs`: 型付き `on_drop`
- `vendor/gpui/src/platform/mac/window.rs` と `vendor/gpui/src/platform/windows/window.rs`: native file-drop event
- `crates/ui/src/view.rs`: `EditorView::open_path` が既存のファイル読込と未保存編集保護を担当する

Hane の view は `ExternalPaths` の `on_drop` をまだ登録していない。GPUI がドロップ完了を mouse-up に変換するため、root view の drop handler がイベントを処理し、通常の本文クリックや選択へ伝播させないようにする。

単一のドロップパスは `open_path` へ渡す。既存の `open_external_path` は `path.is_dir()` により Work folder へ切り替えるため、今回の file-only input では使わない。ドロップ処理中に filesystem metadata を同期取得せず、path の読込は既存の background open 経路へ任せる。複数パスは開かず、一度に1項目だけを落とすよう案内する。

## 範囲

- macOS Finder と Windows Explorer から Hane のウィンドウ全体へ単一ファイルをドロップして開く。
- Hane の既存 `open_path` を再利用する。新しい読込・session・保存経路を作らない。
- 現在の dirty session に対する拒否動作を維持する。
- 複数項目を含むドロップは開かず、利用者へ一度に1項目と伝える。
- フォルダーのドロップで Work folder へ切り替えない。
- GPUI vendor、OS file association、ファイル移動・コピーは変更しない。

## 関連作業と依存

- Issue #499 / Draft PR #500 は Finder の「このアプリケーションで開く」イベントを扱う。今回の画面内ドロップとは別の経路で、実装を混ぜない。
- Issue #22 は Windows 全体の受け入れを扱うが、現行 GPUI の Windows drop handler は存在する。この狭い UI 接続は #22 全体に依存しない。今回の Windows acceptance には native GUI 確認を含める。
- 別の責務整理は不要。current main の GPUI `on_drop` と Hane `open_path` を使う。

## 実装と受け入れ確認

- [ ] root view が単一の `ExternalPaths` drop を受け取る。
- [ ] Finder と Explorer の単一ファイル drop で、そのパスが既存の `open_path` 経由で開く。
- [ ] dirty session があると既存ルールどおり拒否し、本文を失わない。
- [ ] 複数項目は開かず、directory drop は Work folder に切り替えない。
- [ ] drop したパスが本文への入力や編集選択にならない。
- [ ] Open ダイアログ、CLI open、通常のテキストドラッグ選択に回帰がない。
- [ ] 関連する自動検証と macOS / Windows native GUI の focused scenario を実施し、対象headと結果を記録する。

# Issue #499: macOS FinderからMarkdownを開く

## 対象フェーズ

初回実装。対象Issue: [#499](https://github.com/hide212131/hane/issues/499)

基準 `main`: `e82b9a9fe42280bfa78497746bcd7e21c73430c5`（2026-10-17確認）。

この準備commitは実装範囲を固定する文書のみで、製品コード変更や検証は行っていない。

## 採用する動作

- Haneの現在のMarkdown対象である `.md` を macOS app bundle の文書タイプとして登録する。汎用プレーンテキスト全体はclaimしない。
- Finderの「このアプリケーションで開く」に候補として表示し、既定アプリの変更はFinderから利用者が選ぶ。アプリ起動時にLaunch Servicesの既定値を書き換えない。
- macOSのファイルオープンイベントを、Haneの既存の文書オープン経路へつなぐ。Hane未起動時の初回起動と、起動中のアプリへのイベントの両方を考慮する。初回イベントが既定Work folderや初回フォルダ選択より後回しになり、不要なフォルダPromptが文書表示を塞がないようにする。
- ローカルfile URLからパスを安全に取り出し、Haneの既存のsession・未保存編集保護を使う。任意URLの起動や未保存文書の破棄を新設しない。
- READMEにFinderの「情報を見る」から `.md` の既定アプリを変更する手順を記す。
- 配布bundle検証は既存入口の `scripts/bundle-macos.sh` に実装する。現在のmacOS release jobは同scriptを実行するため、`.github/` workflowの変更は不要かつ対象外。

## 実装で読む既存経路

- `scripts/bundle-macos.sh` は現在 `Hane.app/Contents/Info.plist` を生成する。
- `.github/workflows/windows-release.yml` のmacOS jobが同scriptを実行し、bundleを検証して配布する。確認用に読むだけで変更しない。
- `crates/app/src/main.rs` はCLIのパス引数を読み、既存の `EditorView::open` / `open_external_path` を呼ぶ。GPUIには `App::on_open_urls` があるが、現行の起動経路からは登録されていない。
- Work folderのMarkdown列挙は `.md` のみを対象とする。

## 受け入れ条件

- 生成した `Hane.app` の文書タイプが `.md` を扱うことをInfo.plist検査で確認できる。
- Finderの「このアプリケーションで開く」から選んだ `.md` が、Haneの新規起動時と起動中の両方で開く。
- ファイルオープンイベントが既存のdirty-session保護を通り、未保存編集を黙って失わない。
- `scripts/bundle-macos.sh` がbundle文書タイプを検証し、既存macOS release jobでその検証が実行される。
- READMEに利用者が既定アプリを設定する手順がある。
- CLIのファイル指定と通常起動が維持される。

実装に直接関係するテストを追加し、macOSの配布bundle検査を通す。実機GUI確認が必要な場合は、未起動・起動中のFinder選択を区別して結果を記録する。確認していない動作を成功として扱わない。

## 関連作業・依存

- Issue #22のWindowsファイル関連付けは対象外。本IssueはmacOS Finder連携に限定する。
- PR #495（Issue #494）はショートカット/入力先の作業であり、今回のbundle宣言とは独立する。READMEやviewファイルの差分が重なる場合はcurrent mainとPRの状態を確認して調整する。
- `.github/` のworkflowはprotected pathなので変更しない。Issueのbundle検査条件は `scripts/bundle-macos.sh` とそれを呼ぶ既存jobを使って満たす。
- 性能、保存形式、IMEの契約を変更しないため、Issue #297全体の完了待ちは不要。

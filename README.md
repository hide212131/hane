# Hane

![Hane logo](./assets/phase4-feather.svg)

Hane は、巨大な Markdown 文書も軽快に編集できるデスクトップ向け Markdown エディタです。Rust と GPUI で作られており、Typora のように Markdown 記号を普段は隠して見やすく表示し、カーソルや選択範囲が構文に入ったときだけ記号を表示します。編集中のデータは常に Markdown のまま保持されるので、いつでもそのままのテキストとして扱えます。

## 主な機能

- **軽快な編集** — 100MB 級の Markdown でもスムーズにスクロール・編集できます。
- **すっきりした表示** — 見出しや強調などの Markdown 記号を普段は隠し、編集する箇所だけ記号を表示します。
- **画像・表の表示** — 画像やパイプテーブルをインラインで表示します。
- **安全な保存** — atomic save により保存中の破損を防ぎます。自動保存にも対応しています。
- **Recent Files** — 最近開いたファイルは、画面上部の「ファイル」メニューから開けます。
- **設定の保存** — テーマや自動保存などの設定は次回起動時も引き継がれます。
- **テーマ** — 一般設定で system / light / dark のテーマを切り替えられます。
- **1行のフッター** — 保存状態、文字数と選択文字数、表示倍率、「AIに指示」を低いフッターにまとめています。幅が狭いときは「その他」や「状態と操作」のメニューに集約されます。
- **Work folder mode** — フォルダを指定すると、配下の Markdown 一覧をサイドバーから選んですぐ編集できます。`+` を押すだけでファイル名を聞かれずに新しいメモを書き始められ、最初の H1 がファイル名になります。
- **本文検索** — Work folder 内の Markdown 本文を検索し、一致した箇所へ移動できます。開いている未保存文書や下書きも検索対象です。

## 動作環境

- macOS Apple Silicon — [最新の GitHub Release](https://github.com/hide212131/hane/releases/latest) から `hane-macos-arm64.zip` をダウンロードし、展開して `Hane.app` を開きます。[ZIP を直接ダウンロード](https://github.com/hide212131/hane/releases/latest/download/hane-macos-arm64.zip)することもできます。
- Windows 11 x64 — [最新の GitHub Release](https://github.com/hide212131/hane/releases/latest) から `hane-windows-x64.zip` をダウンロードし、展開して `hane.exe` を実行するポータブル運用のみに対応しています（Rust のインストールは不要）。[ZIP を直接ダウンロード](https://github.com/hide212131/hane/releases/latest/download/hane-windows-x64.zip)することもできます。個人利用の動作確認段階（dogfooding）であり、正式な Windows サポートは今後の課題です。

macOS 版の ZIP を初めて置くときは、展開後の `Hane.app` と、その隣にある `Hane.app.update` を必ず一緒に、書き込み可能なローカルの場所（例: `/Applications`）へ移してから起動してください。`Hane.app.update` にはアプリ内更新が本体を検証するための更新用データが入っているため、`Hane.app` だけを移さないでください。ダウンロード場所から直接起動して App Translocation される配置では、アプリ内更新の対象外になります。

macOS 版は Apple Developer ID の署名も notarization も付けていません。そのため、ダウンロード後の初回起動時に macOS のセキュリティ警告が表示される場合があります。

「開発元を検証できない」などの警告で止まった場合は、`Hane.app` を一度開いて警告を閉じたあと、「システム設定」→「プライバシーとセキュリティ」の「セキュリティ」欄に表示される「このまま開く」を選び、確認画面で本人が許可してください。この許可はアプリごとに初回だけ必要です。「壊れているため開けません」と表示される場合は、この手順では解決しません。許可操作で進めず、ダウンロードし直すか、[Issue #510](https://github.com/hide212131/hane/issues/510) で報告してください。

Windows版は [最新のGitHub Release](https://github.com/hide212131/hane/releases/latest) から `hane-windows-x64.zip` を取得し、ユーザーが書き込めるフォルダへ展開して `hane.exe` を実行します。設定と Recent Files は `%LOCALAPPDATA%\Hane` に保存されます（`HANE_STATE_DIR` で変更可能）。v0.19.1以降のZIPにはWindows 11のExplorer拡張も同梱します（v0.19.0のZIPには含まれません）。CLI の使い方とエクスプローラーへの統合については [Windows CLI / Explorer 統合](#windows-cli--explorer-統合) を参照してください。

> macOS で Metal Toolchain を別途導入しなくても動かせるよう、GPUI の `runtime_shaders` を使っています。

### macOS Finder から Markdown を開く

`Hane.app` は `.md` を扱える文書タイプとして登録されており、Finder で `.md` を右クリックして「このアプリケーションで開く」から Hane を選べます。Hane が起動していなくても、起動中でも開けます。未保存の編集がある場合は、既存の保存確認を通らずに内容が置き換わることはありません。

Hane は既定のアプリを自動では変更しません。`.md` をダブルクリックで Hane で開きたい場合は、次の手順で設定します。

1. Finder で `.md` ファイルを選び、「ファイル」メニューの「情報を見る」（`⌘I`）を開きます。
2. 「このアプリケーションで開く」で `Hane` を選びます。
3. 同じ種類のすべての書類に適用するには「すべてを変更…」を押します。

### Windows CLI / Explorer 統合

```text
hane.exe
hane.exe path\to\document.md
hane.exe path\to\folder
hane.exe --register-context-menu
hane.exe --unregister-context-menu
```

- `hane.exe`（引数なし）— 保存済みの既定フォルダを開きます。既定フォルダが未設定の場合（初回起動時など）は、起動後にフォルダ選択ダイアログが表示され、選んだフォルダが以後の既定フォルダとして保存されます。
- `hane.exe path\to\document.md` — 指定した Markdown ファイルを開きます。
- `hane.exe path\to\folder` — 指定したフォルダを Work folder mode で開きます。既にHaneが起動していればそのウィンドウで切り替えます。既定フォルダは変更しません。
- `hane.exe --register-context-menu` — Windows エクスプローラーのフォルダ右クリックメニューに「Haneで開く」を追加します。現在のユーザーのみに登録され、管理者権限は不要です。Windows 11では同じフォルダ内に署名済みの `Hane.ShellIntegration.msix` が必要です。
- `hane.exe --unregister-context-menu` — 追加した「Haneで開く」をエクスプローラーのメニューから削除します。

ファイルの右クリック登録は設定画面の「一般」→「Windowsとの連携」から切り替えます。Windows 11では `IExplorerCommand` を含む署名済みの疎パッケージを導入して、新しい右クリックメニューへ表示します。Windows 10では従来の右クリックメニューを使用します。ファイル登録を無効にしてもフォルダ登録は変更しません。

Windows 11向けの新しい配布ZIPには `Hane.ShellIntegration.msix`、署名用の公開証明書 `Hane.ShellIntegration.cer`、`Trust-HaneShellCertificate.ps1` が `hane.exe` と同じフォルダに、Explorer拡張DLLが `shell-integration\<バージョン>\hane_shell_extension.dll` に入ります。更新用の `hane-updater.exe` と署名付きの `hane-install.json`（所有ファイル一覧）も同梱します。自己署名証明書のため、初回だけ配布先PCで次を行ってください。秘密鍵を配布先で作成する必要はありません。

1. 配布ZIPを信頼できるGitHub Releaseから入手し、展開する。証明書を信頼する前に、ZIPの入手元を確認する。
2. 管理者としてPowerShellを開き、展開したフォルダに移動する。`Unblock-File .\Trust-HaneShellCertificate.ps1` を実行し、続けて `.\Trust-HaneShellCertificate.ps1` を実行する。これは同梱の公開証明書だけをこのPCの `LocalMachine\TrustedPeople` に登録し、MSIXの署名を確認する。
3. 通常権限で `hane.exe` を起動し、「一般」→「Windowsとの連携」でファイルの右クリック登録を有効にする。設定の無効化はメニュー項目を消しますが、証明書の信頼登録は残ります。

ソースからWindows 11向けパッケージをビルドする例（Windows SDK、MSVC、署名証明書が必要）:

```powershell
.\windows\shell-extension\build-package.ps1 -ExePath .\target\windows-x64-1.98.1\release\hane.exe -Architecture x64 -CertificateThumbprint <証明書の拇印>
```

生成される `Hane.ShellIntegration.msix` は `hane.exe` と同じディレクトリに、`hane_shell_extension.dll` は `shell-integration\<バージョン>\` に配置します。登録時にHaneが現在のユーザーへパッケージを導入します。証明書を指定しないビルドは未署名であり、Explorerでは使用できません。配布用の署名秘密鍵はGitHub Actionsのシークレットにのみ設定し、ZIPやリポジトリには含めません。リリース担当者向けの設定手順は [Windows署名手順](docs/windows-release-signing.md) を参照してください。

### ZIP更新の配布物（保守者向け）

アプリ本体の更新（Issue #510、[ADR-0034](docs/adr/0034-zip-application-updates.md)）に使うため、各Releaseは次の4ファイルをそろえて公開します。`hane-update.json` と `hane-update.json.sig` は、両ZIPの実サイズ・SHA-256・Windowsの所有ファイル一覧・macOSのBundle ID/版/署名済みinventoryのhashから生成し、Hane更新専用のEd25519鍵で署名します。

- `hane-windows-x64.zip` — 上記の構成に加え、署名済みの `hane-install.json` / `hane-install.json.sig`。
- `hane-macos-arm64.zip` — ローカルのad-hoc署名で封印した `Hane.app`（`Contents/Helpers/hane-updater` を含む。Apple Siliconは署名のない実行コードを起動しないため、helper → 本体 → bundleの順に内側から署名し、`codesign --verify --deep --strict` で確認します）と、その隣の `Hane.app.update/<バージョン>-<source SHA>/hane-install.json` / `.sig`（更新用Ed25519鍵で署名したinventory。`_CodeSignature/CodeResources` を含む全ファイルを列挙し、アプリの外に置くので封印を壊しません）。Apple Developer ID・Team ID・notarizationは使いません。`Hane.app` と `Hane.app.update` は同じ場所に並べて置いてください。更新で入れ替えた後のinventoryはアプリごとの `.hane-update-*` 内に保存されます。v0.27.0のようにinventoryがアプリ内にある旧形式は、次の更新までの移行用に一度だけ読み取ります。
- `hane-update.json`, `hane-update.json.sig` — 検証済みcatalogとその署名。

更新対応版の最初の導入は、これまでどおり手動でZIPを展開します。更新用の署名鍵・公開鍵・MSIX署名証明書が未設定の間は、リリースは失敗し、更新対応のReleaseは公開されません。手順と必要な設定は [リリース手順](.github/RELEASE_AUTOMATION.md) と [Windows署名手順](docs/windows-release-signing.md) を参照してください。

Windows 11 のメニュー表示状態は実行ファイルと同じディレクトリの `Hane.ShellIntegration.enabled`（ファイル）または `Hane.ShellIntegration.folder.enabled`（フォルダ）で管理します。パッケージ化された Explorer 拡張やそこから起動した Hane では通常の HKCU\Software\Classes キーが仮想化されるため、Windows 11 のファイル登録はこのキーを新規作成しません。旧版が作成した Hane 所有のキーが現在のプロセスから見える場合だけ削除します。無効化時には対応するマーカーを削除します。署名済みパッケージ自体は再有効化を速くするため残りますが、マーカーがない項目は Explorer に表示されません。パッケージを更新した直後に新しい項目が見えない場合は Explorer ウィンドウを開き直してください。

## インストールと起動

ソースからビルドして起動します。

```sh
# そのまま起動
cargo run -p hane

# ファイルを指定して起動
cargo run -p hane -- path/to/document.md

# フォルダを指定して work folder mode で起動
cargo run -p hane -- path/to/work-folder
```

快適に使うにはリリースビルドをおすすめします。

```sh
cargo run --release -p hane
```

## 使い方

起動したら Markdown を入力・編集するだけです。主なキーボードショートカット（macOS / Windows・Linux）:

| 操作 | macOS | Windows / Linux |
| --- | --- | --- |
| カーソル移動 | 矢印キー | 矢印キー |
| 選択 | Shift + 矢印キー | Shift + 矢印キー |
| 行頭・行末へ移動 / 選択 | Command + ← / →、Shift 付きで選択 | Home / End、Shift 付きで選択 |
| 文書の先頭・末尾へ移動 / 選択 | Command + ↑ / ↓、Shift 付きで選択（Shift + Home / End でも選択） | Ctrl + Home / End、Shift 付きで選択 |
| 単語単位で移動 / 選択 | Option + ← / →、Shift 付きで選択 | Ctrl + ← / →、Shift 付きで選択 |
| 前・後の単語を削除 | Option + delete / Option + Fn + delete（前方削除キー） | Ctrl + Backspace / Delete |
| 段落単位で移動 / 選択 | Option + ↑ / ↓、Shift 付きで選択 | Ctrl + ↑ / ↓、Shift 付きで選択 |
| 1 画面分カーソルを移動 | Option + PageUp / PageDown | PageUp / PageDown（Shift 付きで選択） |
| 画面だけをスクロール（カーソル・選択は動かさない） | Home / End（文書の先頭・末尾）、PageUp / PageDown（1 画面分） | なし |
| カーソル行を画面の中央に表示 | Control + L | なし |
| 全選択 | Command + A | Ctrl + A |
| コピー / 切り取り / 貼り付け | Command + C / X / V | Ctrl + C / X / V |
| 元に戻す / やり直す | Command + Z / Command + Shift + Z | Ctrl + Z / Ctrl + Y（Ctrl + Shift + Z も可） |
| 開く | Command + O | Ctrl + O |
| フォルダを開く（work folder mode） | Command + Shift + O | Ctrl + Shift + O |
| Work folder の本文を検索 | Command + Shift + F | Ctrl + Shift + F |
| 左サイドパネルを開く / 閉じる | Command + B | Ctrl + B |
| 保存 / 名前を付けて保存 | Command + S / Command + Shift + S | Ctrl + S / Ctrl + Shift + S |
| 自動保存の切り替え | Command + Option + A | Ctrl + Alt + A |
| 選択中のタブを閉じる（未保存なら確認。タブの中クリック・右クリックの「タブを閉じる」も同じ） | Command + W | Ctrl + W |

行・段落は、画面幅による折り返しではなく、改行で区切られた原文の行を単位にします（Markdown の空行で区切るブロックではありません）。単語は Unicode の単語境界に従い、日本語の辞書による分かち書きはしません。OS 標準のテキスト欄との完全な互換は目指していません。前方への単語移動は、Windows では次の単語の先頭、macOS では単語の末尾に止まります。後方への単語移動は、どちらの OS でも語頭へ進みます。

Emacs 風の Control + 英字（Control + A / E / B / F / K / Y など）は割り当てていません。必要な場合は Karabiner-Elements（macOS）や Keyhac（Windows）などでキー配列を変更してください。

選択した文章への AI 指示は、フッターの「AIに指示」ボタン（macOS / Windows 共通）から開けます。Windows では Alt + I も使えます（Hane 固有のキーで、OS 標準の編集キーではありません）。

テーマと自動保存は設定の「一般」で変更します（変更はすぐ反映・保存され、保存に失敗したときは設定画面に表示されます）。自動保存の切り替えキーも引き続き使えます。単独ファイルを開いているときはフッターの「設定」から、作業フォルダを開いているときはサイドバー下部の設定から開けます。

フッターの左端には現在の文書の保存状態（保存済み / 未保存 / 下書き / 保存処理中… / 保存できませんでした / 外部で変更されています / ファイルが見つかりません）を表示します。保存の失敗と下書き復旧の警告は、保存状態を押すと全文を確認できます。タブを移動しても、その文書の保存が成功するまで失敗は残ります。

文字数は、元のテキストに含まれる Unicode スカラー値の数です。空白・改行・Markdown 記号を含み、結合文字や連結した絵文字は複数文字として数えます。本文を選択すると、選択範囲の文字数も表示します。表示倍率は 75% / 100% / 125% / 150% / 200% から選べ、ホイールやピンチによる連続した拡大縮小もこれまでどおり使えます。

最近開いたファイルは、画面上部の「ファイル」メニューの「最近開いたファイル」から開きます。

### Work folder mode

起動時にディレクトリを引数として渡すか、起動後に Command + Shift + O（Windows / Linux は Ctrl + Shift + O）でフォルダを選ぶと、その配下の Markdown 一覧が左サイドバーに表示されます。一覧からメモを選ぶと保存ダイアログを挟まずすぐ編集でき、`+` を押すとファイル名を聞かれない空のメモがその場で開きます。

左サイドパネルは、パネル左上（閉じている間は本文ヘッダー左端）のボタン、または Command + B（Windows / Linux は Ctrl + B）で開閉できます。開閉は表示を切り替えるだけで、幅・選択・フィルター・本文検索の結果や本文の編集状態は保たれます。名前変更中や日本語入力の変換中は開閉されません。本文検索（Command / Ctrl + Shift + F）やフォルダを開く操作ではパネルが再表示されます。閉じている間も、設定はフッターから開けます。

新規メモは最初の H1（`# ...`）がファイル名になり、自動命名対象のメモは以後 H1 を変更するとファイル名も追従します。ファイル名と H1 が最初から異なる既存の Markdown は、自動命名対象と判断されないため勝手にリネームされません。

H1 をまだ付けていない新規メモは `.hane/drafts` にバックグラウンドで保存されるため、アプリを終了・クラッシュさせても内容を失いません。

#### 本文検索

Work folder mode のサイドバーで「本文検索」を選ぶか、Command + Shift + F（Windows / Linux は Ctrl + Shift + F）を押します。検索語を入力すると確定後に検索が始まり、初期設定では大文字・小文字を区別しません。必要に応じて大小文字の切り替えを使ってください。

結果をクリックするか、上下キーで選んで Enter を押すと、その文書を開いて一致箇所を選択します。開いている文書は未保存の編集内容を優先し、Work folder の下書きも検索します。検索後に文書内容が変わっていた場合は古い位置へ移動せず、再検索して選び直すよう案内します。

## ライセンス

Apache-2.0

## 開発者向け

現在の crate 構造とデータフローは [architecture](docs/architecture.md) を参照してください。

標準の検証は次のとおりです。

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Windows の開発環境を再現する場合は、PowerShell で次を実行します。`-Install` は
winget 経由で LLVM と Visual Studio C++ Build Tools を導入し、`-Verify` はテスト、
フォーマット、clippy、Windows バイナリのビルドまで実行します。ターゲットはホストの
x64 / ARM64 を自動判定し、必要なら `-Architecture x64` または `-Architecture arm64` で
明示できます。

```powershell
.\scripts\setup-windows-dev.ps1 -Install -Verify
```

計測は `scripts/measure.sh all`（`startup` / `input` / `memory` も指定可）、画面キャプチャは
`scripts/capture.sh editor`（`cursor-boundary` / `cursor-scroll` も指定可）で実行します。
性能基準線は [docs/baseline/](docs/baseline/)、設計判断は [ADR index](docs/adr/README.md)、
リファクタリングの進捗は [実施計画](docs/refactor-execution-plan.md) を正とします。

ローカル Mac での GUI 検証（起動・撮影・終了の証拠を残す最小コマンド）は
`python3 scripts/gui_validate.py editor`（`cursor-boundary` / `cursor-scroll` も指定可）で実行します。
実行方法・結果 JSON の形式・終了コードは [docs/local-gui-validate-cli.md](docs/local-gui-validate-cli.md)、
設計は [docs/local-gui-validation.md](docs/local-gui-validation.md) と [ADR-0024](docs/adr/0024-local-gui-validation.md) を参照してください。
実画面を使わない自動テストは `python3 -m unittest discover -s scripts/tests` で実行できます。

GUI検証の要否、対象commitの照合、結果の扱いは [GUI検証設計](docs/local-gui-validation.md) を参照してください。

# Windows Explorer 拡張のリリース署名

GitHub Actions の `build-windows` は `hane.exe` と x64 の Explorer 拡張をビルドし、署名済み `Hane.ShellIntegration.msix`、`hane_shell_extension.dll`、公開証明書 `Hane.ShellIntegration.cer`、信頼登録用スクリプトを `hane-windows-x64.zip` に同梱する。署名用の秘密鍵は配布ZIPとリポジトリには含めない。

## 初回のシークレット登録

Windows 実機で Explorer の動作確認に使用した証明書 `9725B0DE9BCA812573A278059750E7C350CFCA8E` は、秘密鍵がエクスポート不可のためGitHub Actionsに移せない。既存証明書と既存の動作確認用パッケージは変更せず、同じPublisher名で新しいエクスポート可能なコード署名証明書を作る。PowerShellで次を実行する。PFXの保存先は他人が読めない場所にする。

```powershell
$cert = New-SelfSignedCertificate -Type Custom -KeyUsage DigitalSignature `
  -Subject 'CN=Hane Local Test 2026' -CertStoreLocation 'Cert:\CurrentUser\My' `
  -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}') `
  -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256 `
  -Provider 'Microsoft Software Key Storage Provider' -KeyExportPolicy Exportable `
  -NotAfter (Get-Date).AddYears(2) -FriendlyName 'Hane Release Signing'
$pfxPath = Join-Path $env:USERPROFILE 'hane-msix-signing.pfx'
$pfxPassword = Read-Host 'PFXに設定する新しいパスワード' -AsSecureString
Export-PfxCertificate -Cert $cert -FilePath $pfxPath -Password $pfxPassword
$cert | Select-Object Subject, Thumbprint, NotAfter
```

GitHub の `hide212131/hane` → **Settings** → **Secrets and variables** → **Actions** → **New repository secret** で以下の2件を登録する。

1. `HANE_MSIX_PFX_BASE64`: 上記PFXの全バイトをBase64にした値。PowerShellで `[Convert]::ToBase64String([IO.File]::ReadAllBytes($pfxPath)) | Set-Clipboard` とすれば貼り付けられる。
2. `HANE_MSIX_PFX_PASSWORD`: PFXのエクスポート時に入力したパスワード。Base64にはしない。

貼り付け先を確認し、作業後はクリップボードを消去し、PFXを安全なオフライン保管先へ移すか削除する。パスワード、PFX、Base64値をIssue、PR、チャット、ログ、コミットに貼らない。拇印は公開情報なので共有してもよい。GitHub ActionsはPFXの秘密鍵・Publisher名・コード署名用途・有効期間を確認し、配布する公開証明書とMSIXの署名の拇印が一致することを確認する。

## リリース

新しいリリースではCargoのパッケージバージョンを未使用のバージョンへ更新してmainに反映するか、そのバージョンのタグを作成する。`Release builds` ワークフローがWindows/macOSをビルドし、両方成功した場合だけGitHub ReleaseのZIPを公開する。

Windows ZIPには `hane.exe`、`hane-updater.exe`、署名済み `Hane.ShellIntegration.msix`、公開証明書 `Hane.ShellIntegration.cer`、`Trust-HaneShellCertificate.ps1`、`shell-integration/<バージョン>/hane_shell_extension.dll`、署名付き `hane-install.json` を含める。`build-windows` はZIPを再展開し、MSIXのAuthenticode状態が `Valid` で署名者が同梱の公開証明書と一致すること、MSIXのmanifest版・COMクラスのDLLパス・DLLの版別ディレクトリが一致すること、`hane-install.json` がZIP内の所有ファイルをsize/hash付きで網羅することを確認する。検証のために公開証明書をrunnerの `LocalMachine\Root` へ一時的に登録し、検証後に削除する（配布先PCの信頼登録とは無関係）。

## 更新用の署名資格情報（保守者が設定する値）

次の値は保守者だけが設定する。値を推測・生成・commitしない。`prepare` は**存在の真偽値だけ**を評価し、値は署名を行うstepにだけ渡る。1つでも欠けるとリリースは失敗し、未署名のReleaseは作られない。Apple Developer ID・Team ID・notarization・stapleに関する設定は不要で、未設定でもmacOS ZIPの作成やWindows配布は止まらない。macOS ZIPでは、ad-hoc署名で封印した `Hane.app` の外側に `Hane.app.update/<バージョン>-<source SHA>/hane-install.json` と `hane-install.json.sig` を配置する。更新時の信頼根拠は、このinventoryの更新用Ed25519署名とcatalogの `inventory_sha256` である。inventoryは `_CodeSignature/CodeResources` を含む封印後のアプリ全ファイルを列挙し、生成後にアプリ内へ追加して封印を壊さない。ZIPを展開した `Hane.app` と `Hane.app.update` は同じ場所に並べて置く。

| 種別 | 名前 | 用途 |
|---|---|---|
| secret | `HANE_UPDATE_SIGNING_KEY_BASE64` | Hane更新専用Ed25519鍵の32バイト秘密鍵（Base64）。inventory・catalogの署名step専用。Codex runtime catalogの鍵とは共用しない |
| variable | `HANE_UPDATE_KEY_ID` | 署名ファイルの `key_id`（英数字と `.` `_` `-`、64文字以内） |
| variable | `HANE_UPDATE_PUBLIC_KEY_BASE64` | 上記秘密鍵に対応する公開鍵32バイト（Base64）。本体へ埋め込まれ、署名step・検証が秘密鍵との一致を確認する |
| secret | `HANE_MSIX_PFX_BASE64`, `HANE_MSIX_PFX_PASSWORD` | 前節のMSIX署名用PFX |

公開鍵はビルド時の環境変数として `crates/update` の `TrustPolicy::production()` に埋め込まれる。未設定・不正な値の場合は何も埋め込まれず、更新候補は `trust_unconfigured` で拒否される。

## 更新catalogの発行と順序

`release` ジョブは全入口（main push、tag push、手動dispatch、release-request）で共通のconcurrency group `hane-release-publish` に入り、次を直列に行う。

1. 既存のtag/Releaseを読み、Releaseまたはtagが既にあれば停止する（tag pushで作られたtagだけ例外）。versionが既存の最新stableより新しくなければ停止し、古い版に最新ラベルを付けない。
2. 公開済みReleaseのcatalogを全て取得して署名・schema・repository・channelを検証し、最大の `catalog_sequence` + 1 を採番する。取得・権限・解析・検証の失敗は「初回」と見なさず失敗する。公開済みcatalogが無い最初のリリースだけ、`workflow_dispatch` の `initial_catalog` を明示して sequence 1 を発行する（自動起動では初回扱いにならないため、最初の対応版はこの入力を付けて手動起動する）。重複・u64溢れも失敗する。
3. 実ZIPのサイズ・SHA-256・unpacked size・inventory hash・bundle IDから `hane-update.json` を生成し、署名する。
4. Draftを作り、4アセットをuploadし、再取得して同じ検証（署名、版、source SHA、target、ZIP hash、inventory、bundle metadata）を行う。成功した場合だけstableへ公開し、公開後にもう一度検証する。検証に失敗したDraftは削除され、既存のtag/Release/assetは上書きしない。

期限更新だけの再署名や秘密鍵のローテーションはこの自動処理の対象外で、保守者が明示的に行う。この基盤だけでは実署名・二版更新の実機確認は完了しない（Issue #510のU5）。

自己署名証明書なので、配布先のWindows 11ではREADMEに従って**新しい公開証明書**を `LocalMachine\TrustedPeople` に信頼登録する必要がある。配布先で秘密鍵を作成・配布する必要はない。旧証明書の信頼や旧パッケージはこの作業で自動削除しない。

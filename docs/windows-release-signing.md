# Windows Explorer 拡張のリリース署名

GitHub Actions の `build-windows` は `hane.exe` と x64 の Explorer 拡張をビルドし、署名済み `Hane.ShellIntegration.msix`、`hane_shell_extension.dll`、公開証明書 `Hane.ShellIntegration.cer`、信頼登録用スクリプトを `hane-windows-x64.zip` に同梱する。署名用の秘密鍵は配布ZIPとリポジトリには含めない。

## 初回のシークレット登録

Windows 実機で Explorer の動作確認に使用した証明書は `Cert:\CurrentUser\My\9725B0DE9BCA812573A278059750E7C350CFCA8E` にある。この証明書の秘密鍵を持つ元のPCで、PowerShellから次を実行する。PFXの保存先は他人が読めない場所にする。

```powershell
$cert = Get-Item 'Cert:\CurrentUser\My\9725B0DE9BCA812573A278059750E7C350CFCA8E'
if (-not $cert.HasPrivateKey) { throw '秘密鍵がありません' }
$pfxPath = Join-Path $env:USERPROFILE 'hane-msix-signing.pfx'
$pfxPassword = Read-Host 'PFXに設定する新しいパスワード' -AsSecureString
Export-PfxCertificate -Cert $cert -FilePath $pfxPath -Password $pfxPassword
```

GitHub の `hide212131/hane` → **Settings** → **Secrets and variables** → **Actions** → **New repository secret** で以下の2件を登録する。

1. `HANE_MSIX_PFX_BASE64`: 上記PFXの全バイトをBase64にした値。PowerShellで `[Convert]::ToBase64String([IO.File]::ReadAllBytes($pfxPath)) | Set-Clipboard` とすれば貼り付けられる。
2. `HANE_MSIX_PFX_PASSWORD`: PFXのエクスポート時に入力したパスワード。Base64にはしない。

貼り付け先を確認し、作業後はクリップボードを消去し、PFXを安全なオフライン保管先へ移すか削除する。パスワード、PFX、Base64値をIssue、PR、チャット、ログ、コミットに貼らない。GitHub Actionsは証明書の拇印を照合し、一致しないPFXではリリースしない。

## リリース

現在の `v0.19.0` は既にタグが存在するため、修正をmainへ取り込んだだけでは新しいZIPは公開されない。シークレット登録後、Cargoのパッケージバージョンを次の未使用バージョンへ更新してmainに反映するか、そのバージョンのタグを作成する。`Release builds` ワークフローがWindows/macOSをビルドし、両方成功した場合だけGitHub ReleaseのZIPを公開する。公開後、Windows ZIPに上記4ファイルがあり、MSIX署名の拇印が `9725B0DE9BCA812573A278059750E7C350CFCA8E` であることを確認する。

自己署名証明書なので、配布先のWindows 11ではREADMEに従って**公開証明書**を `LocalMachine\TrustedPeople` に信頼登録する必要がある。配布先で秘密鍵を作成・配布する必要はない。

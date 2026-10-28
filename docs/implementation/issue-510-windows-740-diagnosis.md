# Windows update-prepare error 740: diagnostic checkpoint

関連Issue: [#510](https://github.com/hide212131/hane/issues/510)

状態: 初回診断では原因未確定だった。その後の隔離比較で、同一bytesでもbasenameにより起動可否が変わることを観測した。これを受けてWindowsのtransaction copy名を中立化する実装候補が入っている。Windows内部の原因は確定しておらず、通常CI/exact-head検証と実Windowsでの正式な更新・再起動受入は未確認。

## 観測

- Windows x64上のHane 0.32.0で、候補v0.36.1の「更新準備」が2回ともOS error 740（`ERROR_ELEVATION_REQUIRED`）で失敗した。
- UAC Operational Event ID 1にも同じ時刻帯の子プロセス作成エラーが2件ある。イベントXMLに子プロセスの実行パス、PID、起動フラグは残っていない。
- インストール済みhelperはx64で、埋め込みmanifestと隣接manifestがない。対象パスについてHKCU/HKLMのAppCompat `Layers` 値は見つからなかった。失敗したコピーの正確な絶対パスは残っていない。
- Windowsの `EnableInstallerDetection` は有効。ただしMicrosoftの説明するInstaller Detectionの対象は32-bit実行ファイルなどであり、x64 helperのファイル名だけでは今回のエラーを説明できない。AppCompat database/shimなど他の判定要因は未確認。

## v0.32.0の起動経路

v0.32.0では、準備時にインストール済みの署名済みhelperをtransaction用private directoryへコピーし、そのコピーを `Command::new(exe).spawn()` で引数なし起動する。Windowsでは空の環境へ `SystemRoot` だけを設定し、pipe stdioと `CREATE_NO_WINDOW` を使う。候補ZIPに入っている新helperはこの段階では起動しない。

参照: [helper起動](https://github.com/hide212131/hane/blob/v0.32.0/crates/update/src/helper.rs)、[Windows helperコピー](https://github.com/hide212131/hane/blob/v0.32.0/crates/update/src/platform/mod.rs)、[Microsoft: UAC architecture](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/architecture)。

## 承認済みの隔離起動比較（2026-10-29）

- ユーザー承認後、通常権限でインストール済みhelperの同一バイトコピーを一時フォルダに配置した。元ファイル、通常state、UAC設定は変更していない。
- 4条件（`hane-updater.exe` / `hane-worker.exe` × 継承環境 / `SystemRoot`のみ）を、同一作業フォルダ、引数`--codex-740-probe`、pipe stdin/stdout/stderr、非表示で比較した。元名は両環境条件でCreateProcess失敗、中立名は両条件で起動しexit code 2。元ファイルと2コピーのSHA-256は一致し、通常権限tokenであることも確認した。
- 比較と同時刻のUAC Operational Event ID 1が2件あり、両方とも`ERROR_ELEVATION_REQUIRED`。イベントは子実行パスを含まない。隔離コピーの正確なAppCompat `Layers`値はHKCU/HKLMとも見つからなかった。
- この環境ではhelperのbasenameが起動可否に結びつくことを再現した。Windows内部の判定規則は未確定であり、x64の標準Installer Detectionだけでは説明しにくい。

## コード上の境界

v0.32.0と現行mainの`Coordination::from_helper_exe`は、実行ファイル名ではなく親の`helper` directory、transaction/install ID、owner-only directory/ACLを検証する。したがって、実装候補はインストール済みファイルとsigned inventoryの`hane-updater.exe`を維持し、transaction用copyのbasenameだけを中立名へ変えること。

## 実装候補（head ec78b2a）

- Windowsのtransaction用staged helper copy名を中立化した。
- インストール済みhelper名（`hane-updater.exe`）とsigned inventoryは維持している。
- これは隔離比較で観測したbasename依存の起動差に対する候補であり、Windows内部の判定規則を確定した上での修正ではない。UAC/AppCompat側の未確認要因も残る。

参照: [v0.32.0 path validation](https://github.com/hide212131/hane/blob/v0.32.0/crates/update/src/platform/lease.rs#L161-L194)、[current main path validation](https://github.com/hide212131/hane/blob/ad4b80b155bf219a9c5686858dba3c87f14c3e77/crates/update/src/platform/lease.rs#L161-L194)。

## 次の工程

## 未確認事項

- 通常CIとexact-headでの検証結果は、この文書の時点では未確認。
- 実Windowsでの正式な更新準備・適用・再起動の受入は未実施で、成功とは扱わない。
- 診断プローブだけではfull handshakeや適用成功を確認していない。

昇格要求、UAC/AppCompat回避、設定変更は行わない。次の工程（検証・レビュー・実機受入）の判断はCommanderの再観測に委ねる。

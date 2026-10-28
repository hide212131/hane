# Windows update-prepare error 740: diagnostic checkpoint

関連Issue: [#510](https://github.com/hide212131/hane/issues/510)

状態: 原因未確定。診断専用の記録で、製品コードの変更や受入判定は含まない。

## 観測

- Windows x64上のHane 0.32.0で、候補v0.36.1の「更新準備」が2回ともOS error 740（`ERROR_ELEVATION_REQUIRED`）で失敗した。
- UAC Operational Event ID 1にも同じ時刻帯の子プロセス作成エラーが2件ある。イベントXMLに子プロセスの実行パス、PID、起動フラグは残っていない。
- インストール済みhelperはx64で、埋め込みmanifestと隣接manifestがない。対象パスについてHKCU/HKLMのAppCompat `Layers` 値は見つからなかった。失敗したコピーの正確な絶対パスは残っていない。
- Windowsの `EnableInstallerDetection` は有効。ただしMicrosoftの説明するInstaller Detectionの対象は32-bit実行ファイルなどであり、x64 helperのファイル名だけでは今回のエラーを説明できない。AppCompat database/shimなど他の判定要因は未確認。

## v0.32.0の起動経路

v0.32.0では、準備時にインストール済みの署名済みhelperをtransaction用private directoryへコピーし、そのコピーを `Command::new(exe).spawn()` で引数なし起動する。Windowsでは空の環境へ `SystemRoot` だけを設定し、pipe stdioと `CREATE_NO_WINDOW` を使う。候補ZIPに入っている新helperはこの段階では起動しない。

参照: [helper起動](https://github.com/hide212131/hane/blob/v0.32.0/crates/update/src/helper.rs)、[Windows helperコピー](https://github.com/hide212131/hane/blob/v0.32.0/crates/update/src/platform/mod.rs)、[Microsoft: UAC architecture](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/architecture)。

## 未確定点と次の観測

未確定なのは、どのWindows判定がhelperに昇格を要求したか。イベントログは親プロセスしか特定せず、失敗したコピーは清掃済みである。byte-identical helperを隔離した中立名のパスから同じ通常権限で起動し、ファイル名・実行条件を比較するプローブは、Windowsホストの実行審査で拒否されており、実行していない。別経路での回避はしていない。

原因を確定するには、承認された同条件の隔離比較、または対象child pathを含むプロセス作成イベントの新規採取が必要。診断完了までは原因を断定せず、管理者昇格やUAC/AppCompat回避を実施しない。

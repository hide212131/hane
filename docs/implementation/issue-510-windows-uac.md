# Issue #510: Windows更新helperの必要時UAC昇格

日付: 2026-10-31 JST
基準main: `29ed353d1548db7d2df61ffa85085dc4457c98cc`
状態: 利用者が採用した方針の実装準備。製品未実装・実機未検証。
要求の正本: [Issue #510](https://github.com/hide212131/hane/issues/510)
判断の正本: [ADR-0034](../adr/0034-zip-application-updates.md)

## 実装の進捗（第1段階）

- 追加済み: `crates/update/src/elevation.rs`（通常/UAC経路の判定、740とアクセス拒否の区別、通常利用者・install所有者・昇格先SIDの検査、OS由来peer照合、nonceの一回限り消費、取消の日本語表示）と、`platform/windows.rs` の `ShellExecuteExW` / `runas` ラッパー（実processハンドル保持）。いずれも注入可能なseam経由でテストする。
- 未接続（後続）: helper起動経路（`prepare_restart` / `spawn_helper_checked`）への組み込み、昇格helper向けのDACL付きローカルIPC、昇格後の独立再検証と管理者保護stage、一般権限の待機側からの新版起動、`asInvoker` manifest埋込み、UI案内。実機のUAC・GUI検証は未実施。

## 実装の進捗（第2段階）

- 接続済み: `spawn_helper_checked` の通常起動失敗を `elevation::normal_start_error` で分類し、740だけを `elevation_required` として区別して、OSエラー番号・stage・API・実パスを記録する。他のエラー（アクセス拒否を含む）は昇格要求にしない。アプリ側は740を日本語で案内し、旧Haneと作業内容を保ったまま更新を中止する。昇格はまだ実行しない。
- 未実装（安全条件を満たせないため意図的に止めている）:
  - `runas` 後のDACL付きnamed pipe、peer PID/token/session照合、nonce接続（昇格helperの引数契約と親PID検証の変更を伴う）。
  - 昇格helperが起動する新版・limited-health childは、現状 `Command` で起動するため管理者tokenを継承する。一般権限での再起動（非昇格token起動）が実装できるまで、昇格helperにapplyさせてはならない。
  - 保護配置の確認済み権限不足の判定、保護されたcoordination初期化と既存owner-only検査の役割分離。
- 上記は実機のUAC・GUI検証を含め未実施であり、完了扱いにしない。

## 目的と利用者に見える動作

本人のWindows x64の既知のローカルNTFS配置で、通常のHaneを一般権限で使い、手動更新の適用時だけ必要なhelperを正規UACで昇格する。取得・署名検証だけでは昇格しない。「再起動して更新」で保存状態を確認し、必要ならWindows確認画面が開くことを日本語で案内する。同意後に適用・一般権限再起動を行う。拒否・資格情報入力取消ではいまの文書と旧版を使い続け、後から再試行できる。書込可能かつ通常起動可能な配置ではUACを出さない。

## 既存作業・観測との関係

- #656（head `5e720f3b5b433e178fa0045c06b94198971b73d0`）はtransaction helperのbasenameだけの修正。#657（head `3c54af35255bf7238b2159c7c3cfc8032d3c99b6`）は隔離検証feedと資格情報store分離。両方とも本準備時点では未mergeで、mainから分岐している。独立したUACフェーズとして製品実装を始める。未受入の差分を重複実装・無断cherry-pickしない。先にmainへ入った場合は関係する契約を再照合する。
- #657のWindows実行チャットでは `C:\h657-20261031-run2\fixtures\install\Hane` の0.36.1が0.37.0を取得・検証・staging後、`stage=prepare`で740。helper子PID・新版再起動なし、旧版保持。APIとtransaction内helper絶対パスは未記録。保護配置への書込失敗や新版起動失敗と断定しない。記録はWindows側 `candidate-lifecycle-windows-run2.json` / `prepare-740-launch-attribution.json`。GitHubの合格証拠としては未受入。
- 既存経路は `prepare_restart → installed_helper → stage_helper_copy → spawn_helper_checked → Command::spawn`。更新開始側のinstalled helperをコピーして起動する。新ZIP内helperだけの変更では旧版からの最初の740を直せない。必要ならUAC対応bootstrap版を一度手動導入する。
- #656比較は同じcandidateに対し、更新を開始するbaseline側に修正あり/なしを置く。helperの同一bytesの名前比較で起動だけ成功しても、handshake・更新成功とはしない。
- #297/RF0全体を待たない。受入済み更新service・保存境界・journalを使い、UI初期化整理や外部変更検出を混ぜない。#657は隔離runtime検証の依存であり製品コード着手の一律前提にしない。

## Windowsの権限と配置の契約

1. 本体と通常helperは埋込みmanifestで `asInvoker` / `uiAccess=false` を明示する。管理者から起動された本体を黙って通常権限と扱わずtokenを検査する。取得・検証・通常編集を管理者で動かす設計にしない。
2. 書込可能な配置では既存一般権限経路を使う。保護配置で必要権限を得る場合、または検証済みhelperの通常起動で740となった場合に限り、明示操作に結び付く `ShellExecuteExW` / `runas` を使う。740を保護ディレクトリの証明にしない。アクセス拒否全般を無条件昇格retryにしない。
3. 対象はcurrent_exeから解決し、現在版署名inventory・OS file identity・volume・正規化root・所有主体・InstallIdに結び付ける。catalog、任意CLIパス、pipe入力が新しい任意install先を指定できない。既知の保護配置も所有者/ACLと現在版が検証可能なものだけを扱う。network、reparseによる別rootへの誘導、未知owner、改変されたinstallは拒否する。
4. 通常利用者SID、installの所有主体、昇格tokenのSIDを別の意味として保持する。同一管理者の同意だけでなく標準ユーザーが別管理者資格情報を入力する場合も検査する。InstallIdやstateを昇格先のprofile/current userから再導出しない。対応できない関係は変更前に理由を表示して拒否し、対応済みと記録しない。
5. 既存coordination/leaseの一意性を保つ。保護されたinstall親のcoordination初期化、同volumeのstage/backup、journal/receipts、復旧時の再昇格まで設計を接続する。書込不能fallbackやstate違いで別leaseを作らない。既存owner-only検査を単に削除せず、通常ユーザーと管理者の役割に合わせて権限を限定する。

## 昇格境界と適用手順

1. 一般権限側で取得と署名/hash/版/OS検査を完了し、再起動前の保存・IME・AI処理等の既存保護を通す。UAC前に文書を破棄したり本体を終了しない。
2. helper実体を既存の信頼する更新公開鍵と現在版inventoryから検証し、コピー後もhashとfile identityを照合する。検証後からロードまでの置換、親directory/reparse変更、DLL探索の混入を防ぐ。利用者が書換可能なexeを一度hashしただけで安全とせず、起動完了まで差替えを防ぐfile sharing/ACL/identityの保護を実装する。これを満たせない場合は昇格前に拒否する。昇格helper自身の事後自己検査を実行前保護の代用にしない。
3. 絶対exe pathと固定の狭い引数契約で `ShellExecuteExW` を使い、`SEE_MASK_NOCLOSEPROCESS` の実process handleを保持する。shell文字列、PATH探索、任意コマンド、資格情報の保存、UAC設定変更は行わない。UIのowner HWNDを結び付け、取消 `ERROR_CANCELLED` を通常の取消として識別する。OSエラーを740等の数値とstage/API/実パス付きで保存する（秘密は含めない）。
4. stdio継承をrunasへそのまま移さない。ローカル限定named pipe等に明示DACLを設定し、単一transaction用の十分な乱数nonce、InstallId、candidate/catalog hash、protocol、OS由来のpeer PID/token/session identityを相互照合する。nonceをログ/command lineへ露出せず、再利用・replay・別transaction・peer偽装を拒否する。別管理者SIDの場合もOS由来のidentityと権限を検証し、単なる自己申告PIDを信用しない。入力から管理者の任意file操作を生成しない。
5. 昇格後、helperは信頼する鍵で署名と全hash/版/OSを独立に再検証する。通常側の「検証済み」やjournalを権限の根拠にしない。入力を固定化し管理者側の保護stageへ移した後に再検証し、交換中の変更を防ぐ。対象rootのfile identity、旧版inventory、許可する所有file一覧、容量・ACLを再確認する。既存の隣接file、文書、設定、AIデータには権限を使わない。
6. helperからPreparedが確認できた後に本体を通常終了し、既存transaction/startup/runtime leaseとjournal、限定health、durable commit/rollbackを継続する。昇格helperはネットワーク取得、文書保存、AI/runtime起動、証明書信頼登録、Explorer kill、OS再起動予約をしない。
7. 一般権限の待機側を残して新版を起動する。昇格helperから直接起動して本体へ管理者tokenを継承させない。一般権限起動の実PID/tokenを確認し、既存限定healthの実体と結び付ける。healthのcommit前には永続データ変更・通常AI・autosaveを開始しない。取消・失敗・helper終了不明を成功にしない。
8. 取消では本体の作業状態と旧版を保ち、検証済みcandidateを再検査して再試行可能にする。交換開始後の失敗では検証済みbackupを復元し、復旧にもUACが必要で拒否された場合はRecoveryRequiredとして明示的な手動復旧案内を出す。第三者の変更を強制上書きしない。昇格process終了を確認してからstageやnonceを清掃する。

既存Ed25519更新署名は必須。Windows AuthenticodeのUAC発行元表示とは別であり、署名済みinventoryだけで「確認済み発行元」と表示しない。有料Authenticode登録を本フェーズの新たな前提にしない。

## 変更箇所と検証

主対象は `crates/update/src/{helper,identity,state_root,startup}.rs`、`platform/{windows,lease,mod}.rs`、journal/recovery、`crates/updater`、`crates/app/src/{app_update,main}.rs`、必要なWindows manifest/build設定と設定カード。既存関数・所有権を開始時に確認し、必要な範囲だけ変更する。macOSは既存の一般権限経路を保つ。保護された`.github/`やrelease設定の変更が必要なら製品workerで触らず具体的な別作業として報告する。

回帰テストは実UACやproduction storeを操作しない注入可能なOS境界で、通常経路/UAC要否、740と書込拒否の区別、取消・起動失敗・timeout時の本体保持、署名/hash/OS/root/owner/file identity不一致、差替え/reparse、偽peer/nonce/replay、別SID、復旧と一般権限再起動を確認する。既存署名・ZIP・保存保護・macOSテストも維持する。

実機受入はWindows側ChatGPTから直接、最新mainのcanonical GUI lock launcher、短い隔離root、専用state/workspace/資格情報store、使い捨て鍵・feedで行う。実行SHA、x64 PE、binary/ZIP hash、実PID/path/token、ACL、API/OS error、journal、署名と版を記録する。承認成功→再起動→編集保存、取消→旧版で編集継続→再試行、書込可能配置でUACなし、既知の保護配置、代表的な適用失敗→復旧を確認する。別管理者資格情報の経路は対応可否を実測して記録する。secure desktopを操作できなければ本人の正規操作を依頼し保護設定は変えない。production install、通常文書、実AI秘密、本番署名鍵を使わない。全専用process終了、feed停止、lock解放を確認する。隔離feed合格とproduction配布物の検証は区別し、未実施をpassにしない。

## 対象外と完了の扱い

常時管理者本体、常駐サービス、汎用installer、MSI/MSIX全体配布、macOS昇格、UAC/AppCompat回避、保護無効化、自動取得/適用、#649通知、release/mergeは対象外。新規機能実装の依頼であり、740の内部原因を確定した修正と称しない。実装・CI・レビュー・必須GUI受入がそろうまで製品完了/Issue closeとしない。

## 一次資料

- [Microsoft UAC architecture](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/architecture): CreateProcessの740、32-bitに限定されるInstaller Detection。
- [ShellExecute / runas](https://learn.microsoft.com/en-us/windows/win32/shell/launch)、[ShellExecuteExW](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shellexecuteexw): 同意/資格情報、取消。
- [Application manifests](https://learn.microsoft.com/en-us/windows/win32/sbscs/application-manifests): asInvokerは起動元tokenを継承。
- [Named pipe security](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights): DACLを明示し最小アクセスに限定。

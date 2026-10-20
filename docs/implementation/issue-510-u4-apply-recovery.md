# Issue #510 U4: OS適用と復旧

対象Issue: [#510](https://github.com/hide212131/hane/issues/510)
採用済み設計: `docs/adr/0034-zip-application-updates.md`、`docs/application-update-implementation-spec.md`、`docs/application-update-implementation-handoff.md`
前段の実装: [#512 U0/U1/U3](https://github.com/hide212131/hane/pull/512)
準備時の基点: `f9da614d93158fba95b5939ffdb44c67f06b8657`（#512 head）
状態: 実装中。increment 1（process lifetime lease、配置確認、helper private protocol、起動gate）、increment 2（Windows x64 所有ファイル適用のhelper接続）、increment 3（限定health起動とdurable commit/rollback）、increment 4（macOS bundle swapとjournal/recovery）、increment 5（再起動ticket・Settingsの再起動操作・commit後の一回復元）をコードに追加済み。実macOSでの二版適用は未実施（Apple Developer ID・notarizationは要求しない）。`TrustPolicy::production()` は未設定のため、production buildでは「再起動して更新」は表示も実行もされない（fail-closed）。

### Increment 5: 再起動ticket、再起動操作、commit後の復元（`state_root.rs`、`app/src/update_restart.rs`、`ui/src/view/app_update.rs`）

- **ticket発行**（`state_root::issue_ticket`）: TransactionId / InstallId / 束縛済みstate rootと一致するticketを組み立て、schema・件数・path・サイズを検証してから、最後に `still_permitted` を呼び（候補がReadyのままか、配置が同じinstallか、restart permitが新鮮か）、通ればatomic write + fsyncする。拒否・検証失敗ではticketを残さない。内容は保存済みfile path、work folder、active、有界なscroll/caret値だけで、本文・token・command/environmentは持てない（`deny_unknown_fields`）。scroll/caretは現在 `0` を記録し、復元でも適用しない（位置の復元は未実装）。
- **commit後の一回処理**（`TicketRestore::take`）: 当該transactionのjournalがdurableに`Committed`のときだけticketを `restart-ticket.claimed` へrenameして取得する。取得後に読むため、破損ticketや途中crashでも同じticketは二度適用されない。claimed fileの削除失敗は `Cleanup::CleanupPending` で、数回再試行し、残っても再適用はされない。取得は、限定childが `Commit`（またはdurable Committed）を確認した後の `limited_start` で行う。childがdurable Committed後・ticket claim前に終了した場合は、次の通常起動で、受理済みCommitted receiptに対応する未claimのticketを `startup::committed_restore`（`RuntimeGuard::take_committed_restore`）と `crates/app/src/main.rs` の通常起動経路から引き継ぐfallbackもある。このfallbackも同じ `TicketRestore::take` を通るため、二度適用されない。
- **復元**（`update_restart::plan_restore`）: 存在しない保存済みfileは飛ばして件数をstatusに表示し、残りを既存の通常open経路（`open_path` / `open_work_folder_entry`）で順に開く。activeは最後に開く。work folderが無ければ通常起動の既定に戻る。通常起動のdefault folder設定は書き換えない。
- **Settingsの操作**（`EditorView::start_update_restart`）: ReadyかつサービスがrestartをサポートするときだけCardに「再起動して更新」を出す。全blocker再確認（`prepare_update_restart`）→ helper準備（background、`Prepared`を受けるまで終了しない）→ freeze → `run_exit_sequence`: permit確認 → ticket発行 → draft flush(Result付き) → AIの明示停止(10秒) → permit再確認 → `ExitAuthorized` → 通常quit。どこで失敗してもhelperをAbort（ticket削除）、freezeを解除、停止したAI serviceを再起動し、文書・設定・draft・外部openは失わない。freeze中の外部openは既存の `Busy` で送信元が保持する。
- **サポート条件**: `AppUpdateSnapshot::restart_supported` は、trust policyが設定済み・target有り・サポート配置の `RestartWiring` が有効のときだけtrue。production buildはtrust未設定のため常にfalse。
- **更新完了の表示**: durable Commit確認後の限定child由来のプロセスだけが、General settingsのCardに一度だけ「Hane {version} に更新しました。」を表示する。rollback/recoveryや通常起動では表示しない。
- **検証範囲**: fake session / 一時directory / fake `ExitSteps` による単体試験のみ。実Windows・実macOSでの二版適用、実際のGUI操作、U-08/U-09/U-14の実OS証拠は未実施であり、fixture成功を実OS成功とはみなさない。

### Increment 4: macOS bundle適用と復旧（`crates/update/src/macos_apply.rs`、`platform/macos.rs`）

- OS固有の操作は `BundleHost` trait（volume照会、swap対応のprobe、`RENAME_SWAP`相当の交換、更新用Ed25519鍵で署名したinventoryによるbundle検証）に閉じる。実装 `platform::macos::MacHost` は macOS ビルドだけに存在し、他OSでは `PlatformBackend` が `NotApplicable` で拒否する。journal / 復旧は `Placement::target` で選ぶ `DiskView`（macOSは `BundleView`）を使い、Windows の所有ファイル用 view と混ざらない。
- helperは親終了後（排他runtime lease保持後）に、現在の `Hane.app` と候補の両方について、固定Bundle ID、Info.plistの版・source SHA（catalogの `source_sha`）・updater protocol、bundle内の署名済みinventory（`Contents/Resources/hane-install.json`と`.sig`、更新用Ed25519鍵）とbundle内の全ファイルの一致を検査する。候補のinventoryはarchive検査でもcatalogの`inventory_sha256`に結び付ける。更新鍵未設定は `TrustUnconfigured`、署名不正は `SignatureInvalid`、ファイルの不足・追加・改変は `InstallModified`で、mutation前に停止する。Apple Developer ID・Team ID・notarizationは要求しない。ad-hoc再署名、quarantine除去、Gatekeeper迂回、Translocation回避はしない。
- 候補はアプリが `transaction_dir/candidate/` へcopyしたcatalog/署名/archiveをhelperが再検証して `transaction_dir/stage/` へ展開したものだけを使う。`transaction_dir` はinstallと同じ親（coordination directory）の下にあるため、候補はもともと同じvolumeのprivate領域にある。それでもinstallとstaged candidateのvolume idが同一で、両方がローカルの書込可能なAPFSであることを確認し、さらにinstallの親とstageの親にそれぞれ作った一時directoryを実際に交換して swap-renaming 対応を確かめる。条件を満たさなければ journal 作成前に `UnsupportedInstall` で拒否する。
- bundle identity は、まず `Info.plist`、`Contents/MacOS/hane`、`Contents/Helpers/hane-updater`、`Contents/_CodeSignature/CodeResources` をbundleの必須項目として検査し、その後 `collect_identity_entries` でbundle内のすべてのdirectory、file内容、symlink targetを再帰的にidentityへ含めたもの。`CodeResources` だけには依存せず、bundle内のどの資源の変更もidentityを変える。journalは `Hane.app` という一つのstepを持ち、旧identity、新identity、backup（`backup/Hane.app` = swap後に旧bundleが置かれるstaged slot）を記録する。
- 手順は ExitAuthorized → Applying → step Intent（durable）→ 旧/新identityを再観測 → 一回の交換 → 交換後の両identityを観測 → step Applied（durable）→ AwaitingHealth。先に既存bundleを消す二段階renameは使わない。
- health起動は既存の `ProcessLauncher` / `conclude_with_child` を使い、正規配置の `Contents/MacOS/hane` を限定childとして起動する。起動前に生きたbundleがjournalの新identityと一致し、署名済みinventoryで再検証できることを確認する。`Committed` がdurableになるまで `Commit` は送らず、成功の戻りは `settled_result`（Windowsと共通）を通る。
- health失敗・timeout・crash・不一致では、childの終了とlease解放を確認してから、同じ交換（`BundleOps::restore`）で旧bundleへ戻す。復旧は journal だけでなく実diskのlive/slot identityで方向を決める（live=旧なら交換しない、live=新かつslot=旧なら一回交換、それ以外は未知として `RecoveryRequired`）。この判定は既存の `apply::recover` / `decide_recovery` をそのまま使うため、各中断点（intent前、交換後・Applied前、AwaitingHealth、RollingBack途中、rollback交換後・journal更新前）で二重に交換しない。未知identity、損傷したbundle、消えたbackup、intentなしの交換は何も上書きせず `RecoveryRequired` にする。
- 検証範囲: fake `BundleHost` と一時directoryによる決定的なunit testのみ（成功commitで新bundleが残る、health失敗で旧bundleへ戻る、intentがswap前にdurable、各境界のrecovery、unknown identityで交換しない、非APFS・非ローカル・読取専用・swap非対応・別volume・署名不一致・更新鍵未設定・ZIP改変の拒否、Apple identityなしでの適用成功、署名済みinventoryの検証）。`MacHost` の `renamex_np` は macOS CI の一時volume上で「交換するか何も変えない」ことを確認するtestだけを持つ。**実macOS上の実二版適用・電源断耐性は未検証**で、U2/U5のevidenceとして別に行う。
- 本番信頼鍵は埋め込んでいない（Team IDは不要）。`TrustPolicy::production()` は引き続き未設定で、production buildはmacOSでも適用不能のまま。U2のrelease workflow/secrets、再起動UI、state-root/ticket復元、実二版配布証拠はこの増分に含めない。

### Increment 3: 限定health起動とdurable commit（`crates/update/src/health.rs`）

- helperは所有ファイル置換後に排他runtime leaseだけを解放し、`transaction`と`startup`はcommit/rollback完了まで保持する。新版`hane.exe`は現在の署名済みinventoryが所有するfileとして再検証（hash・版）してから、引数なし・空環境・pipe stdioのみで起動する。
- 限定起動の入口は、所有者限定のtransaction directoryにhelperが置く一回限りのcapability file（nonce、helper pid、InstallId、TransactionId、候補ID、版）だけである。公開CLI flag、任意path/command、一般環境変数では入れない。childはOS上のparent pidがcapabilityのhelper pidと一致し、helperが`transaction` leaseを保持し、実行中の版がcapabilityの版と一致する場合だけcapabilityをrenameで一度だけ取得し、共有runtime leaseを取る。一致しない通常起動は従来どおりstartup gateを通る（gateを迂回できない）。
- childは通常editor・AI・autosave・migration・open requestを開始せず（`update_gate::enter`内で待機）、read-onlyの確認後に`Healthy`（InstallId、TransactionId、候補ID、版、自身のpid）をprivate pipeへ送り、`Commit`を待つ。
- helperは45秒以内に、そのtransactionのspawnした実child（nonce、全識別子、OSが返したpid）からの`Healthy`だけを受理する。受理後に`Committed`をdurableに書き、成功した場合だけ`Commit`を送る。書込失敗ならchildへ通知せずrollbackへ進む。helperが`Commit`送信前に死んだ場合、childは同じtransactionの`Committed`がjournalにdurableに残っているときだけ続行する。
- 起動失敗、timeout、crash、wrong/stale identity、malformed/cut IPC、health拒否ではchildのpipeを閉じ、猶予後にhelper自身のchild handleで終了させ、OSの終了確認と排他runtime leaseの再取得（解放確認）を待ってから旧fileへrollbackする。終了またはlease解放を確認できなければrollbackを始めず`RecoveryRequired`にし、更新成功として返さない。
- この増分時点ではmacOSは`PlatformBackend::apply`が`NotApplicable`で拒否していた。increment 4でbundle swapに接続した（上記）。
- state rootの引渡しとread-only検査（`crates/update/src/state_root.rs`）: 更新を開始する親の実際の`FileStateStore::root()`（既定rootまたは`HANE_STATE_DIR`）を、`prepare_restart`がhelper起動前にtransaction専用directoryの`state-root.binding`へ束縛する（絶対・正規化・実directory・owner/権限を検証）。helperはそれを読んでchildの一回限りhealth capabilityに入れ、childは空環境のままcapability経由でだけrootを受け取る。flag・環境変数・任意path/commandの入口は追加しない。bindingが無い・不正ならchildは起動されずrollbackへ進む。childは`Healthy`の前に`StateCheck::verify`（root再検証、binding一致、設定storeのread-only open、`restart-ticket.json`のschema・サイズ・許可field・InstallId/TransactionId/state root一致）を実行し、一つでも失敗すれば`Healthy`を送らない。`Commit`後だけ`pin_state_root`で通常起動がそのrootを使う。ticketを発行する呼び出し元はまだ無いため、この増分ではHealthyは常にfail-closedで、再起動UIは有効化しない。ticketの作成・文書復元とSettingsのrestart/shutdown接続は次増分。実Windowsでの二版適用証拠（U5）、macOSは対象外。

### Increment 2: Windows所有ファイル適用（`crates/update/src/windows_apply.rs`）

- アプリは `prepare_restart` で、Ready候補のcatalog・署名・archiveだけを `Coordination::transaction_dir/<TransactionId>/candidate/` へcopyする。helperのprotocolには候補ID以外を追加していない（任意のsource/destination path、CLI引数、環境変数、commandなし）。
- helperは親終了後に排他的runtime leaseを取ってから、自分の位置から導出した `Coordination` と `find_install` で配置を決め、現在のsigned inventory、候補catalog/署名（候補IDと照合）、archive hash、archive検査と展開（`stage/`）、新inventoryを独立して再検証する。適用planは署名確認済みinventory同士から `ApplyPlan::for_inventories` で作り、precondition違反は書込み前に停止する。
- journal intentをdurable化してから既存の `apply::apply` で所有ファイルだけを置換し、`hane.exe` は最後になる。inventory外のfile、user state、Explorer登録、版別shell DLLは変更しない。曖昧なhashや壊れたbackupは既存engineがRecoveryRequiredにする。
- increment 2 時点では限定healthとdurable commitがなく、`apply_and_settle` は置換後に直ちに旧版へ戻していた。increment 3 で限定health childとdurable commitに接続した（下記）。再起動ボタンへは接続していない。
- 未実施: macOS bundle swap、restart UI有効化、実Windowsでの二版適用証拠（U5）。ここまでの検証はfake/temp directoryによるunit testのみで、実OSのpower-loss耐性は主張しない。

## 目的

#512で準備できる更新候補を、利用者の明示操作後にだけ現在のHane本体へ適用する。親processが通常終了したことをOSのprocess lifetimeで確認し、current helperが候補と配置を再検証する。新版の限定healthとdurable commitが完了するまで通常編集を開始せず、どの失敗点からも旧版へ戻すか安全に停止する。

## 今回の範囲

- `crates/update/src/platform/{windows,macos}.rs` と既存journal/recovery境界に、OS別の配置identity確認、lease、apply、rollbackを追加する。
- `crates/updater/` に現在版から検証済みcopyを作り、private IPC/nonceとprocess handleを使うhelperを追加する。公開CLIや任意path置換toolは作らない。
- `crates/app/` の起動最前段と更新serviceをつなぎ、pending journalがある場合は通常の文書・AI・設定初期化より先に復旧を判定する。
- Windowsではinventoryが列挙する所有ファイルだけをjournal付きで置換する。実行中のexeは最後に扱い、未知ファイル、user data、marker、旧version shell DLLは更新対象に含めない。
- macOSでは署名済みinventory・Bundle ID・配置・APFS swap-renaming対応を確認し、対応する場合だけ同一volume上でbundleを原子的に交換する。非対応配置では手動案内にする。
- 新版を通常起動せずに行う限定health handshake、durable commit、限定ticket復元、失敗時rollback、回復不能時の明示状態を実装する。
- 直接関連するfake/process seamによる故障点テストを実装する。新しい再起動操作は全U4境界がつながり、成功表示をdurable commit後に限定できる状態まで有効化しない。

## 受入条件

1. Apply直前に候補署名/hash/版/target、install identity、owner、権限、容量、配置、現在版inventoryを再確認し、親processが生存中なら一切mutationしない。
2. helperのprivate IPCは実parent/child identityと一回限りのnonceで認証する。古いticket、偽装要求、切断、stale operationは適用を進めない。
3. journalは各mutation前にintentをdurable化する。再起動後は実hash/file identityとjournalを照合し、途中適用は旧一覧へrollbackする。未知hash、壊れたbackup、曖昧なbundle swapでは追加上書きをせずRecoveryRequiredにする。
4. Windowsは署名済みinventory所有fileだけをatomic replacementで適用し、未知の隣接Markdown、user state、Explorer marker、ロード中の旧shell DLLを変更しない。hane.exeは最後に置換する。
5. macOSは更新用鍵で署名したinventory/Bundle ID/versionを確認し（Developer ID・Team ID・notarizationは不要）、APFSのswap-renaming capabilityと対応配置がある場合だけ交換する。二段階rename、quarantine除去、ad-hoc再署名、Translocation回避は行わない。
6. 限定起動の実childから45秒内のHealthyを受け、helperがCommittedをflushするまでは通常編集・AI・autosave・外部openを開始しない。成功表示はdurable commit後だけにする。
7. health失敗時はchild停止とruntime lease解放後に旧版へ戻す。安全な停止や復旧ができなければ成功を示さずRecoveryRequiredにする。
8. restart ticketは保存済みfile pathや有界な表示位置など必要最小限だけを持ち、本文・token・任意command/environmentは持たない。復元はtransactionが一度だけ処理し、cleanup失敗はcleanup_pendingとして再試行できる。
9. 直接関連する決定的故障注入で各journal flush/replacement/swap/health/commit境界を検証し、current-head exact verifierとWindows/macOS CIを通す。実OS・実署名候補の証明はU2/U5 evidenceとして別途行う。

## 対象外

- 配布workflow、署名key、release publication、ZIP layout生成（U2）。
- bootstrap版の発行、両OSの実二版download-to-commit GUI証拠とIssue #510全体の完了（U5）。
- 管理者権限による昇格、強制終了、自動更新/起動時通信、delta更新、任意file/command実行。
- #500/#507のPR branch。freeze中の共通open入口は#512で導入済みの境界を使い、両PRの変更を複製しない。

この文書はClaude実装用PRの準備差分だけであり、製品実装、検証、Issue #510の受入完了を意味しない。

# Issue #510 U4: OS適用と復旧

対象Issue: [#510](https://github.com/hide212131/hane/issues/510)
採用済み設計: `docs/adr/0034-zip-application-updates.md`、`docs/application-update-implementation-spec.md`、`docs/application-update-implementation-handoff.md`
前段の実装: [#512 U0/U1/U3](https://github.com/hide212131/hane/pull/512)
準備時の基点: `f9da614d93158fba95b5939ffdb44c67f06b8657`（#512 head）
状態: 実装中。increment 1（process lifetime lease、配置確認、helper private protocol、起動gate）、increment 2（Windows x64 所有ファイル適用のhelper接続）、increment 3（限定health起動とdurable commit/rollback）をコードに追加済み。macOS bundle swap、実OSでの二版適用は未実施で、「再起動して更新」は有効化していない。

### Increment 3: 限定health起動とdurable commit（`crates/update/src/health.rs`）

- helperは所有ファイル置換後に排他runtime leaseだけを解放し、`transaction`と`startup`はcommit/rollback完了まで保持する。新版`hane.exe`は現在の署名済みinventoryが所有するfileとして再検証（hash・版）してから、引数なし・空環境・pipe stdioのみで起動する。
- 限定起動の入口は、所有者限定のtransaction directoryにhelperが置く一回限りのcapability file（nonce、helper pid、InstallId、TransactionId、候補ID、版）だけである。公開CLI flag、任意path/command、一般環境変数では入れない。childはOS上のparent pidがcapabilityのhelper pidと一致し、helperが`transaction` leaseを保持し、実行中の版がcapabilityの版と一致する場合だけcapabilityをrenameで一度だけ取得し、共有runtime leaseを取る。一致しない通常起動は従来どおりstartup gateを通る（gateを迂回できない）。
- childは通常editor・AI・autosave・migration・open requestを開始せず（`update_gate::enter`内で待機）、read-onlyの確認後に`Healthy`（InstallId、TransactionId、候補ID、版、自身のpid）をprivate pipeへ送り、`Commit`を待つ。
- helperは45秒以内に、そのtransactionのspawnした実child（nonce、全識別子、OSが返したpid）からの`Healthy`だけを受理する。受理後に`Committed`をdurableに書き、成功した場合だけ`Commit`を送る。書込失敗ならchildへ通知せずrollbackへ進む。helperが`Commit`送信前に死んだ場合、childは同じtransactionの`Committed`がjournalにdurableに残っているときだけ続行する。
- 起動失敗、timeout、crash、wrong/stale identity、malformed/cut IPC、health拒否ではchildのpipeを閉じ、猶予後にhelper自身のchild handleで終了させ、OSの終了確認と排他runtime leaseの再取得（解放確認）を待ってから旧fileへrollbackする。終了またはlease解放を確認できなければrollbackを始めず`RecoveryRequired`にし、更新成功として返さない。
- macOSは`PlatformBackend::apply`が引き続き`NotApplicable`で拒否し、新しい成功経路へ接続されない。
- 未実施・既知の制約: 成功後に続行するchildは空環境で起動されるため、`HANE_STATE_DIR`等の検証済みstate rootの明示的な引き渡しは別増分で扱う（再起動UIは未接続）。read-onlyの設定/ticket検査は`ChildSession::run`のcheck closureに接続する余地を残しており、現時点では検査内容を持たない。実Windowsでの二版適用証拠（U5）、設定画面の再起動UI、macOSは対象外。

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
- macOSでは署名・Team ID・Bundle ID・配置・APFS swap-renaming対応を確認し、対応する場合だけ同一volume上でbundleを原子的に交換する。非対応配置では手動案内にする。
- 新版を通常起動せずに行う限定health handshake、durable commit、限定ticket復元、失敗時rollback、回復不能時の明示状態を実装する。
- 直接関連するfake/process seamによる故障点テストを実装する。新しい再起動操作は全U4境界がつながり、成功表示をdurable commit後に限定できる状態まで有効化しない。

## 受入条件

1. Apply直前に候補署名/hash/版/target、install identity、owner、権限、容量、配置、現在版inventoryを再確認し、親processが生存中なら一切mutationしない。
2. helperのprivate IPCは実parent/child identityと一回限りのnonceで認証する。古いticket、偽装要求、切断、stale operationは適用を進めない。
3. journalは各mutation前にintentをdurable化する。再起動後は実hash/file identityとjournalを照合し、途中適用は旧一覧へrollbackする。未知hash、壊れたbackup、曖昧なbundle swapでは追加上書きをせずRecoveryRequiredにする。
4. Windowsは署名済みinventory所有fileだけをatomic replacementで適用し、未知の隣接Markdown、user state、Explorer marker、ロード中の旧shell DLLを変更しない。hane.exeは最後に置換する。
5. macOSはvalid Developer ID/Team ID/Bundle ID/versionとnotarizationを確認し、APFSのswap-renaming capabilityと対応配置がある場合だけ交換する。二段階rename、quarantine除去、ad-hoc再署名、Translocation回避は行わない。
6. 限定起動の実childから45秒内のHealthyを受け、helperがCommittedをflushするまでは通常編集・AI・autosave・外部openを開始しない。成功表示はdurable commit後だけにする。
7. health失敗時はchild停止とruntime lease解放後に旧版へ戻す。安全な停止や復旧ができなければ成功を示さずRecoveryRequiredにする。
8. restart ticketは保存済みfile pathや有界な表示位置など必要最小限だけを持ち、本文・token・任意command/environmentは持たない。復元はtransactionが一度だけ処理し、cleanup失敗はcleanup_pendingとして再試行できる。
9. 直接関連する決定的故障注入で各journal flush/replacement/swap/health/commit境界を検証し、current-head exact verifierとWindows/macOS CIを通す。実OS・実署名候補の証明はU2/U5 evidenceとして別途行う。

## 対象外

- 配布workflow、署名key/Team ID/notary credentials、release publication、ZIP layout生成（U2）。
- bootstrap版の発行、両OSの実二版download-to-commit GUI証拠とIssue #510全体の完了（U5）。
- 管理者権限による昇格、強制終了、自動更新/起動時通信、delta更新、任意file/command実行。
- #500/#507のPR branch。freeze中の共通open入口は#512で導入済みの境界を使い、両PRの変更を複製しない。

この文書はClaude実装用PRの準備差分だけであり、製品実装、検証、Issue #510の受入完了を意味しない。

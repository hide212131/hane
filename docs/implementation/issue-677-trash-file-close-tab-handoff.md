# Issue #677: ファイルのゴミ箱移動成功後にタブを閉じる実装引き継ぎ

## 状態と正本

- 要求・範囲・受入条件の正本: [Issue #677](https://github.com/hide212131/hane/issues/677)。本書は具体的な接続・処理順・検証対応を補う。
- 基準main: `1e0d345e46d8c1805659968f8fad1148b0ae37a8`（2026-11-04 JST調査）。branch: `codex/issue-677-trash-file-close-tab`。
- **Claude実装待ちの準備PR**。本commitは文書だけ。製品コード・テストの変更、実機での再現・GUI・製品CI検証は未実施。
- PR作成以降は別のPR担当チャットへ渡す。検討担当はClaudeへの依頼を投稿しない。先行依存の受入後、担当がcurrent factsとexact headを取り直し、一度だけ依頼してprepare成功を確認する。
- ADR-0019のsession／File I/O／UI境界内の局所拡張であり、新ADRは不要。永続形式や外部削除契約を変更する必要が生じたら範囲を再判断する。

## 利用者報告と確認済み事実

原文は「ファイルを削除したら、タブからも消えるべき」。追加回答により、Haneのサイドパネルからゴミ箱へ移動したファイルを想定していることを確認した。報告OS・版・未保存編集の有無は未確認。

基準mainのコードでは、ゴミ箱移動後に一覧は更新されるが対応sessionはMissingとして残る。これは実画面で再現済みという主張ではない。外部削除、フォルダ行の削除まで利用者の要望として一般化しない。

## 接続先と既存契約

| パス・シンボル | 基準mainの事実／接続方針 |
|---|---|
| `crates/ui/src/view/sidebar.rs::sidebar_context_trash` | ファイル行からの唯一の変更入口。dirty／保存中は現状拒否。背景OS操作の成功後にDeleted、tree／検索／pin更新。ここで操作固有の成功と対象を照合してcloseを接続する |
| 同 `move_path_to_trash_with` | 存在→OS操作→NotFound確認。事前消失、取消、OSエラー、元パス残存、検証I/Oエラーを成功にしない。永久削除fallbackを追加しない |
| 同 `sidebar_context_trash_folder` | 配下のdirty／保存中・pending open/createを保護し、フォルダ操作中の書込み等を拒否済み。今回の仕様変更対象外。保護やdraft移動先の再設定を壊さない |
| `crates/session/src/session.rs::DocumentSession::apply_file_event`, `SessionSet::apply_file_event` | DeletedはMissingとして内容保持。この汎用イベントを自動closeの意味に変えない |
| 同 `SessionSet::close` | 通常dirty guardと最後のタブの空無題置換を維持。#672で受入済みとなる対象限定の明示破棄経路を再利用する |
| `crates/ui/src/view.rs::request_tab_close`, `TabCloseConfirm`, `DocumentInstance`, `close_tab_now` | #672／PR #675の共通確認・close後処理と重なる。sessionだけ消してparse／検索／save problem／active／メニュー／watch寿命の後処理を取りこぼさない |
| `crates/ui/src/view/session_save.rs` | autosave、Save As、H1命名・rename、draft timer／flush。削除中の同対象I/Oを調停し、古い結果・timerが元pathを再作成しない |
| `crates/session/src/draft.rs::DraftStore::remove` | per-draft lockとsequenceによる遅延snapshot拒否を再利用。削除成功・承認対象だけ除外する。OS操作失敗前に復旧情報を消さない |
| `crates/ui/src/view/external_change.rs`, `document_watch.rs` | 通知はhint。Missing／アクセス不能でも本文を保持。今回ここへ自動closeを追加しない。watchのclose後解除は既存経路へ整合させる |
| `crates/ui/src/view/app_update.rs::restart_ticket_snapshot`, `view.rs::restore_files` | 更新再起動ticketは現存sessionの保存済pathを列挙し、restore_filesは順に再openする。成功closeした対象がsnapshotへ残らないこと、pending restore/readが後から復活させないことを確認する |
| `session_save.rs::schedule_draft_save`, `flush_pending_drafts`, 作業フォルダのdraft復旧 | 通常の作業フォルダdraft復旧と更新再起動の保存済ファイル復元は別経路。一般的な全タブ永続化が存在すると仮定しない。他draftの復旧を維持する |

## 処理順と不変条件

1. 右クリック行の対象path、workspace、対応sessionとDocumentInstanceを確定する。表示名や現在のactiveだけで対象を決めない。未オープンの場合も後続pending openが同pathの古い内容を持ち込まないよう調停する。
2. 保存中／IME／rename／Save As等の競合を安全に待つか拒否する。dirtyなら対象名と未保存変更が失われることを示して「未保存変更を破棄してゴミ箱に移動」／取消を提示する。保存を勝手に開始しない。
3. 承認は削除操作に結び付ける。#672の『閉じるだけ』の承認を削除権限として流用せず、そこで受入済みのsnapshot検証・対象限定破棄・後処理の部品を使う。**先にタブを閉じたりdirtyを偽ってcleanにしたりしない。**
4. 実行直前に承認対象・revision等を再照合し、背景OS操作を開始する。対象のautosave／自動命名／rename／Save As／openとの順序を既存job寿命で調停し、実行中に削除対象が再作成されないようにする。汎用の新workerやworkflowは追加しない。
5. OS取消／失敗／検証失敗ではタブ・本文・Undo/Redo・draftを保持し、結果を表示する。OS移動済みだが確認失敗の場合、disk rollbackができると約束せず保持内容からの復旧を提示する。
6. 成功時にworkspace、操作対象session、instance、path、必要なrevisionとI/O contextを再照合する。追加編集、文書差替え、同名再作成、状態不明なら古い承認でcloseせずMissing等で保持・可視通知する。path文字列だけの遅延closeを禁止する。
7. 安全に確定できた対象だけ共通close／明示破棄で閉じる。対象draftの廃棄が必要なら#672の成功確認と遅延write拒否を用い、失敗時は内容保持・可視エラー。対象外タブ・draft・履歴を変更しない。tree／検索／pinの既存成功処理も維持する。
8. 非アクティブ対象のcloseはactiveを保持。最後のタブは新ID空無題に置換。監視・保存待ち・メニュー・確認・描画の後処理を既存経路で整合させる。終了flush、draft復旧、restart snapshot／pending readで閉じた対象が復活しない。

上記はAPI名の新設を強制しない。実装者は#672の受入後のcurrent mainに照らし、既存境界に必要な最小変更で接続する。実行中のディスクI/Oを単に結果無視しただけで安全に取り消せるとは仮定しない。追加編集を保護するための保持例外を『通常の削除成功でも常にタブを残す』動作に戻さない。

## 局所依存と並行開発

**製品実装は #672／PR #675 の明示破棄・確認snapshot・draft寿命・共通closeの受入とmain反映を先行条件とする。** 調査時の準備headは `4b81889abad98a0aba244f266a2e25d17fe645a6`。これは製品実装済みの証拠ではない。担当チャットは開始時にPRの状態と採用commitを確認し、本branchへ必要なbase統合を行うかCommander Policyに従って判断する。別実装のdirty bypassを追加しない。

#308／#309／#368は共有保存・命名・draft・job寿命の確認対象であり、全体完了を一律の待ち条件にしない。採用済み#554／PR #555、#585／PR #586、#653／PR #655の経路を使う。フォルダ削除・外部Missing・キーボード削除追加は別責務として本件へ混ぜない。

## 検証対応

| シナリオ | 必要な観測 |
|---|---|
| clean active／inactive／未オープン、最後のタブ | disk→ゴミ箱移動成功、一覧と対象tab消失、他tab保持、最後は空無題 |
| dirtyの承認／取消／Escape／確認後の編集 | 承認前と取消は内容・Undo・draft保持。承認済みrevisionだけ成功後に破棄。追加編集は保持と通知 |
| OS取消／権限拒否／事前消失／元パス残存／確認I/O失敗 | 成功表示・closeなし、本文と復旧手段維持。失敗注入はunit／GPUIで決定的に行う |
| 保存／autosave／H1命名／rename／Save As／IME／openと競合、連打 | 二重移動や元path再作成なし。未完了save結果を失って編集を捨てない |
| workspace切替、同名再作成、session差替え、遅延完了 | 古い結果で別tabを閉じない。file identityと文書世代の保護 |
| 対象外タブ・draft、非選択対象 | 本文・Undo/Redo・選択・scroll・active・復旧情報維持 |
| draft遅延write／終了flush／フォルダ再読込／更新restart | 成功closeだけ復活せず、取消／失敗と他draftは復旧可能 |
| 外部削除／短時間消失／atomic置換／アクセス不能／folder trash | 新しい自動closeなし、既存のMissing／外部変更保護・folder安全条件維持 |

session／filesystem失敗注入と実イベント経由のfocused GPUIを実装者が追加する。実装前に既存の関連テストと代表的な削除処理の応答・I/O構成を確認し、必要なら同条件で変更前後の測定を残す。新しい定期全走査を追加しない。

製品テスト・build・lintはcurrent CIと実行計画の該当要件に従う。macOS／Windowsのfocused native GUIは `hane-gui-test`、ADR-0024、Commander Policyに従い、対象head・必要base context・実行入口・観測・artifactを残す。GUI不調では `docs/gui-test-recovery.md` に従って復旧する。Windowsは許可済みWindowsチャットの直接操作のみ。Mac経由VMを使わない。接続／担当の特定とGUI実施は残件であり、unknownをpassにしない。

準備commitでは文書の差分・参照・日本語を確認するだけで、製品受入の成功とはしない。実装・required CI・レビュー・GUI・mergeはPR担当チャットが進める。

# Issue #672: 未保存タブを保存せず破棄する実装引き継ぎ

## 状態と正本

- 要求・範囲・受入条件の正本: [Issue #672](https://github.com/hide212131/hane/issues/672)。本書は具体的な接続・保護条件・検証対応を記録する。
- 基準main: `1e0d345e46d8c1805659968f8fad1148b0ae37a8`（2026-11-04 JST調査）。branch: `codex/issue-672-discard-tab`。
- **Claude実装待ちの準備PR**。このcommitは文書だけで、製品コード・テストの変更、実機GUI検証は行っていない。
- PR作成以降は別のPR担当チャットが進行する。検討担当はClaudeへの依頼コメントを投稿しない。PR担当がcurrent factsを取り直して一度だけ依頼し、prepare成功で受理を確認する。
- 既存ADR-0019のsession／I/O／UI境界内の局所拡張。新ADRは不要。境界・保存形式・復旧形式の変更が必要と判明した場合は勝手に拡大せず再判断する。

## 実装後の回帰テスト範囲（Claude補完分）

実イベント（中クリック・右クリックclose・Cmd/Ctrl+W・Escape・背景click）から破棄ボタンへ到達するGPUIテスト、遅延・失敗・部分失敗を制御できる `ScriptedDraftStore`、保存中／H1 create中／draft削除中の追加編集・新確認・文書差替え・workspace切替を `crates/ui/src/view.rs` のテストに追加した。局所修正は、新しい確認が走った時点で旧承認を失効させること（`PendingTabDiscard::superseded`）と、承認済み破棄がある間は保存完了後にキュー済みの本文保存を開始しないこと。このworkerはテストを実行していないため、検証結果はCIで確認する。native Save Asと両OSの実GUIは後続検証である。

## 利用者報告と調査結果を分ける

原文は「保存していないタブを閉じるとき、保存せずに破棄というルートがない。保存するしかない」。報告OS・版・入口・文書種別は不明。

コードでは確認が2択であることを確認した。キャンセル経路は存在するため、「閉じるには保存が必要」と「取消できない」を同一視しない。実画面の再現と両OSの挙動は未確認であり、製品受入evidenceにはしていない。

## 既存の責務と変更候補

| パス・シンボル | 基準mainの契約／今回の接続 |
|---|---|
| `crates/ui/src/view.rs`: `request_tab_close`, `close_active_file_tab`, `close_file_tab_from_menu` | 中クリック・Cmd/Ctrl+W・右クリックcloseを共通化済み。入口を別実装しない |
| 同: `TabCloseConfirm`, `DocumentInstance`, `tab_close_confirm_element` | 保存して閉じる／キャンセルの2択。描画時snapshotと生きた確認全体、および文書・workspace世代を照合する。ここへ明示破棄を追加 |
| 同: `confirm_save_and_close_tab`, `prompt_save_as_for_close`, `handle_save_as_for_close_response`, `resolve_pending_tab_close` | path有無別の保存・結果受理。取消／失敗／競合／SavedStaleを破棄扱いしない |
| 同: `close_tab_now` | 現在はclean文書用。`SessionSet::close`もdirty拒否するため、ボタンからこの関数を呼ぶだけでは不足。close後の問題表示・確認・メニュー・active文書再構築等の後処理を維持 |
| `crates/session/src/session.rs`: `DocumentSession::close_decision`, `SessionSet::close` | 通常closeのdirty guardを維持し、明示承認された対象に限定する破棄操作をsession責務として追加する。dirtyを偽って保存済みにしない |
| `crates/ui/src/view/session_save.rs`: `schedule_draft_save`, `flush_pending_drafts`, `retire_work_folder_draft`, 保存・H1命名処理 | 対象draftとtimer／非同期処理の寿命を調停する。既存retireは保存後のbest effort削除であり、今回の破棄成功確認とは契約が違う |
| `crates/session/src/draft.rs`: `DraftStore::remove`, `OsDraftStore::write_snapshot` | per-root/DraftId lockとsequenceによりremove以前のsnapshotを拒否済み。この経路を使い、独自の直接ファイル削除を足さない |
| `crates/ui/src/actions.rs`, `crates/ui/src/view/render.rs` | 既存の入力先・Escape・確認失効処理を保持し、実イベントで破棄ボタンへの到達を確認 |
| `crates/app/src/main.rs`: `on_window_should_close`、viewのapp quit hook | 既存draft flushを維持。破棄済み対象がflush対象へ残らないことだけを確認し、終了全体の確認機能を新設しない |

実装時はcurrent mainでパス・シンボル・責務を再確認する。大規模なView分割等と混ぜず、受入済みの経路を拡張する。

## 文書種別と選択肢

| 対象 | 保存して閉じる | 保存せずに破棄 | キャンセル |
|---|---|---|---|
| dirtyな既存ファイル／path確定済み文書 | 既存保存経路。成功してcleanかつ対象一致のときだけclose | 未保存のメモリ編集を捨て対象だけclose。diskファイルは保持 | 内容とタブを保持 |
| 未保存の新規無題（作業フォルダ外） | 既存native Save As。取消／失敗は保持 | Save Asなしで対象だけclose | 同左 |
| 未保存の新規無題（作業フォルダ内draft） | 同上。H1命名でpathが確定済みなら上の行 | 対象の復旧journalだけを除外してclose。別draftは保持 | 同左 |
| clean文書・空無題 | 通常closeの既存動作を維持。3択を不要に増やさない | 同左 | 同左 |

自動保存やH1命名ですでにdiskへ到達した内容は巻き戻さない。pathの有無とdirtyは別の状態であり、「一度も手動保存していない」をpathなしと決めつけない。GUI fixtureは自動保存設定とH1の有無を記録する。

## 破棄の保護条件・処理順

1. 確認snapshotが現在の確認と一致し、SessionId・DocumentInstance（文書世代とworkspace世代）が現在の対象に一致する場合だけ処理する。古いclickや新しい確認に対する旧イベントを無効にする。
2. 明示破棄専用の承認経路を使う。通常close、ファイルopen、外部変更再読込等のdirty保護を緩めず、選択中タブへの置換もしない。破棄で本文の保存・Save As・強制上書きを新たに開始しない。
3. 進行中save／H1 create・renameがある場合は既存の要求・結果受理と調停する。安全になるまで破棄を待機させ、画面で待機を示すなどして利用可能な経路を残す。受理済みI/Oの取消・巻戻しを保証しない。待機中に文書・workspaceが変われば失効し、追加編集があれば古い本文への承認で破棄せず再確認する。未完了writerと競合して別名ファイル生成やclose後の状態復活を起こさない。
4. draftを持つ対象では、対象を一時的に破棄処理中として扱い、後続timer・終了flushが新しいsnapshotを発行してremove後に復活させないよう調停する。remove以前の取得済みsnapshotは既存sequence契約を利用して拒否する。削除のI/Oは背景で行い、対象root／DraftIdと文書identityを固定する。
5. draft除外成功と対象の鮮度を確認して対象だけをcloseする。非同期中の編集・新しい確認・folder切替・ID再利用があれば旧承認でcloseしない。削除失敗は本文とタブを保持し、成功扱いせずエラーを表示する。失敗・失効後も現存文書のdraft保存を再開し、復旧不能な中間状態を残さない。部分的削除失敗・編集後の再journalもテストする。
6. close後は対象のdraft追跡、pending close、保存問題、メニュー、派生状態を既存責務に沿って片付ける。他文書の状態を消さない。最後の文書は既存の新IDの空無題へ置換する。

具体的な新API名や待機の実装方法は固定しない。既存の保存・draft境界内で上記不変条件を満たす最小変更とする。無関係なrare raceへの全域対応へ拡張しないが、本文のデータ損失・誤編集や通常経路での復活はblocker。

## 検証対応

`hane-gui-test`とADR-0024に従う。新しいテストは実際の保護契約を検証し、実装の写しにしない。

| 不変条件 | focused検証 |
|---|---|
| 正しい対象だけ破棄、元ファイル保持 | session＋GPUI実click。active／非active既存dirty、pathなし無題、複数タブ。disk本文を比較 |
| 保存・取消契約維持 | GPUI保存／キャンセル／Escape／背景click、native Save Asの取消・選択成功・失敗。保存失敗・競合・SavedStaleの既存回帰を維持 |
| stale承認拒否 | 確認snapshot、文書差替え、workspace切替、ID再利用、連打、削除完了中の追加編集／確認変更を決定的に再現 |
| draftの非復活と他draft保持 | removeと遅延snapshotの順序、debounce前の破棄、flush、folder再読込／再起動、削除失敗・再試行・再journal。複数draftから対象だけ除外 |
| writerとの整合 | save中・H1自動命名create／rename中に破棄。既存I/Oの完了・失敗と失効、対象に追加編集があるケース |
| 最終タブ・入力先 | 空／単一／複数、最後タブ新ID、既存ショートカット3入口、設定入力・IME変換を横取りしない |
| 実画面で使える3択 | macOS／Windowsで中クリック・右click・Cmd/Ctrl+Wから到達し、既存編集と新規無題で3択、非active、取消・失敗・最終タブ・draft再起動確認 |

対象に適した既存tests、workspace tests、fmt、必要なbuild/lintとcurrent-head CIを行う。テスト／GUI対象headと必要なbase context・fixture・結果・artifactをPRに残す。GUI未実施を製品failともpassとも扱わない。必要evidence不足のまま受入／mergeしない。

Windowsは利用者が許可した既存Windowsチャット上のChatGPTアプリから直接操作し、Mac経由VM操作は使わない。担当チャット・接続を特定してから依頼する。GUI障害時は `docs/gui-test-recovery.md` で復旧し、PID・実行path・隔離stateを記録する。

## 関連作業と残る確認

- #551のIssueはopenだがPR #552はmerge済み。#411／PR #416とともに共通入口を再利用する。close入口追加を再実装しない。
- #308／#309／#368、#653／PR #655と保存・命名・draft・非同期jobを共有する。current main上の実装を基準とし、RF全体の完了は待たない。必要な局所整理が判明したときのみ別PRで先行させる。
- #542はタイトルバー、#592は履歴、#659はクラッシュ保存退避の復旧で今回対象外。調査時のopen PR #671は更新確認準備で別責務。開始時に最新の競合作業とrunを再確認する。
- 共有作業ツリーの既存製品変更は本PRに含めない。検討担当は独立checkoutでこの文書だけをcommitする。
- PR担当はIssue／本書／default branchのCommander PolicyとClaude実行手順を読み、exact head・write権限・same-repository/open・重複runを確認してClaudeへ依頼する。製品コードはCommanderが変更しない。
- 実装後のCI、必要な両OS GUI、CodeRabbitの最終current-head full reviewと受入はPR担当へ引き継ぐ。Draft作成を実装完了としない。

## Jev判断の記録

公開GitHub情報だけを使い、provider `typesafe`、要求model `jev-latest`、実応答 `jev-1.14.0` で確認した。Issue作成選択 `create_issue`（0.59）、文書準備選択 `prepare`（0.83）。通常の意味判断として採用し、Commanderが権限・main鮮度を確認した。追加の費用・性能閾値は導入していない。

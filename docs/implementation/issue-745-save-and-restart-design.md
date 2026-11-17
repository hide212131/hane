# Issue #745 設計書（Codex GPT Sol 作成、司令塔検分済み）

位置づけ: 本書は Issue #745 の実装設計である。対象コードは v0.48.0（24a40400）。実装時は実コードを正とし、本書と食い違う場合は実コードの契約を優先して司令塔に報告すること。

対象コードは `24a40400a3fedbc3ae229e6a408a6264cd497ea4`（`v0.48.0`）です。以下は実コードを確認した設計案です。ファイルの変更・テスト実行は行っていません。

**1. 現状の構造**

主な参照箇所は次のとおりです。

| ファイル | 確認した責務 |
|---|---|
| `crates/ui/src/view/app_update.rs` | ブロッカー、更新 UI、再起動準備・終了処理、既存テスト |
| `crates/ui/src/view/session_save.rs` | 個別保存、非同期完了処理、自動保存、下書き保存、タイトル同期 |
| `crates/session/src/session.rs` | dirty 判定、保存要求・チケット・完了結果 |
| `crates/session/src/service.rs` | 通常保存の I/O と外部変更の検査 |
| `crates/ui/src/view/footer.rs` | セッション単位の保存失敗記録・表示 |
| `crates/ui/src/view.rs` | 保存試行の構造体、更新状態、編集後処理 |
| `crates/ui/src/view/initialization.rs` | 更新状態の初期化 |

ブロッカー収集は `restart_blockers` が担当します。アクティブタブだけでなく、開いている全セッションを調べます。

- `is_dirty()` ごとに `DirtyDocument(session.label())`。
- `save_in_flight()` ごとに `SaveInProgress(session.label())`。
- 全セッションのエディタ、サイドバーフィルター、インライン名前変更の IME 状態から `ImeComposition`。
- タイトル同期の各集合、インライン名前変更、タブ終了確認・保存後終了待ち、タブメニューから `PendingEdit`。
- 保存先ダイアログ、ファイル読み込み、AI 処理・ログイン・復旧状態、設定フォームの未保存変更、設定反映処理も個別に検査。

`DirtyDocument(String)` が保持するのは表示ラベルだけです。保存対象の識別やパス有無の判定には使えません。

更新操作の入口は二つあります。

| UI | 入口 | メッセージ表示 |
|---|---|---|
| 文書上部の更新帯 | `update_prompt_element` → `start_update_restart(Some(candidate_id))` | `app-update-prompt-message` |
| 一般設定の更新カード | `app_update_card` → `run_app_update_action(CardAction::Restart)` → `start_update_restart(None)` | `settings-app-update-restart-message` |

どちらも `restart_message` を表示します。現在、ブロッカーがあれば `RestartBlocker::message()` を連結して表示するだけです。

再起動処理は次の順序です。

```text
start_update_restart
  → Ready・再起動対応・candidate を確認
  → prepare_update_restart
      → 短時間の restart freeze
      → restart_blockers
      → 下書き・設定・最近使ったファイルを保存
      → RestartObservation を検証
      → freeze 解除
  → restart_ticket_snapshot
  → restart_in_progress = true
  → background で controller.prepare_restart
  → continue_update_restart
      → freeze を取得
      → 全ブロッカーと observation を再検証
      → run_exit_sequence
          → ticket 発行
          → 下書きを再度排出
          → AI 停止
          → freshness 再確認
          → 外部 open queue を seal
          → helper に終了許可
      → cx.quit()
```

`RestartObservation` は全セッションの ID・generation・revision、workspace generation、external-open generation を記録します。設定などは observation だけで判断せず、後段のブロッカー再収集でも検査します。

`restart_freeze` は外部 open を `Busy` にする仕組みです。**編集全般や自動保存を禁止するロックではありません。** ヘルパー準備中も編集可能で、後段の再検証によって変更を検出する構造です。

保存経路は次のとおりです。

```text
save_current
  → save_active(Current)
  → save_session(SessionId, Current)
      → DocumentSession::request_save
      → SaveDecision::Write(job)
      → background で run_save_job
      → UI thread の finish_save
      → DocumentSession::finish_save
      → 保存状態・最近使ったファイル・下書き・タイトル同期等を更新
      → pending_save を再実行
```

`save_session` は非アクティブなセッションも保存できます。戻り値は `()` であり、呼び出し側が完了を待つ仕組みはありません。

セッション内では保存開始時に文書をスナップショットし、`SaveTicket` で完了を照合します。UI 側も `DocumentInstance` により workspace の交換や同じ ID の再利用を防いでいます。結果には次があります。

| 結果 | 意味 |
|---|---|
| `Saved` | 保存スナップショットと現在の文書が一致 |
| `SavedStale` | 保存は成功したが、その間の編集が残っている |
| `Conflict` | 外部変更を検出して書き込みを拒否 |
| `Failed` | I/O 失敗 |
| `Superseded` | 現在の保存試行に対応しない結果 |

保存中の追加要求は `pending_save` 一つに集約されます。`finish_save` はその再実行に加え、タイトル同期の再試行なども行います。

**2. 「保存して再起動」の設計**

**提示条件**

現在のブロッカー列を `B`、全セッションのうち `is_dirty()` が真の集合を `D` とします。次の全条件を満たす場合だけ提示します。

```text
更新が Ready
かつ restart_supported
かつ candidate_id が存在
かつ再起動・一括保存が進行中ではない
かつ restart freeze 中ではない
かつ D が空でない
かつ D の全セッションが path().is_some()
かつ B の全要素が DirtyDocument
```

空の `B` に対する `all()` だけで判定してはいけません。dirty 文書がない場合は従来の「再起動して更新」です。

判定例は次のとおりです。

| 状態 | 提示 |
|---|---|
| パス付き dirty 文書が一つ以上、他ブロッカーなし | する |
| パス付き dirty 文書＋パスなし dirty 文書 | しない |
| パス付き dirty 文書＋変更されていない無名セッション | する |
| dirty 文書なし | しない。通常の再起動 |
| dirty 文書＋IME・保存中・`PendingEdit` 等 | しない |

パスなしセッションについては、**パスの有無だけで一律除外せず、dirty かどうかで区別します。**

`DocumentSession::is_dirty()` の定義は次です。

```rust
self.revision() != self.saved_revision
```

`schedule_draft_save` と `flush_pending_drafts_checked` は下書きストアへ本文を書きますが、`saved_revision` を更新しません。したがって、下書き保存済みでも編集済みの無名メモは dirty のままであり、今回の対象外です。

変更されていない無名セッションは既存処理でも許容されています。一括保存の対象には加えず、既存の checked draft flush に引き継ぎます。その下書き保存が失敗すれば従来どおり再起動を止めます。

**UI**

更新帯と設定カードの両方で、上記条件を満たすときは主アクションを「保存して再起動」にします。それ以外は従来のアクションとブロッカー表示を維持します。

表示判定は副作用のない共通関数に集約します。描画時に `prepare_update_restart` を呼んではいけません。同関数には下書き・設定の保存という副作用があるためです。

クリック時には表示時の `candidate_id` を渡し、条件を再確認します。古いボタンが別 candidate を保存・再起動することを防ぎます。

**保存方式**

対象はクリック時の `D` 全件です。非アクティブタブも含め、`SessionId` 昇順で逐次保存します。アクティブタブは切り替えません。

逐次方式により、失敗した時点で後続の保存を止められ、同時に追跡する書き込みも一つで済みます。成功済みファイルはそのまま保存済みとし、ロールバックしません。

使用する intent は `SaveIntent::Current` に限定します。`Overwrite`・`To`・自動採番・保存先ダイアログは使いません。通常保存の外部変更検査を維持します。

**完了通知の追加**

既存の `save_session` を内部共通処理へ分離し、更新用に一回限りの完了通知を付けられる入口を追加します。

概念上の API は次です。名前は新規提案です。

```rust
save_session_with_completion(id, SaveIntent::Current, cx)
    -> Task<TrackedSaveResult>
```

実装上の契約を次に固定します。

- 通常の `save_session` は通知なしで同じ内部処理を呼ぶ。
- 更新用通知は、実際に開始した `SaveTicket`・`DocumentInstance`・対象パスに結び付ける。
- `finish_save` の既存の後処理まで終わってから通知する。
- `Saved` 以外は再起動を続行できない結果として返す。
- ゴミ箱移動による拒否、セッション消失、`NeedsPath`、`Queued`、`ApprovalExpired` など、書き込み開始前の終了も必ず通知する。
- instance 不一致など `finish_save` の早期 return でも通知する。
- 通知経路が閉じた場合も成功とは扱わない。

更新用要求が `Queued` になった場合は中止します。既存のキューは後の要求で置き換わり得るため、その完了を今回の保存完了として待ちません。開始前に保存スロットを再確認し、通常はこの分岐へ入らない構造にします。

`save_in_flight == false` や `is_dirty == false` のポーリングを完了判定に使ってはいけません。別の保存の成功や、失敗後の状態と今回の結果を区別できないためです。

**進行状態と再起動への合流**

`EditorView` に一括保存専用の状態を追加します。

```text
UpdateSaveBatch
  operation_id
  candidate_id
  開始時の全セッション構成・instance・revision・path
  保存対象一覧
  次の対象位置
```

`operation_id` は遅れて届いた完了通知を識別するためのローカルな番号です。

一括保存中は `restart_in_progress` をまだ立てず、一括保存状態の存在によって通常再起動と新しい一括保存を禁止します。UI の busy 判定は両状態の和にします。

全件の `Saved` を確認した後、同じ UI 更新内で次を行います。

1. 開始時からのセッション・revision・path の変化を検査。
2. `restart_blockers` を通常どおり再収集し、空であることを確認。
3. 一括保存状態を解除。
4. `start_update_restart(Some(original_candidate_id), cx)` を呼ぶ。

**再起動への合流点は `start_update_restart` です。** 保存前の observation・ticket は流用せず、保存後に既存経路で新しく取得します。`continue_update_restart` や終了許可へ直接進んではいけません。

**失敗時**

最初の失敗で一括保存を中止し、後続保存・ヘルパー準備・AI 停止・終了許可を実行しません。

`restart_message` に対象ラベル、パス、理由を表示します。例：

```text
「notes.md」を保存できなかったため、更新を中止しました。
保存先: …
理由: …
```

外部変更は外部変更として説明し、自動上書きしません。既存の `save_problems` による文書単位の警告も保持します。

一括保存状態を解除して編集・再試行可能に戻します。本文、dirty 状態、成功済みの保存結果は既存保存処理の結果をそのまま維持します。

**3. 二重押しと保存中の状態変化**

二重押しは UI の非表示だけでなく、操作入口でも防ぎます。最初の書き込みを開始する前、同じ UI 更新内で一括保存状態を登録します。通常再起動入口にも一括保存中のガードを追加します。

保存中は「保存しています…（完了数／対象数）」を表示します。編集は引き続き可能とし、再編集があった場合は自動再保存を繰り返さず、今回の更新を中止します。

| 保存中の変化 | 対応 |
|---|---|
| 保存対象を再編集 | `SavedStale` または revision 差分で中止 |
| 保存済み対象や対象外文書を編集 | 開始時 snapshot との差分で中止 |
| セッション追加・終了、workspace 交換、`adopt` | 構成・instance 差分で中止 |
| ファイル名・保存先変更 | path 差分で中止 |
| IME・設定変更・新たな読み込み等 | 次の保存前／最終再収集で中止 |
| candidate 変更・Ready 解除 | 次の保存前／合流時に中止 |
| 古い操作の完了通知 | `operation_id` 不一致として無視 |

既に開始した I/O は無理に取り消しません。通常の保存完了処理は適用し、再起動の続行だけを止めます。

各保存の前後では、今回の残り対象の `DirtyDocument` と、現在待っている自分の保存だけを識別して扱います。他の `SaveInProgress` や `PendingEdit` を免除してはいけません。

特に `finish_save` は pending save の再実行や `retry_title_sync` を呼びます。完了通知を後処理の後へ置くことで、そこで新たに発生した保存・名前変更を検出できます。その場合も今回の再起動は中止し、従来の理由を表示します。

また、`after_input` はタイトル同期を予約します。作業フォルダー内の編集直後は `PendingEdit` が存在し得るため、dirty 文書だけを見てアクションを出してはいけません。タイトル同期の各集合を空にして強行する処理も追加しません。

**4. ファイル・関数単位の作業分割**

| 単位 | 変更内容 |
|---|---|
| `app_update.rs`：提示判定 | `save_and_restart_targets` 相当を追加。ブロッカー集合と実セッションから対象 ID を決定 |
| `app_update.rs`：UI | `update_prompt_element`・`app_update_card` に共通条件を適用。candidate 付きアクション、busy 表示、キーボード操作を接続 |
| `app_update.rs`：操作入口 | `start_save_and_restart` 相当を追加。逐次保存、状態検証、失敗表示、通常再起動への合流を担当 |
| `app_update.rs`：既存入口 | `start_update_restart` に一括保存中のガードを追加 |
| `session_save.rs` | `save_session` の内部処理を共通化。完了通知付き入口を追加し、`finish_save` の全終了経路で通知 |
| `view.rs` | 一括保存状態、操作番号、通知を運ぶ保存試行の情報を追加 |
| `view/initialization.rs` | 新しい状態を初期化 |
| `app_update.rs`・`session_save.rs` のテスト | 提示条件、保存通知、再起動への接続、競合を検証 |

`crates/session/src/session.rs` の dirty 定義、保存キュー、保存チケットの意味は変更しません。`service.rs` の I/O や外部変更検査も再実装せず、そのまま使用します。

**5. テスト計画**

既存の以下のテストを回帰対象として維持します。

- `a_dirty_document_blocks_and_keeps_its_content`
- `a_dirty_background_session_blocks`
- `a_draft_write_failure_blocks_the_restart`
- `an_open_save_as_dialog_blocks`
- `an_external_open_during_the_freeze_is_busy_and_not_consumed`
- `the_exit_is_authorized_last_after_the_ticket_drain_and_ai_stop`
- ヘルパー準備中の設定変更・AI 開始・open を検出する既存テスト群

追加テストは次の単位とします。

| 追加テスト名 | 検証内容 |
|---|---|
| `save_and_restart_is_offered_only_for_path_backed_dirty_sessions` | 単数・複数・非アクティブ対象で提示。dirty なしでは通常アクション |
| `a_dirty_unnamed_session_disables_save_and_restart_even_after_draft_flush` | 下書き保存成功後も dirty が残り、名前付き dirty 文書との混在で非提示 |
| `a_clean_unnamed_session_does_not_disable_save_and_restart` | 未編集の無名セッションは対象に含めず許容 |
| `other_restart_blockers_keep_the_existing_message_and_actions` | IME、保存中、タイトル同期、ダイアログ、open、AI、設定等との混在で非提示。従来メッセージを維持 |
| `save_and_restart_saves_all_targets_before_preparing_the_helper` | 複数ファイルの本文を検証。ID 順・同時書き込み最大一件・全保存後に一度だけヘルパー準備 |
| `save_and_restart_rejoins_the_existing_exit_sequence` | 保存後の ticket を使い、既存の ticket→draft→AI→authorize の順序へ接続 |
| `a_save_failure_stops_the_batch_and_shows_the_target_and_reason` | I/O 失敗で後続保存・再起動を止め、対象と原因を両 UI に表示。成功済み保存と未保存本文を保持 |
| `an_external_conflict_never_forces_save_or_restart` | 外部変更を保持し、強制上書き・再起動を行わない |
| `a_stale_save_aborts_restart_without_losing_new_edits` | I/O を保留して編集し、`SavedStale` 後も新しい本文・dirty を保持 |
| `changes_to_another_session_abort_the_batch` | 対象外文書／既に保存した文書の再編集も検出 |
| `replaced_sessions_and_changed_paths_cannot_continue_the_batch` | workspace・instance・path 変更、セッション構成変更を検出 |
| `double_activation_starts_only_one_save_batch` | マウス・キーボード・通常再起動入口から重複開始できない |
| `a_changed_candidate_cannot_be_restarted_after_saving` | 表示後／保存中の candidate 変更で別更新を開始しない |
| `tracked_save_reports_every_terminal_path_once` | 開始拒否、instance 不一致、各 `SaveOutcome`、通知切断を検査 |
| `post_save_pending_work_blocks_restart` | 保存完了後に pending save・タイトル同期が始まった場合、通知後の再検査で停止 |
| `persistence_failure_after_batch_save_still_blocks_restart` | 全ファイル保存後でも下書き・設定保存失敗なら終了しない |

非同期テストには完了を明示的に解放できる `FileService` の fake を使い、時間待ちに依存せず保存中の編集や二重押しを再現します。成功テストはメソッド呼び出し回数だけでなく、保存内容と終了許可まで確認します。

実装時は `hane-ui` の対象テストと既存保存回帰テストを実行します。GUI 確認は対象 SHA の Debug/development build で、両 UI の提示、キーボード操作、成功、保存失敗、無名メモ・IME 混在を確認します。

**6. 受入条件チェックリスト**

- [ ] パス付き未保存文書だけがブロッカーの場合、更新帯・設定カードに「保存して再起動」が表示される。
- [ ] 非アクティブタブを含む対象全件を、通常保存と同じ外部変更保護で保存する。
- [ ] 全件の正確な保存成功を待ってから、追加クリックなしで既存の更新再起動へ進む。
- [ ] 保存失敗・外部変更時は再起動せず、失敗対象と理由を表示する。
- [ ] 失敗時も本文と既存の保存警告を保持し、編集・再試行できる。
- [ ] dirty な無名メモは、下書き保存済みでも対象外になる。
- [ ] IME、保存中、`PendingEdit` その他のブロッカー混在時は従来動作を維持する。
- [ ] 二重押しで保存バッチや再起動が重複しない。
- [ ] 保存中の再編集・セッション交換・保存先変更・candidate 変更によって誤って終了しない。
- [ ] 保存後も既存の下書き・設定保存、再検証、外部 open 保護、終了許可の順序を維持する。
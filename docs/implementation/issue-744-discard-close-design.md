# Issue #744 設計書（Codex GPT Sol 作成、司令塔検分済み）

位置づけ: 本書は Issue #744 の実装設計である。対象コードは v0.48.0（24a40400）。実装時は実コードを正とし、本書と食い違う場合は実コードの契約を優先して司令塔に報告すること。

# Issue #744 設計書：タブを保存せずに閉じる

対象は v0.48.0、HEAD／ローカルの `origin/main` がともに `24a40400a3fedbc3ae229e6a408a6264cd497ea4` のコードです。以下は実コードを読んだ設計であり、ファイル変更・テスト実行はしていません。

実装方針は、**確認ダイアログに「保存せずに閉じる」を追加し、明示的な破棄を許可するセッション API を新設する**ことです。タブ削除・最後のタブの置換・UI 更新は既存処理と共用します。

## 1. 現状の構造

### 1.1 タブを閉じる入口

`crates/ui/src/view.rs` と `crates/ui/src/actions.rs` で、次の入口を確認しました。

| 入口 | 呼び出し経路 | 対象とガード |
|---|---|---|
| タブの中クリック | `header_element` → `request_tab_close(id, cx)` | 描画時に捕捉したタブの ID |
| タブのコンテキストメニュー「タブを閉じる」 | `close_file_tab_from_menu` → `request_tab_close` | メニューが捕捉した `DocumentInstance` と現在の文書が一致する場合だけ実行 |
| Cmd/Ctrl+W | `CloseFileTab` action → `close_active_file_tab` → `request_tab_close` | アクティブタブ。確認表示中は無操作。サイドバーフィルター・インラインリネーム・IME 等の既存入力制御あり |

この版の `header_element` には、**タブの×ボタンはありません**。今回、閉じる入口を増やす必要はありません。

上記の入口は、すべて次の共通判定を通ります。

```text
request_tab_close(id)
  └─ DocumentSession::close_decision()
       ├─ Close                → close_tab_now(id)
       └─ Reject(UnsavedChanges)
            → TabCloseConfirm { id, instance } を設定
```

`DocumentSession::close_decision` は `is_dirty()` だけを判定します。未変更タブは確認なしで閉じます。

### 1.2 確認ダイアログ

`crates/ui/src/view.rs::tab_close_confirm_element` は、次の構成です。

- 本文：`"{label}" の変更を保存しますか?`
- 左から「キャンセル」「保存して閉じる」
- キャンセル：`dismiss_tab_close_confirm_if(snapshot, cx)`
- 保存：`confirm_save_and_close_tab(snapshot, cx)`
- ダイアログ外のクリック：`dismiss_tab_close_confirm_if(snapshot, cx)`
- オーバーレイの `.occlude()` により背面へのクリックを遮断

Escape は `dismiss_tab_close_confirm` を呼びます。こちらは描画時の特定ダイアログではなく、現在表示中の確認を取り消す仕様です。

### 1.3 保存して閉じる処理

`confirm_save_and_close_tab` の処理順は次のとおりです。

1. 現在の `tab_close_confirm` と、ボタンが捕捉した `TabCloseConfirm` 全体を比較する。
2. `document_instance(id)` と確認が保持する `instance` を比較する。
3. 一致した場合だけ確認を解除する。
4. パスの有無で保存経路を分ける。

| 対象 | 処理 |
|---|---|
| パスあり | `tab_close_after_save` に対象 instance を登録し、`save_session(id, SaveIntent::Current, cx)` |
| パスなし | `prompt_save_as_for_close` で保存先を問い合わせる。この時点では閉じる予約を登録しない |

パスなしの場合、`handle_save_as_for_close_response` が文書 instance を再確認します。保存先選択後は `save_as_to(..., true, ...)` を経由し、`crates/ui/src/view/image_paste.rs::write_save_as` で保存に対応する閉じる予約を設定します。保存先選択のキャンセル・失敗では閉じません。

保存完了は `crates/ui/src/view/session_save.rs::finish_save` が扱います。

- 文書 instance が変わっていれば、旧保存結果で現在の文書を操作しない。
- `SaveOutcome::Saved` の場合に `resolve_pending_tab_close` を呼ぶ。
- `SavedStale`、競合、失敗等では閉じる予約を解除する。
- `resolve_pending_tab_close` は instance の一致、保存処理が残っていないこと、dirty でないことを確認して `close_tab_now` を呼ぶ。

したがって、保存中に追加編集された文書を、古い保存結果だけで閉じることはありません。

### 1.4 既存の閉じる内部経路

`close_tab_now` は無条件破棄ではありません。

```text
close_tab_now
  └─ SessionSet::close
       └─ close_decision が dirty を拒否
```

`crates/session/src/session.rs::SessionSet::close` は、未保存なら `Reject(UnsavedChanges)` を返します。未保存変更を破棄して閉じる専用 API はありません。

閉じることが認められた場合の既存動作は次のとおりです。

- 最後のタブなら、新しい ID の空の `Untitled` に置換する。
- 複数タブなら対象だけを除去する。
- アクティブタブを除去した場合は、残った先頭セッションを選択する。
- 非アクティブタブを除去しても、アクティブセッションは維持する。
- UI 側で保存後クローズ予約、保存問題、対象の確認・メニューを除去する。
- 必要に応じて文書表示・検索・解析・タブの表示位置を更新する。

### 1.5 スナップショットによる stale click 対策

現在の `TabCloseConfirm` は次の値を持ち、`Copy` と値比較ができます。

```rust
struct TabCloseConfirm {
    id: SessionId,
    instance: DocumentInstance,
}
```

`DocumentInstance` は文書の `generation` と作業フォルダーの `workspace` 世代です。

各ボタンと背景クリックは `let snapshot = *confirm` を捕捉します。クリック時に現在の確認と比較するため、同じ ID が別文書に再利用されても、古いクリックで新しい確認を操作しません。保存ハンドラは、さらに現在の文書 instance も検査します。

`crates/ui/src/view/render.rs` は描画時にも古い確認を除去しますが、イベントが先に届く場合があるため、ハンドラ内の検査は必須です。

ただし、**同じ文書 instance について確認をキャンセルして再表示した場合、現在の二つのフィールドは同値になります**。現行構造は、この二回の表示を区別しません。今回の破棄操作では、この点も明示的に対処します。

## 2. 実装方式

### 2.1 セッション層に明示的な破棄 API を追加する

`SessionSet` に、次の API を追加します。

```rust
pub fn close_discarding_changes(
    &mut self,
    id: SessionId,
    label: impl Into<String>,
) -> bool;
```

戻り値は、対象が存在して閉じられた場合に `true`、存在しない場合に `false` とします。

この API は次だけを行います。

- dirty 判定を省略する。
- 対象を除去する、または最後のタブを空の `Untitled` に置換する。
- 既存と同じアクティブセッション選択を行う。

保存、再読込、ファイル削除は行いません。文書を疑似的に「保存済み」にして既存ガードを通す方式も採用しません。

現在の `SessionSet::close` から、削除・最後のタブの置換処理を private helper に切り出し、通常 close と破棄 close で共用します。通常の `close` にある dirty 拒否と、対象不在時の既存戻り値は維持します。

### 2.2 UI の閉じる後処理を共用する

`close_tab_now` のうち、`SessionSet::close` 成功後の処理を、例えば次の helper に分離します。

```rust
fn finish_tab_close(
    &mut self,
    id: SessionId,
    was_active: bool,
    cx: &mut Context<Self>,
);
```

通常 close と破棄 close の両方が、セッションを閉じた後に呼びます。`was_active` は削除前に取得します。

共用する内容は、現行 `close_tab_now` の後処理です。加えて、dirty なセッションを直接除去するため、外部 open 保護の状態を `sync_external_open_guard()` で同期します。

これにより、破棄経路だけ解析・検索更新や保存後クローズ予約の除去を忘れることを防ぎます。

### 2.3 新しい確認ハンドラ

ハンドラ名は次とします。

```rust
fn confirm_discard_and_close_tab(
    &mut self,
    requested: TabCloseConfirm,
    cx: &mut Context<Self>,
);
```

処理順は固定します。

1. `self.tab_close_confirm == Some(requested)` を確認する。
2. `self.document_instance(requested.id) == Some(requested.instance)` を確認する。
3. 対象セッションが保存処理中でないことを再確認する。
4. 削除前の `was_active` を取得する。
5. `SessionSet::close_discarding_changes` を呼ぶ。
6. 成功した場合だけ `finish_tab_close` を呼ぶ。

**ガードをすべて通る前には、確認・予約・メニューを変更しません。** 古いクリックは副作用なしで戻ります。

保存関数や追加の確認ダイアログは呼びません。非アクティブタブを先にアクティブ化する必要もありません。

### 2.4 確認の表示世代を追加する

同じ文書に再表示された確認も区別するため、`TabCloseConfirm` に `generation: u64`、`EditorView` に確認世代のカウンターを追加します。

- `request_tab_close` が新しい確認を設定するたびに世代を進める。
- 三つのボタンと背景クリックは、世代を含む構造体全体を捕捉する。
- 保存・キャンセルの既存の全体比較も、そのまま新しい世代を比較対象にする。
- 文書 instance の検査も維持する。
- Escape は従来どおり現在の確認を取り消す。

これは文書の編集 revision とは別です。確認表示中の通常編集まで自動的に確認を失効させる仕様変更は行いません。

### 2.5 保存予約・進行中の保存との関係

`tab_close_after_save` は保存の取消ハンドルではなく、**保存成功後に閉じる予約**です。除去しても、開始済みのファイル書込みは取り消せません。

「保存せずに閉じる」の契約を守るため、次の扱いにします。

| 状態 | 扱い |
|---|---|
| 保存処理なし | 破棄して閉じる。対象の `tab_close_after_save` は共通後処理で除去 |
| 保存処理中 | 破棄ボタンを表示したまま無効化し、「保存処理中です」と表示 |
| 描画後に保存が開始された | ハンドラ側の再検査で無操作。確認を維持 |
| 保存前の autosave／H1 命名タイマーが待機中 | 破棄 close を実行可能。閉じた後は既存の instance 検査で保存開始を拒否 |
| 旧 Save As ダイアログの応答が後着 | 閉じた文書の instance と一致しないため既存ガードで拒否 |

保存中の無効化は、取消不能な書込みが残ったまま「ファイルを変更せず閉じた」と扱わないための仕様です。既に完了した保存を巻き戻す機能は含めません。保存・キャンセルの既存動作は維持します。

## 3. ボタンの並び順とラベル

左から次の順序にします。

```text
キャンセル　　保存せずに閉じる　　保存して閉じる
```

- 「キャンセル」は左端、「保存して閉じる」は右端という既存配置を維持する。
- 追加操作は中央に置き、保存するかどうかをラベルだけで判断できるようにする。
- 「破棄」「閉じる」だけでは失われる対象や保存の有無が曖昧なため、Issue の語句どおり「保存せずに閉じる」を使う。
- 追加の確認は出さない。このボタンを押すこと自体を明示的な破棄承認とする。

新ボタンの要素 ID／debug selector は `tab-close-confirm-discard` とします。既存の cancel／save の識別子は変更しません。

現在のボタン用 helper は `.w_full()` を持ち、ダイアログは最大幅 `420px` です。三択の横並びでは、確認ダイアログ側の幅とボタンの縮小設定を調整し、三つのラベルが省略・重なりなく表示されることを確認します。コンテキストメニュー全体の行レイアウトは変更しません。

## 4. ファイル・関数単位の作業分割

| 作業 | ファイル・対象 | 内容 |
|---|---|---|
| セッション API | `crates/session/src/session.rs`：`SessionSet::close` 周辺 | 削除処理を共用化し、`close_discarding_changes` を追加 |
| UI の閉じる処理 | `crates/ui/src/view.rs`：`close_tab_now` 周辺 | `finish_tab_close` を抽出し、新しい破棄ハンドラを追加 |
| 確認の識別 | `crates/ui/src/view.rs`：`TabCloseConfirm`、`EditorView`、`request_tab_close` | 確認の表示世代を追加 |
| 初期化 | `crates/ui/src/view/initialization.rs` | 確認世代カウンターを初期化 |
| 表示 | `crates/ui/src/view.rs`：`tab_close_confirm_element` | 中央ボタン、保存中の無効化・説明、幅調整 |
| セッションテスト | `crates/session/tests/conflict_rules.rs` | 明示的破棄と既存 close 拒否の両立を検証 |
| GPUI テスト | `crates/ui/src/view.rs` の既存テスト群 | 実描画・クリック、ディスク内容、stale click、保存予約を検証 |
| fixture 更新 | `crates/ui/src/view/clipboard.rs`、`crates/ui/src/view/app_update.rs` のテスト | 直接構築している `TabCloseConfirm` に表示世代を追加 |

`actions.rs`、保存完了処理、Save As 経路の動作変更は不要です。確認世代の追加に伴い、`view.rs` 内の既存 fixture も更新します。

## 5. テスト計画

### 5.1 追加するセッションテスト

配置：`crates/session/tests/conflict_rules.rs`

| テスト名 | 検証内容 |
|---|---|
| `explicit_discard_closes_a_dirty_session_without_writing` | dirty な対象を除去し、FileService 上の元ファイル内容が不変 |
| `explicit_discard_of_the_last_session_creates_a_clean_untitled` | 最後の対象 ID が消え、別 ID の空・clean・パスなしセッションになる |
| `explicit_discard_of_an_inactive_session_keeps_the_active_session` | 対象だけを除去し、アクティブセッションとその内容を維持 |

既存の `closing_refuses_to_drop_unsaved_work_and_never_empties_the_window` も実行し、通常の `close` が引き続き dirty を拒否することを確認します。

### 5.2 追加する GPUI テスト

配置：`crates/ui/src/view.rs`

| テスト名 | 検証内容 |
|---|---|
| `tab_close_confirm_renders_three_choices_in_order` | 実描画で三つの selector が存在し、左右の座標が設計順になり、クリック領域が重ならない |
| `middle_click_dirty_tab_discard_and_close_keeps_file_unchanged` | 実ファイルを編集して確認を開き、破棄ボタンをクリック。対象が閉じ、ディスク内容は元のバイト列のまま |
| `discard_and_close_an_untitled_tab_does_not_prompt_save_as` | 新規未保存タブを閉じても、Save As が開かず、ファイルが生成されない |
| `discard_and_close_an_inactive_tab_keeps_the_active_document` | 中クリック／メニューが指した背景タブだけを閉じ、前面文書・選択を維持 |
| `discard_and_close_clears_pending_save_close_for_its_target_only` | 対象の閉じる予約と確認を除去し、他タブの予約を維持 |
| `discard_and_close_is_disabled_while_a_save_is_in_flight` | 保存を保留できる fixture で、新ボタンが無効。直接ハンドラを呼んでも閉じず、確認を維持 |
| `discard_and_close_invalidates_pending_autosave_and_title_sync` | close 前にタイマーを設定し、close 後に満了させても元ファイルを更新・生成しない |

ファイル不変テストでは autosave を明示的に無効にして通常操作を検証します。タイマーとの競合は最後の専用テストで、進行中保存は別テストで検証します。

### 5.3 stale click の追加テスト

| テスト名 | 検証内容 |
|---|---|
| `confirm_discard_and_close_tab_ignores_a_stale_document_instance` | 確認を残したまま `adopt` で文書を置換。旧クリックで置換後文書を閉じない |
| `confirm_discard_and_close_tab_ignores_a_stale_snapshot_after_id_reuse` | 同じ ID・異なる instance の新確認を作り、旧クリックが新文書・確認・予約を変更しない |
| `confirm_discard_and_close_tab_ignores_a_stale_snapshot_after_reprompting_the_same_document` | 同じ文書の確認を取り消して再表示。表示世代の違いで旧クリックを拒否 |
| `tab_close_confirm_buttons_ignore_stale_snapshots_after_reprompting_the_same_document` | 保存・キャンセル・背景クリックにも表示世代の比較が効き、新しい確認を保存・解除しない |

これらはハンドラを直接呼び、描画側の stale cleanup が先に働かなくても安全であることを検証します。通常のボタン接続は実クリックテストで確認します。

### 5.4 従来動作の回帰確認

以下の既存テストを維持・実行します。

- `middle_click_on_dirty_tab_prompts_and_cancel_leaves_it_open`
- `middle_click_dirty_tab_save_and_close_writes_then_closes`
- `middle_click_dirty_tab_save_and_close_failure_leaves_it_open`
- `middle_click_close_save_as_cancel_for_an_untitled_note_leaves_it_open`
- `middle_click_closes_a_clean_non_active_tab_without_switching_active_session`
- `secondary_w_closes_only_the_selected_tab`
- `secondary_w_while_a_tab_close_confirm_is_open_closes_nothing`
- `context_menu_close_item_on_an_unsaved_tab_keeps_it_until_confirmed`
- 保存中の編集・外部変更・競合、Save As の旧応答に関する既存テスト
- 保存・キャンセルの既存 stale snapshot テスト
- `tab_close_confirm_overlay_blocks_clicks_to_background_elements`

実行は `hane-session` の関連テスト、`hane-ui` の対象テストから始めます。ラベルそのもの・文字切れ・日本語の見やすさは、対象 head の Debug build による macOS／Windows の表示確認で補います。保存先ダイアログの従来動作も同じ focused scenario で確認します。

## 6. 受入条件チェックリスト

実装後、次を満たしたことを evidence で確認します。

- [ ] 未保存タブの確認に「キャンセル」「保存せずに閉じる」「保存して閉じる」が表示される。
- [ ] 表示順とラベルが設計どおりで、文字切れ・重なりがない。
- [ ] 保存処理のない状態で「保存せずに閉じる」を選ぶと、対象タブが閉じる。
- [ ] 破棄操作では元ファイルの内容を変更せず、新規ファイルも作らない。
- [ ] 破棄操作で追加確認・Save As を開かない。
- [ ] 最後のタブを閉じた場合は、従来どおり空の `Untitled` を表示する。
- [ ] 背景タブを破棄しても、別のアクティブ文書を変更しない。
- [ ] 「保存して閉じる」は保存成功後に閉じ、失敗・競合・保存中の追加編集では誤って閉じない。
- [ ] キャンセル・Escape・背景クリックは、未保存内容を保持する。
- [ ] 未変更タブは従来どおり確認なしで閉じる。
- [ ] 中クリック・コンテキストメニュー・Cmd/Ctrl+W が共通の確認を通る。
- [ ] 文書置換・ID 再利用・同じ文書への確認再表示後の旧クリックが、新しい確認や文書を操作しない。
- [ ] 破棄成功後に対象の保存後クローズ予約が残らない。
- [ ] 保存中は破棄を受け付けず、その状態を画面に表示する。描画とクリックの間に保存が始まってもハンドラが再検査する。
- [ ] 閉じた文書の待機タイマーや旧 Save As 応答から、新しい保存や別文書の close が発生しない。
# 司令塔の確定事項（2026-11-13、設計ドラフトへの追記）

- **Jev 判定**: 全体判断 noul 0.71（妥当）、専用述語の方針 noul 0.83（妥当）、検索 reveal 抑制は本 Issue に含める（choice include、信頼度 0.96）。
- **ポインター表示は分離**: 設計ドラフト §6（手形ポインター）と、それに伴う `crates/presentation/src/layout.rs` の範囲抽出・`crates/ui/src/view/render.rs` の変更・関連テスト（`task_checkbox_bounds_match_hit_testing`、`reading_checkbox_pointer_region_matches_press_region`）は本 Issue の実装範囲から外し、フォローアップ Issue #717 とした（Jev: follow_up、信頼度 0.56 / 確率 0.71）。実装者はこの部分を実装しないこと。
- 上記以外はドラフトの設計どおりとする。特に §2 の検索再走査 reveal 抑制は受入条件「切替でスクロールが動かない」の必須要素として範囲内である。

---

# Issue #716 実装設計書：閲覧モードでのタスクチェック切替

## 概要

`body_editable()` は引き続き「編集モードで一般の本文編集を許可する」述語とし、チェックボックス切替にだけ専用の許可述語を設ける。

切替は既存の `plan_task_toggle` → `replace_range_recorded` → `after_input` を利用する。これにより、1回の切替を1つの Undo 単位として記録し、dirty・再解析・保存へ反映する。閲覧モードの Undo / Redo と、その他の本文編集は引き続き拒否する。

ただし、`after_input` が予約する文書内検索の再走査には、完了後に検索一致へスクロールする処理がある。**既存のスクロール復元だけでは受入条件を保証できないため、チェックボックス切替時に限って、この自動移動も抑制する。**

調査対象：

- ローカル main：`27b5ebc2e04d4c25ec30aca624fe0e9fe4b106ba`
- GitHub main：[`b06d8648375c3fd76b7c1c8d6b2e7e01edb0cd1b`](https://github.com/hide212131/hane/commit/b06d8648375c3fd76b7c1c8d6b2e7e01edb0cd1b)

ローカルには #704 の実装がなかったため、a11y と最新の表示・入力経路は GitHub main も確認した。コード変更・テスト実行は行っていない。

## 現状の経路整理

### チェックボックスの経路

```text
行の MouseDown
  → on_row_mouse_down
  → press_task_checkbox
      左ボタン・修飾キーなし・編集可能・本文IMEなし
  → 描画済みlayoutから対象を取得
  → toggle_task_checkbox
      編集可能・本文IMEなし
      session / revision を照合
  → plan_task_toggle
      parser由来のタスク・marker_range・checked・sourceを照合
  → replace_range_recorded
  → after_input
  → scroll_y / pending_caret_visibility_after_layout を復元
```

成功時は行の通常クリック処理より先に return するため、キャレット移動や選択ドラッグを開始しない。

### 編集経路ごとの点検表

| 経路 | 現在の拒否点・制御 | 本設計での扱い |
|---|---|---|
| チェックボックスのプレス | `task_checkbox.rs::press_task_checkbox` の `body_editable()` | 専用述語へ変更 |
| チェックボックスの置換適用 | `task_checkbox.rs::toggle_task_checkbox` の `body_editable()` | 専用述語へ変更。stale 検証は維持 |
| 通常入力・改行 | `view.rs::insert_text` / `insert_newline` | `body_editable()` による拒否を維持 |
| Insert・削除・Undo / Redo | `view.rs::dispatch` と `reading_mode.rs::command_changes_text` | 拒否を維持。コマンド分類は変更しない |
| OS の確定文字列・遅延コールバック | `input.rs::replace_text_in_range` | 本文の `body_editable()` 検査を維持 |
| IME 更新・確定 | `replace_and_mark_text_in_range` / `unmark_text` | 既存の本文ゲートを維持 |
| 本文への入力ハンドラ登録 | `input_routing.rs::body_input_capture_enabled` | 閲覧モードでは引き続き無効 |
| Enter・Backspace・Indent 等のリスト編集 | `view.rs::apply_list_edit_intent` | 閲覧モードでは handled no-op。通常入力への fallback も実行しない |
| Cut | `actions.rs` の本文ゲート | クリップボード書込み前の拒否を維持 |
| 通常・プレーンテキスト貼付 | `view/clipboard.rs::body_paste_ready` | 開始時・遅延結果の適用時とも既存ゲートを維持 |
| 画像貼付 | `view/image_paste.rs` の編集可能性検査 | 開始・適用側の拒否を維持 |
| 文書内検索置換 | `document_find/replace.rs` の開始・適用・表示用ゲート | すべて維持 |
| AI 置換 | `ai_selection.rs` の適用時ゲート | 遅延結果を含めて拒否を維持 |
| コンテキストメニューからの編集 | `editor_context_menu.rs` の状態取得・再検証・適用ゲート | 維持 |
| Mermaid ソースへの挿入 | `mermaid_diagrams.rs` の本文ゲート | 維持 |
| 移動・選択・コピー・検索 | 本文変更とは別の操作 | 従来どおり許可 |
| 保存 | Save アクション → `save_or_prompt` → session 保存 | モードゲートなし。既存経路を利用 |
| a11y の Click | 最新 main の `a11y_links.rs` | リンク起動のみ。チェックボックス操作経路なし |

専用述語はチェックボックスの操作・見た目にだけ使用し、上表の一般編集ゲートへ流用しない。

## 設計

### 変更ファイルと変更内容

| ファイル | 変更内容 |
|---|---|
| `crates/ui/src/view/task_checkbox.rs` | 専用許可述語、press / toggle のゲート変更、切替後の検索自動移動抑制呼出し、回帰テスト |
| `crates/ui/src/view/reading_mode.rs` | 「本文変更をすべて拒否」とする説明を例外に合わせて修正。一般編集の既存テストの説明・名称を整理 |
| `crates/ui/src/view/document_find.rs` | 切替後の検索再走査について、自動 reveal だけを抑制する局所メソッドを追加 |
| `crates/presentation/src/layout.rs` | チェックボックスの横方向の当たり範囲を取得する処理を抽出し、クリック判定とポインター表示で共有 |
| `crates/ui/src/view/render.rs` | 操作可能なチェックボックスの範囲だけにポインター指定を追加 |
| `crates/ui/src/view/session_save.rs` | 閲覧モードでの切替後保存のテスト追加。製品の保存処理は変更不要 |
| `crates/ui/src/view.rs` | 最新 main にある a11y テスト群へ、切替による旧リンク要求の失効テストを追加 |

`crates/editor` と `plan_task_toggle` の製品実装は変更しない。

### 切替処理の順序

1. 専用述語で許可を確認する。
2. クリック条件と描画済み対象を確認する。
3. session・revision・マーカー・checked を既存処理で再検証する。
4. 現在選択を保持した置換計画を作る。
5. `scroll_y` と `pending_caret_visibility_after_layout` を保存する。
6. `replace_range_recorded` で1回適用する。
7. 成功時に `after_input` を1回呼ぶ。
8. 文書内検索の後発の自動移動を抑制する。
9. 保存したスクロールとキャレット追従要求を復元する。
10. 成功を返し、通常の選択操作へ進ませない。

置換失敗時はエラーを報告し、成功扱いの return と後処理を行わない。

## 判断と理由

### 1. 例外ゲートの配置

`task_checkbox.rs` に、次の意味を持つ `task_checkbox_toggle_allowed()` を追加する。

```rust
matches!(self.editor_mode(), EditorMode::Editing | EditorMode::Reading)
    && self.editor().ime().is_none()
```

`press_task_checkbox` と `toggle_task_checkbox` の両方で使用する。

- press 側は、クリックを通常の選択操作より先に受理する。
- toggle 側は、直接呼出しでも同じ制約を守る。
- 将来モードが増えた場合も、列挙していないモードへ自動的に許可を広げない。
- 左ボタン・修飾キーなしの条件は press 側に残す。
- session / revision / marker / checked の照合は許可述語と分離して維持する。

`body_editable()` の戻り値、`command_changes_text`、入力先判定は変更しない。モードを一時的に Editing にして適用する方法も採用しない。

### 2. `after_input` の安全性

最新 main の [`view.rs::after_input`](https://github.com/hide212131/hane/blob/b06d8648375c3fd76b7c1c8d6b2e7e01edb0cd1b/crates/ui/src/view.rs#L2611) を確認した。

| 処理 | 評価・対応 |
|---|---|
| 編集保護状態の同期・block index 更新・再解析予約 | 実際の本文変更に必要。そのまま実行 |
| disclosure の高さ更新 | 閲覧モードでは `active_height_disclosure()` が `None`。選択による記法展開は起こさない |
| `scroll_cursor_into_view()` | スクロールを変更する。既存の保存・復元を維持 |
| `pending_caret_visibility_after_layout = true` | 後続フレームの追従を起こし得る。既存の保存・復元を維持 |
| `cancel_center_caret()` | 古い中央表示要求を破棄する既存挙動を維持 |
| autosave・draft save・title sync | 通常編集として必要。予約処理を維持 |
| 検索結果・AI 結果の無効化 | revision 更新に伴って必要 |
| IME の開始・確定・取消 | `after_input` 自体にはない |

なお、`replace_range_recorded` は内部で `commit_composition()` を呼ぶ。このため、**本文 IME 中の拒否は両モードで維持する必要がある**。通常のモード切替も本文 composition 中は拒否される。

追加対応が必要なのは文書内検索である。

```text
after_input
  → resync_document_find_after_edit
  → restart_document_find(..., reposition = true)
  → 非同期検索完了
  → reveal_current_find_match
  → スクロール変更
```

[`document_find.rs`](https://github.com/hide212131/hane/blob/b06d8648375c3fd76b7c1c8d6b2e7e01edb0cd1b/crates/ui/src/view/document_find.rs#L579) のこの経路は、切替直後の `scroll_y` 復元より後に実行される。

対策として `suppress_document_find_reveal_after_task_toggle()` を同モジュールに追加し、`after_input` 直後、同じ同期更新内で呼ぶ。

- 今回の再走査の `reposition_on_next_result` を `false` にする。
- `pending_find_visibility_after_layout` を解除する。
- 検索の再走査・結果更新・古い置換計画の失効は継続する。
- その後の明示的な Next / Previous やクエリ変更は、従来どおり移動できる。

既存の `reposition = false` は結果の `current` を `None` にする仕様なので、切替後は一致一覧を更新し、明示的な検索移動で current を再設定する。通常入力の検索挙動は変更しない。

### 3. 現在選択の扱い

現在の `Selection { anchor, active }` をそのまま渡し、返された `selection_after` を使用する。

[`plan_task_toggle`](https://github.com/hide212131/hane/blob/b06d8648375c3fd76b7c1c8d6b2e7e01edb0cd1b/crates/markdown/src/list_editing.rs#L1930) は、選択範囲を置換対象の決定や適用可否に使用しない。置換対象は検証済みマーカーの中央1バイトだけであり、渡した選択をそのまま返す。

したがって、次の状態でも選択を縮めたり、選択全体を置換したりしない。

- キャレットだけ
- 順方向・逆方向の選択
- マーカーを含む選択
- 複数項目・複数ブロックをまたぐ選択

ASCII の1バイトを同じ長さで置換するため、文書長と選択の byte offset は変わらない。閲覧モードの選択を一時的なキャレットへ変換する必要はない。

### 4. a11y 経路

**チェックボックス操作経路はない。**

最新 main の [`a11y_links.rs`](https://github.com/hide212131/hane/blob/b06d8648375c3fd76b7c1c8d6b2e7e01edb0cd1b/crates/ui/src/view/a11y_links.rs#L520) は、`Role::Document` と可視リンクの `Role::Link` を公開する。合成ノードの `Action::Click` は `activate_a11y_link` に接続され、URL を開く。チェックボックスの role・checked 属性・切替 action は登録していない。

本 Issue ではチェックボックスの a11y 公開を追加しない。

一方、チェック切替は本文 revision を進めるので、既存仕様どおり旧リンクノードの要求は失効しなければならない。既存の内容世代検査で成立することを回帰テストする。

### 5. 閲覧モードでの保存

保存可能であり、製品処理の変更は不要。

`actions.rs` の Save は `secondary-s` に割り当てられ、`save_or_prompt` を呼ぶ。`save_current` → `save_active` → `save_session` に `body_editable()` の条件はない。

- 保存先あり：閲覧モードのまま通常保存できる。
- 保存先なし：既存の Save As へ進む。
- 自動保存有効時：`after_input` による既存の予約を利用する。

設定画面・外部比較・リネーム中などの既存制約は維持する。dirty のテストでは自動保存を無効にし、保存完了前の状態を確認する。

### 6. ホバー時の見た目

チェックボックス上には手形ポインターを追加する。

現状の本文行・チェックボックスには専用のカーソル指定がない。最新 main の `link_hover.rs` はリンクの色を変える処理であり、チェックボックスの操作可能性を示さない。

実装は、状態を保持する新しい hover 管理機構を作らず、描画フレームごとの範囲に限定する。

1. `BlockLayout::task_checkbox_at_x` 内の範囲計算を `task_checkbox_bounds_on_row` に抽出する。
2. クリック判定もその戻り値を使用する。
3. `render.rs` で同じ範囲に透明な絶対配置要素を置き、`.cursor_pointer()` を指定する。
4. 要素にはクリックハンドラ・独立 focus・a11y action を付けず、既存の行イベントへ伝播させる。

横幅はグリフの当たり範囲、縦幅は対象行に合わせる。通常の本文・余白・折返し後の行・編集モードで source が開示された `[ ]` には付けない。本文 IME 中など切替不可の状態では付けない。

## テスト計画

以下の新規テスト名は実装時の予定名。

### `crates/ui/src/view/task_checkbox.rs`

| テスト名 | 確認内容 |
|---|---|
| `reading_press_toggles_once_and_marks_dirty` | 実描画のグリフ位置へ mouse down / up。両方向の切替、revision 更新、dirty、release・再描画で再切替しないこと |
| `reading_toggle_keeps_general_body_edits_blocked` | 切替後も Insert・改行・削除・リスト編集・Undo / Redo が本文を変えない。`body_editable()` と本文入力 capture は false |
| `reading_toggle_is_undoable_after_returning_to_editing` | 閲覧中の Undo は無効。正式なモード切替後、Undo で切替だけを戻す。`[X]` の復元、Redo、隣接入力との Undo 単位分離も確認 |
| `reading_toggle_preserves_directional_selection` | キャレット、順逆選択、マーカー内端点、全選択、複数ブロック選択で anchor / active を保持 |
| `reading_press_preserves_viewport_after_layout_and_parse` | 画面外に選択端点を置き、スクロール後にチェック。直後・再描画・背景解析完了後も位置保持、キャレット非表示 |
| `composition_blocks_task_toggle_in_both_modes` | 本文 composition がある状態では両モードで拒否し、本文・選択・IME を保持 |
| `reading_task_targets_are_revalidated` | 古い revision、別 session、不正マーカー、誤 checked を拒否 |

既存 `reading_mode_and_composition_never_toggle` は新仕様と矛盾するため置換する。実プレスのテストを必須とし、`toggle_task_checkbox` の直接呼出しだけで受入を済ませない。

既存のドラッグ、本文クリック、開示済みマーカーのテストも維持する。ドラッグ終点がチェックボックス上でも切り替わらないことは閲覧モードでも確認する。

### `crates/ui/src/view/document_find.rs`

追加：

- `task_toggle_rescans_find_without_revealing_a_match`

検索バーを開き、遠方の一致とは別の位置までスクロールしてから切り替える。debounce・走査・再描画が完了してもスクロールが変わらず、一致一覧は新しい本文へ更新されることを確認する。走査中に再度切り替える場合と、古い find visibility 要求がある場合も含める。

その後の明示的な Next / Previous は正常に移動することを確認する。編集・閲覧の両モードを対象にする。

### `crates/ui/src/view/reading_mode.rs`

既存テストの影響：

- `reading_refuses_every_body_change_but_allows_movement`  
  一般編集の拒否アサーションは維持。名称を `reading_refuses_general_body_changes_but_allows_movement` に変更し、チェックボックス例外を説明する。
- `late_os_text_callbacks_do_not_change_a_read_only_body`  
  維持し、チェック切替後にも OS 入力・IME 更新・unmark が本文を変えないケースを追加する。
- `reading_cut_and_paste_leave_the_body_and_the_clipboard_alone`
- `undo_and_redo_survive_a_mode_round_trip`

後二つも維持する。貼付・画像貼付・検索置換・AI 置換の既存拒否テストを関連回帰として実行する。

### 保存・a11y・ポインター

| ファイル | テスト名 | 確認内容 |
|---|---|---|
| `view/session_save.rs` | `reading_task_toggle_can_be_saved_without_switching_modes` | 一時ファイルを開き切替。Cmd/Ctrl+S を送信し、保存 bytes、dirty 解消、Reading 維持を確認 |
| `view.rs` | `reading_task_toggle_invalidates_old_a11y_link_requests` | 切替前のリンク要求を再描画前から拒否し、新フレームの要求は有効であること |
| `presentation/src/layout.rs` | `task_checkbox_bounds_match_hit_testing` | 範囲の左右境界、折返し、ネスト・引用、zoom、開示済み source の除外 |
| `view/task_checkbox.rs` | `reading_checkbox_pointer_region_matches_press_region` | ポインター用領域と実プレス領域の一致、隣接本文では選択操作になること |

### 実行・実機確認

実装後はまず `hane-ui` の関連テストと `hane-markdown` の既存 task toggle テストを実行し、fmt・Clippy・必要な CI を確認する。

macOS / Windows の Debug / development build で、手形表示、両方向の切替、長文での位置保持、入力拒否、保存、編集モード復帰後の Undo を確認する。Windows は接続済み Windows 環境から直接操作する。

## 対象外

- 閲覧モードでの Undo / Redo 解禁
- チェックボックス以外の本文編集解禁
- チェックボックスの a11y 合成ノード・キーボード操作追加
- 読み取り専用ファイルという別概念の追加
- Undo・保存形式・IME の新しい仕組み
- 全域リファクタリング

## リスク・未確認点

- **検索完了後のスクロールは具体的な既存経路がある。** 対策と非同期完了後のテストを本 Issue の実装範囲に含める。
- 高さ再測定・モード切替直後のアンカー要求との組合せは実行未確認。安定後のクリックだけでなく、保留要求がある状態もテストする。
- ポインター用の透明要素がイベント伝播を妨げないこと、OS 上の形状と境界は実装後に確認する。
- #704 はリンクのみの公開であり、本変更後も支援技術からチェックボックスを切り替えられるとは説明しない。
- 全域リファクタリングや #704 全体の完了待ちは不要。実装開始時に current main と、チェックボックス・検索・描画の同じ責務を変更する進行中 PR を再確認する。
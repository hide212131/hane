# Issue #737 実装設計書: プレーンテキストモード

> 位置づけ: 採用済み設計（2026-11-17、Claude Fable 5.2 による設計を Commander が検分し採用）。本書が本 Issue 実装の正本であり、判断に迷う場合は本書の決定表と仕様表に従うこと。未実装・未検証の準備文書であり、製品コードの現状説明ではない。

- 対象 Issue: #737「feat(editor): プレーンテキストモード（普通のエディタのような振る舞い）」
- 関連 Issue: #738（Markdown ソースのハイライト）
- 調査対象: origin/main = `6d45d29a`（v0.47.0）
- この文書の行番号はすべて上記コミット時点のもの。実装時にずれていたら関数名で探すこと。
- 「未確認」と書いた箇所は、コードを読み切れていない。実装前に確認が要る。

---

## 0. 結論（先に読む）

| 論点 | 決定 |
| --- | --- |
| モードの定義 | 投影（マーカー隠蔽・ブロック変換・開示）を一切行わず、ソース 1 物理行 = 表示 1 行（＋ソフトラップ）で表示・編集する |
| モデル | `EditorMode`（編集可否）とは**直交する軸**として追加する。第 3 の `EditorMode` にはしない |
| 切替単位 | アプリ全体で 1 値（全タブ共通） |
| 永続化 | `settings.conf` に `plain_text_mode=true\|false`。欠損は `false` |
| 切替時の保全 | 既存の `rebuild_presentation_for_mode_change()` と `ModeAnchor` を流用。Editor（本文・選択・Undo・dirty）には触れない |
| 編集 | Enter / Shift+Enter / Backspace は素の編集。リスト編集 intent は通さない |
| Tab | フェーズ 3 で空白インデントを実装。それまでは現状どおり無動作 |
| 貼り付け | ⌘V はプレーンテキスト貼り付けと同じ動作 |
| 保存 | モードは `ui` crate の view だけが知る。`session` / `editor` / 保存経路は変更しない |
| #738 | 新設する `present_block_plain` の `style_runs` が唯一の注入点 |

最重の不変条件は次の 3 つ。すべてテストで固定する（6 章）。

1. 切替の前後で `(Revision, 本文長, dirty, can_undo)` と本文バイト列が変わらない。
2. 切替の前後でキャレットと選択のソースオフセットが変わらない。
3. プレーン表示中、各行の `visual_text` はソース行から行末の `\r` `\n` を除いたものと完全に一致し、SourceMap は恒等である。

---

## 1. 目的と範囲

### 1.1 目的

Markdown の投影表示（WYSIWYG 風）をやめ、ファイルの中身をそのまま見て編集できる表示を提供する。記号が隠れる・行の高さが変わる・Enter でリストが続く、といった「エディタが気を利かせる」挙動を止めたい利用者向け。

### 1.2 やること

- プレーンテキスト表示の投影経路（ソースをそのまま 1 行ずつ出す）
- 表示切替の UI（タブ行のトグルボタン、設定画面のチェックボックス）
- 切替状態の永続化
- プレーン表示中の編集挙動の定義（Enter、Backspace、Tab、貼り付け、IME）
- Markdown 前提機能の機能ごとの扱いの定義と実装
- 切替時の本文・キャレット・選択・スクロール・Undo の保全

### 1.3 やらないこと

- 行番号・ガター（現行に存在しない。別 Issue）
- 等幅フォントへの切替（8 章 U-3。既定は本文フォントのまま）
- シンタックスハイライト（#738。接続点だけ用意する）
- `.md` 以外のファイルを開けるようにすること
- 自動インデント（前行のインデントを引き継ぐ Enter）
- CRLF 文書での改行コード整合（8 章 U-6。両モード共通の既存課題）
- タブ単位・ファイル単位のモード記憶（8 章 U-1）
- 既存の投影経路・高さ算術のリファクタリング（機能追加と同じ PR に混ぜない）

### 1.4 着手メモ（docs/refactor-execution-plan.md §2.1 用）

- 変更する責務: `ui` の表示ポリシー（`DisclosurePolicy`）と高さ見積り、`presentation` の投影入口、`session` の `Settings`。
- 関連 Issue: #738。
- 先行条件: 依存なし。`Editor` / `DocumentSession` / 保存経路は変更しない。高さ算術は既存関数に分岐を足すだけで、責務の移動は行わない。
- 使う経路と検証: 受入済みの `presented_block` → `layout_block` 経路。検証は 6 章。
- 変更前測定: 6.4 節。

---

## 2. 現行処理の調査

### 2.1 crate 構成とデータフロー

```text
DocumentSession → Editor → BlockIndex → VisualBlock → BlockLayout → LayoutLine → TextRun
   (session)      (editor)  (markdown)  (presentation)      (presentation)        (ui)
```

- `RopeBuffer` が本文の唯一の正本。
- `Editor` はソースオフセットだけを扱い、表示モードを知らない（`EditorCommand`、`insert_text` L350、`replace_range_recorded` L359、`selected_text` L677。いずれも `crates/editor/src/lib.rs`）。
- 上下移動は `Editor::move_vertical_to(target, extend, preferred_x)`（`crates/editor/src/lib.rs` L241-254）。行の位置を知るのはレイアウトだけ、という前提で view が移動先オフセットを計算して渡す。`preferred_visual_x` が列保持を担う。

**帰結**: 表示モードを変えても Editor の状態は 1 ビットも変わらない。プレーンモードは `presentation` と `ui` だけで実現できる。

### 2.2 表示モードの既存モデル

`crates/ui/src/view/reading_mode.rs`

| 行 | 内容 |
| --- | --- |
| L16-21 | `pub(crate) enum EditorMode { #[default] Editing, Reading }` |
| L37-47 | `ModeAnchor { session, instance, revision, offset, inside_row, attempts }` |
| L64 | `editor_mode()` |
| L71 | `body_editable()`（`editor_mode == Editing`） |
| L77 | `body_caret_visible()` |
| L86 | `reading_visible_row(offset)` |
| L101-106 | `disclosure_policy()`（`EditorMode` → `DisclosurePolicy` の 1:1 写像） |
| L110 | `editor_mode_switch_blocked()`（`self.editor().ime().is_some()`） |
| L121-148 | `set_editor_mode`。同値なら `true`。IME 合成中は `MODE_SWITCH_BLOCKED_BY_COMPOSITION` を status に出して `false`。`cancel_text_selection_autoscroll()`、Reading へは `close_ai_selection`、`rebuild_presentation_for_mode_change()`、notify |
| L154-187 | `rebuild_presentation_for_mode_change()`（下記） |
| L192 / L202 / L253 | `mode_anchor_is_current` / `capture_mode_anchor` / `resolve_mode_anchor`（最大 3 回試行） |
| L307-353 | `reading_mode_button(cx)`。`Button::new("reading-mode-toggle")`、`.disabled(blocked)`、`HoverCard` ツールチップ |

`rebuild_presentation_for_mode_change()` は次を行う。コメントに「without touching the document, its history, the selection or the tabs」とある。

- `pending_mode_anchor` を capture（既存のものが current なら保持）
- `mode_generation` を加算
- `block_cache` / `line_owners` / `layout_cache` を clear
- `caret_geometry = None`、`pending_caret_visibility_after_layout = false`、`cancel_center_caret()`
- `last_background_height_disclosure = None`、`force_height_disclosure_snapshot = false`
- `height_blocks.content_width_bits = None`
- `install_heights(granularity, HeightIndex::new(self.item_heights()))`

`EditorMode` はウィンドウ全体で 1 値、非永続（`crates/ui/src/view/initialization.rs` L220 で `EditorMode::default()`）。session・editor・settings は知らない。

`ModeAnchor` のフィールドは `EditorMode` に依存しない。したがって別軸の切替からそのまま再利用できる。

### 2.3 projection と SourceMap

`crates/ui/src/line.rs`

- L124-129 `DisclosurePolicy { Editing, Reading }`
- L132-143 `BlockPresentationRequest`（`forced_disclosure`、`disclosure_policy` を含む）
- L172-223 `presented_block`、L233-247 `presented_block_with_diagram`
- L253 `lines_are_disclosed`（Reading は常に false）
- L284 `expected_block_disclosures`
- L360-486 `block_context`（joinable ブロックの span 読み、`range_disclosure`、fence の zero-height 行数算出）
- L600-621 `styled_block_with_surface`（`display.monospace` なら `font_family("ui-monospace")`）
- L741 `row_element`（Diagram 分岐、quote bar、画像など）

`crates/presentation/src/projection/mod.rs`

- L34-64 `present_plain`（恒等 SourceMap、単一 Visible セグメント）
- L72-83 `fn present_raw_source(line_id, revision, range, source, line_height) -> VisualLine`（**private**）。`kind = BlockKind::Unsupported`、高さは `line_height`
- L280 `table_delimiter_is_collapsed`

`crates/presentation/src/projection/block.rs`

- L4 `block_line_span`、L20 `trailing_blank_lines`
- L53-64 `block_heights_with_disclosure`、L70-105 `block_height_with_disclosure`（畳む行を引き、先頭行は `estimated_height(block_display_kind(block.kind), line_height)`、最終ブロックは `trailing_empty_lines` を加算）
- L210 `present_block_with_table_projection`
- L403-405 各行の提示後に `while presented.visual_text.ends_with(['\r','\n']) { pop }`
- 末尾で `VisualBlock { id, kind: block_display_kind(block.kind), source_range, revision, confidence, span, lines, table_projection, lines_before, lines_after, zero_height_lines_before, zero_height_lines_after, line_height }` を組む

`crates/presentation/src/display.rs`

- L480-521 `VisualLine` のフィールド（`visual_text`、`style_runs`、`links`、`kind`、`source_map`、`estimated_height`、`image`、`list`、`quote`、`table_row` など）
- L674 `VisualBlock::matches(block)`（`id`、`source_range`、`confidence`、**`kind == block_display_kind(block.kind)`**）
- L740 `covers`、L750 `rebase`
- L871 `estimated_height`（Heading1 = 1.65 倍、Heading2 = 1.45 倍、Heading3 = 1.25 倍、Image = 190 × line_height / 26、TableDelimiter = 0、他 = line_height）

`crates/presentation/src/layout.rs`

- L982 `layout_block`。`block.kind == BlockKind::TableRow` または `table_row.is_some()` の行があれば `layout_table_block` へ入る。通常経路は折り返し（`fragment_boundaries`）と `LayoutLine` 生成
- L773 `vertical_target`、L744 `task_checkbox_at_x`

`crates/ui/src/shape.rs` L60-100: フォントは `style_runs` 由来の `inline.monospace` と、行の `display().weight` で決まる。

**帰結**

- 「ソースをそのまま出す 1 行」を作る関数（`present_raw_source`）は既にある。プレーンモードはこれをブロックの全行に適用すればよい。
- `VisualBlock.kind` を元のブロック種別のままにすると、表ブロックが `layout_table_block` に入ってしまう。プレーンでは `BlockKind::Unsupported` にする必要がある。
- すると `VisualBlock::matches` の kind 比較が常に落ち、キャッシュが毎フレーム無効になる。プレーン用の一致判定が要る（4.2 節）。

### 2.4 view 側の提示・高さ・仮想化

`crates/ui/src/view.rs`（25,335 行）

| 行 | 内容 |
| --- | --- |
| L2523-2560 | `on_document_replaced()`。`granularity = Lines`、高さを行数分で再構築、`pending_mode_anchor = None`、キャッシュ clear |
| L2622-2666 | `after_input(cx)`。`block_index.apply_edits` → `apply_height_index_update`（失敗時 `resync_heights`）→ `schedule_document_parse` → `ensure_active_disclosure_height` → `scroll_cursor_into_view` → autosave ほか |
| L4233 / L4248 | `is_crossed_fence` / `collapses_when_inactive`（`DisclosurePolicy::Reading` で提示して zero-height 行を検出） |
| L4576-4620 | `neighbor_row_target`（`policy = self.disclosure_policy()`） |
| L4882 | `line_height()`（`theme.line_height * zoom`） |
| L4935-4968 | `visible_line_prefix(block, physical_line)`（畳まれた fence 行と table delimiter を引く） |
| L4976 | `physical_line_prefix_for_visible_rows` |
| L5195-5276 | `cached_block(block, visible)`。再利用条件は rebase 可、`cached.matches(block)`、`cached.covers(visible)`、`disclosures_are_current`、`diagram_plan_matches` |
| L5323 | `disclosures_are_current` |
| L5390-5405 | `neighbor_block_window_from` 内の `match policy { Editing => …, Reading => None }`（**網羅 match**） |

`crates/ui/src/view/height_sync.rs`

- L6-19 `item_heights()`
- L34 `ensure_active_disclosure_height()`（`active_height_disclosure()` が None なら早期 return）
- L336 `install_disclosure_heights_preserving_measurements`（None で早期 return）
- L439-449 `active_height_disclosure()`（`!body_editable()` なら None）
- L453 `resync_heights`、L481 `install_heights`
- L540 `apply_height_index_update`（L567 で `block_height_with_disclosure`）

`crates/ui/src/view/background_parse.rs`

- L7 `schedule_document_parse`。L54 で `self.editor_mode`、L60 で `mode_generation` を捕捉し、L81 / L148 で不一致なら結果を捨てる。L108 で `block_heights_with_disclosure` により高さスナップショットを作る
- L260 `schedule_joined_parse`

`crates/ui/src/view/mermaid_diagrams.rs`

- L98 `schedule_mermaid`、L252 `diagram_plan_for`（`lines_are_disclosed(…, self.disclosure_policy())`）、L312 `click_diagram_row`

`crates/ui/src/view/render.rs` L272 / L292 / L366 / L386: `!self.body_editable()` を「Reading で閉じた行がある」ことの代理条件として使い、`reading_visible_row` を呼ぶ。

**帰結**: 高さ算術は「開示（disclosure）」と「ブロック種別ごとの見積り」に結合している。プレーンではどちらも不要で、高さは「物理行数 × line_height」（＋折り返しの実測）になる。分岐を入れる箇所は 4.3 節に列挙する。

### 2.5 設定と永続化

`crates/session/src/store.rs`

- L52-78 `pub struct Settings { autosave, theme, default_folder, sidebar_sort, network, auto_update_check }`、L80-91 `Default`
- L460-490 `impl SettingsRepository for FileStateStore`。`store` は `format!("autosave={}\ntheme={}\nsidebar_sort={}\nauto_update_check={}\n", …)` に `default_folder` と network 行を足して `atomic_write_bytes`
- L495-527 `settings_from_read`。行ごとに `strip_prefix("autosave=")` などで読む。未知キーは無視、欠損は既定値。`auto_update_check` は `value == "true"`
- L558 `MemoryStateStore`、L868 テスト `settings_survive_a_store_reload`

`crates/ui/src/view.rs`

- L817 `settings: Settings`、L992 `settings_store_error`
- L2601-2611 `store_settings()`（失敗時は `settings_store_error` と status "Settings failed: …"）

`crates/ui/src/view/settings_screen.rs`

- L11 `set_autosave`、L586 `display_and_save_settings`（「表示」カード。Checkbox "settings-autosave"）
- L680-696 `set_theme`（キャッシュ clear → 高さ再構築 → `store_settings()` → notify）

`crates/session/src/session.rs` L20-22: `SessionViewState { scroll_y: f32 }`。セッション単位の表示状態はスクロールだけ。

`self.settings` を丸ごと差し替える代入は grep で見つからなかった（フィールド単位の更新のみ）。

複数ウィンドウを開いたとき各ウィンドウの `settings` がどう同期されるかは**未確認**（`crates/app/src/main.rs` L939 と `limited_start.rs` L125 に `open_window` がある）。

### 2.6 キーバインドと編集コマンド

`crates/ui/src/actions.rs`（キーコンテキスト `"HaneEditor"`）

| 行 | アクション | Body での現行動作 |
| --- | --- | --- |
| L359 | `Newline("enter")` | `ime().is_none() && !late_input_guard_armed() && !apply_list_edit_intent(Enter)` なら `insert_newline` |
| L394 | `ShiftNewline("shift-enter")` | リスト編集 intent 経由 |
| L427 | `Backspace` | リスト編集 intent 経由（未処理なら素の Backspace） |
| L439 / L461 | `Indent("tab")` / `Outdent("shift-tab")` | `apply_list_edit_intent` のみ。**リスト外は無動作** |
| L745 | `Copy` | `editor().selected_text()`（ソース文字列） |
| — | `Paste("secondary-v")` | `PasteMode::Rich` |
| — | `PastePlainText("secondary-shift-v")` | `PasteMode::PlainText` |

- `crates/ui/src/view.rs` L3599-3661 `apply_list_edit_intent(intent, cx) -> bool`、L3663 `insert_newline`（`EditorCommand::Insert("\n")` 固定）、L3674 `insert_text`、L3685 `dispatch`。
- `insert_newline` は LF 固定で、CRLF 文書でも LF を挿入する。これは現行の両モード共通の挙動。
- モード切替のキーバインドは現行に無い。
- 使用中のキー（プラットフォーム別の `ctrl-*` などを除く）: backspace, ctrl-shift-tab, ctrl-tab, delete, down, enter, escape, f2, f3, left, right, secondary-0, secondary-a, secondary-alt-a, secondary-b, secondary-c, secondary-f, secondary-o, secondary-s, secondary-shift-f, secondary-shift-o, secondary-shift-s, secondary-shift-v, secondary-shift-z, secondary-v, secondary-w, secondary-x, secondary-z, shift-down, shift-enter, shift-f3, shift-left, shift-right, shift-tab, shift-up, tab, up。
- `\t` を特別扱いするコードは `ui` / `presentation` / `editor` に見当たらなかった。GPUI がタブ文字をどの幅で描くかは**未確認**。

`crates/ui/src/view/clipboard.rs`

- L42 `body_paste_ready`
- L87-133 `paste_into_body(mode, window, cx)`。合成中は `COMPOSING_MESSAGE` で拒否。`mode == PasteMode::Rich && self.paste_image_from_snapshot(...)` で画像貼り付け。`decide(mode, text)` が InsertPlain / Replace / Refuse / Convert を返す
- `replace_selection_with`（1 Undo 単位）

### 2.7 Markdown 前提機能の入口

| 機能 | 場所 | モード判定 |
| --- | --- | --- |
| チェックボックス切替 | `crates/ui/src/view/task_checkbox.rs` L60-66 `task_checkbox_toggle_allowed()` | `matches!(editor_mode(), Editing \| Reading) && ime().is_none()`。コメント「Listed modes only, so a future mode is not allowed by accident」 |
| リンクを開く | `crates/ui/src/view.rs` L3903 / L4016 | Reading の無修飾クリック。`links` が空なら何もしない |
| a11y の説明文 | `crates/ui/src/view/a11y_links.rs` L200-203 | `EditorMode` で match |
| コンテキストメニュー | `crates/ui/src/view/editor_context_menu.rs` | cut / paste は `body_editable()`、リンクは `plan_link_for` |
| AI 連携 | `crates/ui/src/view/ai_selection.rs` L1194 | `!body_editable()` で適用拒否。ソース範囲基準 |
| 文書内検索・置換 | `crates/ui/src/view/document_find.rs` L398 / L415 | ソース範囲基準 |
| Mermaid 図 | `mermaid_diagrams.rs` L252 / L312 | `disclosure_policy()` / `body_editable()` |
| 画像貼り付け | `clipboard.rs` L87-133、`image_paste.rs` | `PasteMode::Rich` のとき |
| IME | `crates/ui/src/input.rs` L283 / L339 / L393 / L420 | `body_editable()` |
| 保存時の書換 | `crates/session/src/session.rs` の `arm_save_rewrite` / `RewritePlan` | 画像リネーム専用。モード依存なし |

表編集の入力経路が `VisualBlock.table_projection` 以外（`BlockIndex` 直読みなど）からも表を認識しているかは**未確認**。実装時に `table_projection` と `table_row` を grep して確認すること。

---

## 3. 期待挙動の仕様

以下「プレーン」はプレーンテキストモード ON、「投影」は OFF（現行の表示）を指す。

### 3.1 モードの組合せ

| プレーン | `EditorMode` | 表示 | 編集 |
| --- | --- | --- | --- |
| OFF | Editing | 現行どおり | 現行どおり |
| OFF | Reading | 現行どおり | 不可 |
| ON | Editing | ソースそのまま | 可（3.4 節） |
| ON | Reading | ソースそのまま | 不可。キャレット非表示。リンクは開かない |

### 3.2 表示

| 入力・状態 | 期待結果 |
| --- | --- |
| `# 見出し` | `# 見出し` がそのまま、本文と同じ文字サイズ・太さ・行高で出る |
| `**太字**`、`` `code` ``、`[t](u)` | 記号を含めそのまま。装飾なし、リンクとして反応しない |
| `- [ ] task` | そのまま。チェックボックス図形は出ない |
| `> 引用` | そのまま。引用バーは出ない |
| `![alt](a.png)` | そのまま。画像は出ない |
| 表（`\| a \| b \|` と区切り行） | 3 行ともそのまま。区切り行も 1 行分の高さで出る。列揃えなし |
| コードフェンス | ` ``` ` の行も常に 1 行分の高さで出る。等幅にしない |
| Mermaid フェンス | ソースのまま。図は出さない |
| 空行、連続空行 | 1 行ずつ 1 行分の高さ |
| 行末の空白、全角空白 | そのまま（可視化記号は出さない） |
| 行幅を超える行 | 既存と同じソフトラップ。上下移動は表示行単位 |
| タブ文字を含む行 | GPUI の既定の描画に従う（未確認。8 章 U-4） |
| 空の文書 | 空行 1 行。キャレットはオフセット 0 |
| 末尾に改行がない文書 | 最終行をそのまま表示 |
| 末尾が改行で終わる文書 | 最後に空行を 1 行表示（現行と同じ行数） |
| CRLF 文書 | `\r` は表示しない。ソースには残る |
| キャレットがある行 | 他の行と見た目が変わらない（開示の概念がない） |
| 検索ヒット | 現行と同じハイライト。現在ヒットへの移動で行の見た目は変わらない |
| 選択範囲 | 現行と同じ描画。範囲はソースオフセットと 1:1 |

### 3.3 切替

| 入力・状態 | 期待結果 |
| --- | --- |
| トグルを押す（合成中でない） | 表示が切り替わる。本文・Revision・dirty・Undo 履歴・選択・キャレットのソースオフセットは不変 |
| 切替前にキャレット行が画面内 | 切替後もキャレット行が画面内に残る。画面内の相対位置は `ModeAnchor` の精度で保つ |
| 切替前にキャレット行が画面外 | 既存の Reading 切替と同じ扱い（`capture_mode_anchor` の現行仕様に従う） |
| 投影で隠れた記号の中（例 `**` の間）にキャレット | 同じソースオフセットにキャレットが出る |
| 選択が複数ブロックにまたがる | アンカーとヘッドのオフセットが不変 |
| IME 合成中にトグル | 切り替えない。status に `MODE_SWITCH_BLOCKED_BY_COMPOSITION`。ボタンは disabled |
| 切替直後に Undo | 切替前の最後の編集が取り消される。切替自体は Undo 履歴に入らない |
| 連続入力の途中で切替 | Undo グループの扱いは変えない（Editor を触らない）。切替前後の入力が 1 グループにまとまってもよい |
| ON → OFF → ON を繰り返す | 毎回同じ表示・同じオフセット。本文不変 |
| 切替後にタブを切り替える | 新しいタブも同じモードで表示 |
| 切替後にファイルを開く・再読込 | 同じモードで表示 |
| 背景パースが走っている最中に切替 | 古い結果は `mode_generation` 不一致で捨てられる |
| プレーン ON のまま Reading に入り、戻る | プレーンのまま |
| アプリ再起動 | 前回のモードで起動 |
| `settings.conf` の保存に失敗 | セッション内では切り替わる。既存の `settings_store_error` 表示 |
| `settings.conf` にキーが無い・値が不正 | OFF |
| 文書を開いていない状態でトグル | 設定だけ変わる。クラッシュしない |

### 3.4 編集（プレーン ON かつ Editing）

| 入力・状態 | 期待結果 |
| --- | --- |
| Enter（リスト項目の中） | `\n` を 1 つ挿入するだけ。マーカー継続・番号振り・空項目の解除をしない |
| Enter（どこでも） | `\n` を 1 つ挿入。インデントを引き継がない |
| Shift+Enter | Enter と同じ |
| Enter（IME 合成中） | 現行どおり IME が処理（本文に改行を入れない） |
| Enter（`late_input_guard_armed()`） | 現行どおり無視 |
| Backspace（リストマーカー直後、インデント内） | 1 書記素だけ削除。マーカー解除・アウトデントをしない |
| Backspace（選択あり） | 選択を削除 |
| Delete | 現行どおり |
| Tab（フェーズ 1〜2） | 現行のリスト外と同じく無動作 |
| Tab（フェーズ 3、選択なし） | キャレット位置に半角空白 4 個を挿入 |
| Tab（フェーズ 3、複数行選択） | 選択がかかる各物理行の行頭に空白 4 個。1 Undo 単位 |
| Shift+Tab（フェーズ 3） | 対象各行の行頭から空白最大 4 個、または `\t` 1 個を削除。無ければ無変更。1 Undo 単位 |
| ⌘V（テキスト） | プレーンテキストとして挿入。HTML → Markdown 変換をしない |
| ⌘V（URL、選択あり） | リンク化しない。選択を URL 文字列で置換 |
| ⌘V（画像のみ） | 何も挿入しない。画像ファイルも作らない（8 章 U-5） |
| ⌘⇧V | 現行どおり |
| ⌘C / ⌘X | 現行どおり（ソース文字列） |
| IME 合成・確定・キャンセル | 現行どおり。合成中の下線位置は恒等写像で決まる |
| 文字入力（`*`、`` ` ``、`[` など） | その文字だけが入る。現行に自動補完が無いことは view の入力経路で確認済みの範囲に限る（`insert_text` L3674 は素の挿入） |
| ←→ | 1 書記素ずつ。隠れた記号を飛ばす概念がない |
| ↑↓ | 表示行単位。`preferred_visual_x` で列を保持 |
| Home / End | 物理行の先頭・末尾（現行と同じ `MoveToLineStart/End`） |
| クリック・ドラッグ | クリック位置のソースオフセット。チェックボックス・リンク・図に反応しない |
| ダブルクリック | 現行の語選択（`on_row_mouse_down` の処理は未確認。恒等写像なので語境界はソース基準になる想定） |
| ⌘Z / ⌘⇧Z | 現行どおり。投影モードで行った編集もプレーンで取り消せる。逆も同じ |

### 3.5 機能別の扱い

| 機能 | プレーンでの扱い | 理由 |
| --- | --- | --- |
| 表編集（セル移動・列揃え表示） | 無効。素のテキスト | 表の投影が無い |
| 画像の表示 | 無し | 投影が無い |
| 画像の貼り付け | 無効（U-5） | ⌘V をプレーン貼り付けにする |
| チェックボックスのクリック切替 | 無効 | 図形が出ない。ガードも明示的に足す |
| リンクのクリック / ホバー | 無し | `links` が空 |
| コンテキストメニュー「リンクとして貼り付け」等 | そのまま | 明示コマンドで、ソース範囲に対する編集 |
| Mermaid 図 | 無し。レンダリングジョブも起こさない | 無駄な計算を避ける |
| AI 連携（選択 → 提案 → 適用） | そのまま | ソース範囲基準 |
| 文書内検索・置換 | そのまま | ソース範囲基準 |
| 自動保存・ドラフト・タイトル同期（H1） | そのまま | ソース基準で view の表示に依存しない |
| a11y の説明文 | プレーン用の文言を追加 | 利用者に状態を伝える |
| ズーム、テーマ | そのまま | `line_height()` 経由 |

---

## 4. 実装方針

### 4.1 モデル: 直交軸にする

**採用**: `EditorMode` は変更しない。view に「プレーンかどうか」を別に持ち、`DisclosurePolicy` に `Plain` を足す。

```rust
// crates/ui/src/line.rs
pub(crate) enum DisclosurePolicy {
    Editing,
    Reading,
    /// 投影なし。ソース行をそのまま出す。開示の概念がない。
    Plain,
}
```

```rust
// crates/ui/src/view/reading_mode.rs
pub(super) fn disclosure_policy(&self) -> DisclosurePolicy {
    if self.plain_text() {
        return DisclosurePolicy::Plain;
    }
    match self.editor_mode { /* 現行どおり */ }
}
```

**不採用: `EditorMode::Plain`（第 3 のモード）**

- `EditorMode` の doc は「本文を変更できるか、読むだけか」。表示方法とは別の関心。
- 3 状態にすると「プレーンで読み取り専用」が表現できず、Reading に入って戻るとプレーンを失う。
- `EditorMode` は非永続で、永続化したいプレーン設定と寿命が合わない。
- `body_editable()` は `== Editing` 判定なので、`Plain` を足すと編集可否の判定を全箇所見直すことになる。

### 4.2 変更ファイル一覧

#### (1) `crates/session/src/store.rs`

- `Settings` に `pub plain_text_mode: bool` を追加。`Default` は `false`。
- `FileStateStore::store` の `format!` に `plain_text_mode={}\n` を追加。
- `settings_from_read` に `strip_prefix("plain_text_mode=")` を追加。`value == "true"` のときだけ `true`（`auto_update_check` と同じ形）。
- `MemoryStateStore` は構造体をそのまま保持する実装なら変更不要（要確認）。

#### (2) `crates/presentation/src/projection/block.rs` と `mod.rs`

新しい公開関数を追加する。既存関数は変更しない。

```rust
/// ソース行をそのまま提示する。隠蔽・変換・開示を行わない。
/// `window` は既存の `present_block_with_table_projection` と同じ形で、
/// 提示する行（`BlockLine`）とブロックの span を渡す。
pub fn present_block_plain(
    block: &Block,              // BlockIndex のブロック。既存関数と同じ型
    window: &BlockWindow<'_>,   // 既存と同じ
    revision: Revision,
    line_height: f32,
) -> VisualBlock
```

- 引数の正確な型は既存の `present_block_with_table_projection`（L210）のシグネチャに合わせること。上記は意図を示すもので、型名は実装時に既存関数から写す。
- 各行は `present_raw_source(line_id, revision, range, source, line_height)` で提示する（同じモジュール階層なので `pub(super)` か `pub(crate)` に可視性を上げる）。
- 提示後、既存の L403-405 と同じく `visual_text` 末尾の `\r` `\n` を pop する。**この後処理を省くと行末に改行文字が描画・計測される**ので必ず通す。
- `lines_before` / `lines_after` は既存と同じ式（`first.line_id - span.start`、`span.len() - lines_before - lines.len()`）。
- `zero_height_lines_before = 0`、`zero_height_lines_after = 0`、`table_projection = None`。
- `kind = BlockKind::Unsupported`（`layout_table_block` に入れないため）。
- 各 `VisualLine` は `kind = Unsupported`、`style_runs` 空、`links` 空、`image` / `list` / `quote` / `table_row` なし、`estimated_height = line_height`。これは `present_raw_source` の現行出力と同じ。

高さ見積りも追加する。

```rust
/// プレーン表示でのブロック高さ。畳む行が無く、種別による倍率も無い。
pub fn plain_block_height(block: &Block, is_last: bool, trailing_empty_lines: usize, line_height: f32) -> f32
pub fn plain_block_heights(/* block_heights_with_disclosure と同じ入力から disclosure を除いたもの */) -> Vec<f32>
```

- 値は `line_height × (ブロックの span 行数 + 最終ブロックなら trailing_empty_lines)`。
- 「span 行数」は `block_height_with_disclosure`（L70-105）が数えているのと同じ行集合を使う。ブロック間の空行をどちらのブロックが持つかは既存の `block_line_span` / `trailing_blank_lines` に従う。**独自に数え直さない**。

#### (3) `crates/presentation/src/display.rs`

```rust
impl VisualBlock {
    /// プレーン提示のキャッシュ一致判定。kind は常に Unsupported なので比較しない。
    pub fn matches_plain(&self, block: &Block) -> bool  // id, source_range, confidence
}
```

#### (4) `crates/ui/src/line.rs`

- `DisclosurePolicy::Plain` を追加。
- `presented_block` / `presented_block_with_diagram`: 先頭で `Plain` を判定し、`block_context`・joined span・list・fence・table・diagram を**すべて通さず**に `present_block_plain` を呼ぶ。`forced_disclosure` は無視する。
- `lines_are_disclosed`: `Plain` は `false`（図を出さない側）。
- `expected_block_disclosures`: `Plain` は空（開示なし）。
- `DisclosurePolicy` を match している箇所はコンパイルエラーで全部わかる。`_ =>` で受けている箇所が無いかは `grep -n "DisclosurePolicy::" crates/ui/src` で確認する。

#### (5) `crates/ui/src/view/plain_text_mode.rs`（新規）

```rust
impl EditorView {
    pub(super) fn plain_text(&self) -> bool { self.settings.plain_text_mode }

    pub(super) fn toggle_plain_text(&mut self, cx: &mut Context<Self>) -> bool {
        self.set_plain_text(!self.plain_text(), cx)
    }

    /// `set_editor_mode` と同じ形。戻り値は「要求した状態になったか」。
    pub(super) fn set_plain_text(&mut self, plain: bool, cx: &mut Context<Self>) -> bool {
        if self.plain_text() == plain { return true; }
        if self.editor_mode_switch_blocked() {
            // 既存と同じ status 文言
            return false;
        }
        self.cancel_text_selection_autoscroll();
        self.settings.plain_text_mode = plain;
        self.rebuild_presentation_for_mode_change();
        self.store_settings();
        cx.notify();
        true
    }

    pub(super) fn plain_text_button(&self, cx: &mut Context<Self>) -> impl IntoElement
}
```

- 状態の置き場所は `self.settings.plain_text_mode` の 1 か所。view に別フィールドを持たない（二重管理を避ける）。
- 順序は「設定値を変える → `rebuild_presentation_for_mode_change()`」。`rebuild…` は `item_heights()` を呼ぶので、先に値を変えないと旧モードの高さが入る。ただし `capture_mode_anchor` は旧表示の幾何を使う必要がある。`rebuild…` の中で capture が最初に走るため、**値を変える前に anchor を capture する必要があるかを実装時に確認する**。`set_editor_mode`（L121-148）が `self.editor_mode = mode` と `rebuild…` をどの順で呼んでいるかに合わせること（L121-148 の代入位置は未確認）。合わない場合は `rebuild…` を「capture」と「再構築」に分けず、`set_plain_text` 内で先に `capture_mode_anchor` 相当を済ませる。
- `store_settings()` が失敗してもメモリ上の値は残る（`set_theme` と同じ）。
- AI 選択ポップアップは閉じない（プレーンでも AI 連携は有効）。
- `plain_text_button` は `reading_mode_button`（L307-353）と同じ構造。`Button::new("plain-text-toggle")`、`.disabled(self.editor_mode_switch_blocked())`、`on_mouse_down` で `prevent_default` + `stop_propagation`、`HoverCard`、debug_selector `"plain-text-toggle"`。
- 配置は `crates/ui/src/view.rs` L6339 の `.child(self.reading_mode_button(cx))` の隣。

#### (6) `crates/ui/src/icons.rs` と `assets/icons/`

- `ICON_PLAIN_TEXT_MODE = "icons/plain-text-mode.svg"` を追加（L36-38 の並び、L119 の解決、L174 のテスト一覧にも追加）。
- 状態は 1 アイコン＋ボタンの選択状態表示で示すか、ON / OFF で 2 アイコンにするかは既存ボタンに合わせる（既存は 2 アイコン）。

#### (7) `crates/ui/src/view.rs`

| 箇所 | 変更 |
| --- | --- |
| `apply_list_edit_intent`（L3599） | 先頭に `if self.plain_text() { return false; }`。Enter / Shift+Enter / Backspace / Tab / Shift+Tab のリスト処理がここ 1 か所で止まる |
| `cached_block`（L5195-5276） | `Plain` のとき: 一致判定は `matches_plain`、`disclosures_are_current` は見ない、diagram plan は `None` |
| `disclosures_are_current`（L5323） | `Plain` は常に `true` |
| `visible_line_prefix`（L4935） | `Plain` は `physical_line` をそのまま返す |
| `physical_line_prefix_for_visible_rows`（L4976） | `Plain` は恒等 |
| `neighbor_block_window_from`（L5390） | `match policy` に `Plain => None` 相当の arm。畳み込み行なし |
| `is_crossed_fence`（L4233）/ `collapses_when_inactive`（L4248） | `Plain` は `false` |
| `layout_around_widened`（L4540-4563） | `presented_block` 経由なら変更不要。直接 `block_context` を呼んでいないか確認 |

`ShiftNewline` と `Backspace` が `apply_list_edit_intent` を通らずに別のリスト処理を呼んでいないかは、L394 と L427 のハンドラ本体を実装時に読んで確認する。通っていれば上記 1 か所で足りる。

#### (8) `crates/ui/src/view/height_sync.rs`

- `item_heights()`（L6-19）: Blocks 粒度のとき `Plain` なら `plain_block_heights`。Lines 粒度は現行のままでよいはず（1 行 = `line_height`）。Lines 粒度の現行の値が `line_height` 固定であることは実装時に確認する。
- `active_height_disclosure()`（L439-449）: `Plain` は `None`。これで `ensure_active_disclosure_height` と `install_disclosure_heights_preserving_measurements` は早期 return し、背景の disclosure ジョブも起きない。
- `apply_height_index_update`（L540、L567）: `Plain` なら `plain_block_height`。

#### (9) `crates/ui/src/view/background_parse.rs`

- `schedule_document_parse`: L54 で `editor_mode` と一緒に `plain_text()` を捕捉し、L108 の高さスナップショットを `Plain` なら `plain_block_heights` で作る。
- 切替時は `mode_generation` が加算されるので、旧モードで作った結果は L81 / L148 で捨てられる。新しい条件は足さない。
- `schedule_joined_parse`（L260）: `Plain` では起動しない。

#### (10) `crates/ui/src/view/mermaid_diagrams.rs`

- `schedule_mermaid`（L98）: `Plain` では起動しない。
- `diagram_plan_for`（L252）: `Plain` は `None`。

#### (11) `crates/ui/src/view/clipboard.rs`

- `paste_into_body` の先頭で `let mode = if self.plain_text() { PasteMode::PlainText } else { mode };`。これで画像貼り付けと HTML 変換の分岐に入らない。
- コンテキストメニューからの明示的な貼り付けコマンドは変えない。

#### (12) `crates/ui/src/view/task_checkbox.rs`

- `task_checkbox_toggle_allowed()` に `&& !self.plain_text()` を追加。プレーン行は `list` を持たないので到達しない想定だが、既存コメントの方針（許可するモードを明示する）に合わせて明示的に塞ぐ。

#### (13) `crates/ui/src/view/a11y_links.rs`

- L200-203 の説明文にプレーン用の文言を追加（例: 編集可なら「プレーンテキスト編集」、Reading なら「プレーンテキスト表示（読み取り専用）」）。

#### (14) `crates/ui/src/view/settings_screen.rs`

- `display_and_save_settings`（L586）の「表示」カードに Checkbox `"settings-plain-text-mode"` を追加。ハンドラは `set_plain_text` を呼ぶだけ。
- 合成中で拒否された場合に Checkbox の見た目が実状態とずれないことを確認する（`self.plain_text()` から描画すればずれない）。

#### (15) `crates/ui/src/view/render.rs`

- L272 / L292 / L366 / L386 は `!body_editable()` で `reading_visible_row` を使う。プレーン + Reading では畳まれた行が無い。`visible_line_prefix` を恒等にしてあれば `reading_visible_row` は正しい行を返すはずだが、`reading_visible_row`（`reading_mode.rs` L86）が `DisclosurePolicy::Reading` を直接使っていないかを実装時に確認し、使っていれば `disclosure_policy()` に置き換える。6.2 節の `plain_reading_*` テストで固定する。

#### (16) フェーズ 3: Tab / Shift+Tab

- `crates/ui/src/actions.rs` L439 / L461 の Body 分岐に「`apply_list_edit_intent` が `false` かつ `plain_text()` かつ `body_editable()` かつ `ime().is_none()` かつ `!late_input_guard_armed()`」のとき `plain_indent` / `plain_outdent` を呼ぶ分岐を足す。
- `plain_indent` / `plain_outdent` は `plain_text_mode.rs` に置く。対象行範囲の全文を 1 回の `replace_range_recorded`（または既存の `replace_selection_with` と同等の 1 Undo 経路）で置換し、選択を「同じ文字を指す位置」に付け直す。
- 選択なしの Tab は `insert_text("    ")` でよい。
- 選択の付け直し規則: アンカーとヘッドのそれぞれについて、その位置より前に挿入・削除されたバイト数だけずらす。行頭より左には行かない。

### 4.3 高さ算術の方針（重要）

検討して捨てた案を記す。同じ道を再検討しないため。

| 案 | 判定 | 理由 |
| --- | --- | --- |
| プレーン中は `Granularity::Lines` に固定 | 不採用 | `apply_height_index_update` が Blocks 専用で、行数が変わる編集のたびに全高さを作り直し、折り返しの実測高さを失う |
| 「文書全体を開示」として既存算術に流す（`Some(0..len)`） | 不採用 | `schedule_document_parse` の `disclosure_refresh` が編集のたびに背景ジョブを起こす。見出し・画像の倍率も消えない |
| **プレーン専用の高さ関数を 3 か所で使う** | 採用 | 分岐が `item_heights` / `background_parse` L108 / `apply_height_index_update` L567 に限られ、値が単純（行数 × line_height） |

守る不変条件:

- プレーンかつ折り返しが起きない幅では、`HeightIndex` の合計 = 文書の表示行数 × `line_height()`。
- 任意のブロックについて、`present_block_plain` の `VisualBlock::height()`（全行提示時）= `plain_block_height`。
- 折り返しの実測高さは既存の測定経路（`drawn_line_height` L5161 ほか）がそのまま反映する。プレーン用の特別扱いはしない。

### 4.4 データフローの変更点

```text
[切替]
  トグル → set_plain_text
         → settings.plain_text_mode を更新
         → rebuild_presentation_for_mode_change()   ← ModeAnchor capture / キャッシュ破棄 / 高さ再構築
         → store_settings()
         → notify → 次フレームで resolve_mode_anchor がスクロールを合わせる

[描画]
  cached_block → presented_block(policy = Plain) → present_block_plain → VisualBlock(kind = Unsupported)
              → layout_block（通常経路。表経路に入らない）→ LayoutLine → 既存の shape / row_element

[編集]
  キー → actions.rs → apply_list_edit_intent（Plain なら即 false）→ insert_newline / dispatch
      → Editor（変更なし）→ after_input → apply_height_index_update（plain_block_height）
```

Editor、DocumentSession、保存、autosave、ドラフト、履歴には一切手を入れない。

---

## 5. 既存機能との相互作用と回帰リスク

| # | リスク | 起きる条件 | 対策 |
| --- | --- | --- | --- |
| R1 | 本文が変わる | 切替処理が Editor に触れる | 切替は `settings` と view のキャッシュだけを触る。テスト `plain_toggle_preserves_document_snapshot` |
| R2 | キャレット位置がずれる | SourceMap が恒等でない、行末の `\r\n` を pop し忘れる | `present_raw_source` を使い、既存と同じ pop を通す。テスト `plain_lines_are_identity_mapped` |
| R3 | スクロール位置が飛ぶ、空白や重なりが出る | 高さ見積りと実際の提示がずれる | 4.3 節の不変条件テスト。`visible_line_prefix` の恒等化 |
| R4 | 表ブロックで panic / 表レイアウトになる | `VisualBlock.kind` が `TableRow` のまま | `kind = Unsupported`。テスト `plain_table_block_uses_text_layout` |
| R5 | キャッシュが毎フレーム無効、入力遅延 | `matches` の kind 比較で常に不一致 | `matches_plain`。テスト `plain_block_cache_is_reused_across_frames` |
| R6 | 切替後に古い背景結果が入り高さが壊れる | 旧モードのジョブが後から届く | `mode_generation` で破棄（既存機構）。テスト `plain_toggle_discards_stale_parse_result` |
| R7 | 投影モードの挙動が変わる | 分岐の入れ方を誤る | すべて `Plain` のときだけの分岐にする。既存テストが全部通ること |
| R8 | IME 合成中の切替で合成範囲が壊れる | 切替を許す | `editor_mode_switch_blocked()` で拒否（既存）。テスト `plain_toggle_is_refused_while_composing` |
| R9 | 上下移動で行を飛ばす・止まる | `neighbor_block_window_from` / `visible_line_prefix` に畳み込み前提が残る | 分岐追加。テスト `plain_vertical_move_crosses_fence_and_table` |
| R10 | プレーン + Reading で表示行がずれる | `reading_visible_row` が Reading 用の開示を固定で使う | 4.2 (15)。テスト `plain_reading_caret_row_matches_source_line` |
| R11 | 設定ファイルの互換性 | 旧版がキーを読む | 未知キーは無視される（L495-527）。新版が旧ファイルを読むと欠損 = `false` |
| R12 | 既存設定が消える | `store` の `format!` 変更ミス | テスト `settings_survive_a_store_reload` を拡張 |
| R13 | 画像貼り付けでファイルだけ作られる | プレーンで Rich 経路に入る | `paste_into_body` 先頭でモードを PlainText に落とす。テスト `plain_paste_image_creates_no_file` |
| R14 | Mermaid ジョブが無駄に走る・図が出る | ガード漏れ | `schedule_mermaid` / `diagram_plan_for` のガード |
| R15 | 検索の現在ヒットで行が「開く」処理が誤動作 | `forced_disclosure` を使う | `Plain` では無視 |
| R16 | 文書を開いた直後（Lines 粒度）でプレーンの高さが違う | Lines 粒度の高さが `line_height` 固定でない | 4.2 (8) の確認。テスト `plain_heights_match_before_and_after_first_parse` |
| R17 | Tab インデントで選択が壊れる（フェーズ 3） | 付け直し規則の誤り | テスト `plain_indent_*` |
| R18 | 複数ウィンドウで設定が食い違う | ウィンドウごとに `settings` を持つ | 未確認。8 章 U-7 |

投影モード側への影響は「`Plain` でなければ現行と同じ式を通る」ことで抑える。新しい分岐はすべて `if policy == Plain` / `if self.plain_text()` の早期分岐にし、既存の式を書き換えない。

---

## 6. テスト計画

テストの置き場所と書き方は既存に合わせる。view のテストは `reading_mode.rs` L356 以降のヘルパ（`settle` L366、`open_view` L373、`deliver_requested_frames` L400、`toggle_and_settle` L434、`measured_row` L536、`assert_caret_row_on_screen` L659、`snapshot` L668）を再利用する。`snapshot` は `(Revision, len, dirty, can_undo)` を返す。

### 6.1 単体テスト

**`crates/presentation`**

| テスト名 | 入力 | 期待 |
| --- | --- | --- |
| `plain_lines_are_identity_mapped` | 見出し・強調・リンク・リスト・引用・表・フェンス・画像を含む文書 | 全行で `visual_text` = ソース行から行末 `\r\n` を除いたもの。SourceMap で visual → source → visual が全オフセットで往復一致 |
| `plain_lines_carry_no_markdown_decoration` | 同上 | 全行で `style_runs` / `links` 空、`image` / `list` / `quote` / `table_row` なし、`kind == Unsupported` |
| `plain_block_has_no_zero_height_lines` | フェンス、表 | `zero_height_lines_before/after == 0`、`table_projection == None`、全行 `estimated_height == line_height` |
| `plain_block_strips_crlf_from_visual_text_only` | `a\r\nb\r\n` | `visual_text` は `a` / `b`。`source_range` は `\r\n` を含む |
| `plain_block_height_matches_presented_block` | 各種ブロック、最終ブロック＋末尾空行 | `plain_block_height == present_block_plain(...).height()` |
| `plain_block_heights_sum_to_line_count` | 空文書、末尾改行あり / なし、連続空行、CRLF | 合計 = 表示行数 × line_height |
| `plain_window_reports_lines_before_and_after` | 長いブロックの一部だけ提示 | `lines_before + lines.len() + lines_after == span.len()` |
| `matches_plain_ignores_kind` | 表ブロック | `matches_plain` true、`matches` false |

**`crates/session`**

| テスト名 | 入力 | 期待 |
| --- | --- | --- |
| `plain_text_mode_survives_a_store_reload` | `true` を保存して再読込 | `true`。他の設定値も不変 |
| `plain_text_mode_defaults_to_false_when_missing` | キーなしの `settings.conf` | `false` |
| `plain_text_mode_ignores_invalid_value` | `plain_text_mode=yes` | `false` |

### 6.2 統合テスト（`crates/ui`、GPUI テストコンテキスト）

**切替の保全**

| テスト名 | 手順 | 期待 |
| --- | --- | --- |
| `plain_toggle_preserves_document_snapshot` | 編集して dirty にし、ON → OFF → ON | 毎回 `snapshot` 不変、本文文字列不変 |
| `plain_toggle_preserves_caret_and_selection_offsets` | `**bo|ld**` の中、表セル内、フェンス行、複数ブロック選択 | 選択のアンカー・ヘッドのオフセット不変 |
| `plain_toggle_keeps_caret_row_on_screen` | 長文の中ほどにスクロールして切替 | `assert_caret_row_on_screen` |
| `plain_toggle_round_trip_restores_scroll` | ON → OFF | キャレット行の画面内位置が許容差内で元に戻る |
| `plain_toggle_does_not_enter_undo_history` | 入力 → 切替 → Undo | 入力が取り消される。`can_undo` の遷移が切替なしと同じ |
| `undo_across_modes_restores_text` | 投影で編集 → プレーンで Undo、逆も | 本文が編集前に戻る。キャレットも Editor の規則どおり |
| `plain_toggle_is_refused_while_composing` | IME 合成中に切替 | `false`、モード不変、合成範囲不変、status 文言 |
| `plain_toggle_discards_stale_parse_result` | パース予約 → 切替 → フレーム配送 | 高さ合計がプレーンの不変条件を満たす |
| `plain_mode_survives_reading_round_trip` | ON → Reading → Editing | プレーンのまま |
| `plain_mode_applies_to_other_tabs_and_new_documents` | ON → タブ切替 / 文書を開く | プレーンで表示 |
| `plain_toggle_persists_setting` | ON | `MemoryStateStore` に `true` |
| `plain_toggle_survives_settings_store_failure` | 保存失敗する store | モードは切り替わる。`settings_store_error` が立つ |

**表示・高さ**

| テスト名 | 手順 | 期待 |
| --- | --- | --- |
| `plain_rows_show_source_verbatim` | 各種構文の文書を描画 | 画面の行テキスト = ソース行 |
| `plain_heading_row_has_body_line_height` | `# H1` | 行高 = `line_height()` |
| `plain_fence_and_table_delimiter_rows_are_visible` | キャレットを離す | フェンス行・区切り行が 1 行分の高さで存在 |
| `plain_table_block_uses_text_layout` | 表 | panic しない。各行が通常の `LayoutLine` |
| `plain_height_index_matches_line_count` | 折り返しなしの幅 | 高さ合計 = 行数 × line_height |
| `plain_heights_match_before_and_after_first_parse` | 開いた直後と初回パース後 | 高さ合計が同じ |
| `plain_block_cache_is_reused_across_frames` | 2 フレーム描画 | 2 回目に再提示が起きない（既存のキャッシュ計測手段があれば使う。無ければ `matches_plain` の単体テストで代える） |
| `plain_mode_schedules_no_mermaid_job` | mermaid フェンス | ジョブなし、図の行なし |
| `plain_find_highlight_uses_source_ranges` | 検索 → 次へ | 現在ヒット行の高さ・テキスト不変、ハイライト範囲 = ソース範囲 |
| `plain_reading_caret_row_matches_source_line` | プレーン + Reading | `reading_visible_row` = 物理行 |

**編集**

| テスト名 | 手順 | 期待 |
| --- | --- | --- |
| `plain_enter_in_list_inserts_bare_newline` | `- a|` で Enter | `- a\n`。マーカーなし |
| `plain_enter_on_empty_list_item_keeps_marker` | `- |` で Enter | `- \n` |
| `plain_shift_enter_inserts_bare_newline` | 同上 | `\n` のみ |
| `plain_backspace_after_marker_deletes_one_grapheme` | `- |a` で Backspace | `-a` |
| `plain_tab_in_list_does_not_indent_item` | `- a|` で Tab（フェーズ 1〜2） | 本文不変 |
| `plain_enter_is_ignored_while_composing` | 合成中 Enter | 本文に改行なし |
| `plain_paste_html_inserts_plain_text` | HTML + テキストのクリップボード | テキストがそのまま入る |
| `plain_paste_url_over_selection_replaces_text` | 選択 + URL | リンク化されない |
| `plain_paste_image_creates_no_file` | 画像のみ | 本文不変、ファイル生成なし |
| `plain_click_on_task_marker_does_not_toggle` | `- [ ] a` の `[ ]` をクリック | 本文不変、キャレットがその位置 |
| `plain_click_on_link_in_reading_does_not_open` | プレーン + Reading でリンク文字列をクリック | 開く要求が出ない |
| `plain_vertical_move_crosses_fence_and_table` | フェンス・表の上から ↓ を繰り返す | 物理行を 1 行ずつ進む。列保持 |
| `plain_vertical_move_keeps_preferred_x_over_short_lines` | 長・短・長の行 | 3 行目で元の列 |
| `plain_ime_commit_inserts_at_caret` | 合成 → 確定 | 確定文字列がキャレット位置に入る |
| `plain_ai_apply_replaces_source_range` | AI 提案の適用 | 対象ソース範囲が置換される（既存テストをプレーンで再実行） |
| `plain_edit_then_toggle_shows_same_text_in_projection` | プレーンで編集 → OFF | 投影表示が編集後ソースに一致 |

**フェーズ 3**

| テスト名 | 手順 | 期待 |
| --- | --- | --- |
| `plain_tab_inserts_four_spaces` | 選択なし | 空白 4 個、キャレットはその後ろ |
| `plain_indent_selected_lines_is_one_undo_step` | 3 行選択で Tab → Undo | 各行頭に 4 空白。Undo 1 回で全部戻り、選択も戻る |
| `plain_outdent_removes_up_to_four_spaces` | 行頭 2 / 4 / 6 空白、`\t` | 0 / 0 / 2 空白、`\t` 削除 |
| `plain_outdent_without_indent_is_noop` | 行頭に空白なし | 本文・Revision 不変 |
| `plain_indent_keeps_selection_on_same_text` | 行の途中から途中まで選択 | 選択文字列が同じ |
| `plain_indent_respects_crlf` | CRLF 文書 | 改行コード不変 |

### 6.3 GUI 確認

Debug ビルドで行う。記録にはビルド種別・対象 SHA・実行バイナリの由来を含める。

| # | シナリオ | 確認点 |
| --- | --- | --- |
| G1 | 見出し・リスト・表・フェンス・画像・Mermaid を含む文書でトグル ON | 記号がすべて見える。見出しが本文と同じ大きさ。画像・図が出ない |
| G2 | 長文の中ほどで ON / OFF を 5 往復 | キャレット行が画面内。スクロールが飛ばない。本文に変更なし（dirty にならない） |
| G3 | プレーンで日本語 IME 入力（合成・変換・確定・キャンセル） | 下線と候補ウィンドウの位置が正しい。合成中はトグルが押せない |
| G4 | リスト内で Enter / Backspace | マーカーが自動で付かない・消えない |
| G5 | 折り返す長い行で ↑↓ / Home / End / クリック / ドラッグ選択 | 期待位置にキャレット |
| G6 | 検索して次へ / 置換 | ハイライト位置が正しい |
| G7 | ON のまま終了して再起動 | プレーンで起動 |
| G8 | プレーンで編集して保存し、外部ツールでファイルを確認 | 入力どおり。余計な変更なし |
| G9 | ズーム変更、テーマ変更 | 表示崩れなし |
| G10 | 大きな文書（数万行）でスクロール・入力 | 引っかかりが投影モード以下 |

G3 は macOS と Windows の両方で行う（Windows は Windows 側で直接実行）。

### 6.4 変更前測定

プレーン経路は投影より処理が少ないので悪化は見込まないが、`cached_block` と高さ更新に分岐を足す。投影モードの退行がないことを確認するため、変更前に次を取る。

- 大きな文書での入力 1 打鍵あたりの処理時間、スクロール時のフレーム時間。
- 測定手段（既存のベンチ・トレース）がリポジトリにあるかは**未確認**。`docs/refactor-execution-plan.md` の測定手順に従う。

---

## 7. 段階分割

| フェーズ | 内容 | 完了条件 |
| --- | --- | --- |
| **P1 表示と切替** | 4.2 の (2)(3)(4)(5)(6)(7)(8)(9)(10)(12)(13)(15) と、`Settings` へのフィールド追加（(1)）、(11) の貼り付け。トグルボタン。永続化も含む | 6.1 全部、6.2 の「切替の保全」「表示・高さ」「編集」が通る。既存テストが全部通る。G1〜G9 |
| **P2 設定画面** | (14) 設定画面のチェックボックス | チェックボックスとトグルボタンが同じ状態を示す。合成中の拒否で表示がずれない |
| **P3 Tab インデント** | (16) | 6.2「フェーズ 3」のテスト。G4 に Tab を追加 |

- P1 に永続化を含める理由: 状態の置き場所を `settings` の 1 か所にするため。非永続の view フィールドで先に出すと、P2 で置き場所を移す変更が要る。
- P1 だけで Issue の中核（投影なしで表示・編集できる）は成立する。P2 と P3 は独立に出せる。
- P1 が大きすぎる場合は、(2)(3) の `presentation` 追加（未使用の公開関数＋単体テスト）を先行 PR に切り出せる。

follow-up 候補（本 Issue の外）: 行番号、等幅フォント、自動インデント、タブ単位のモード、ショートカット、CRLF の改行整合。

---

## 8. 未確定事項

| # | 事項 | 選択肢 | 推奨 |
| --- | --- | --- | --- |
| U-1 | 切替単位 | (a) アプリ全体 (b) タブ単位（`SessionViewState` 拡張、非永続） (c) ファイル単位で記憶（新しい保存先が要る） | **(a)**。既存の Reading がウィンドウ全体で、利用者の好みとして固定されやすい。(b)(c) は要望が出てから |
| U-2 | ショートカット | (a) 付けない (b) `secondary-shift-p` など空きキー | **(a)** で P1 を出す。2.6 の一覧に無いキーでも、プラットフォーム別バインドとの衝突は未確認 |
| U-3 | フォント | (a) 本文フォントのまま (b) 等幅（`ui-monospace`） (c) 設定で選ぶ | **(a)**。(b) は日本語混在時の字幅と行高の検証が要る。#738 と合わせて決める |
| U-4 | Tab キーで入れる文字 | (a) 空白 4 個 (b) `\t` (c) 設定 | **(a)**。GPUI のタブ文字の描画幅が未確認で、(b) はキャレット位置の検証が要る |
| U-5 | プレーンでの画像貼り付け | (a) 無効 (b) 現行どおりファイル保存 + `![](…)` 挿入 | **(a)**。「普通のエディタ」に合わせる。(b) を望む声があれば明示コマンドで足す |
| U-6 | CRLF 文書での Enter | (a) 現状維持（LF を挿入） (b) 文書の改行コードに合わせる | 本 Issue では **(a)**。両モード共通の既存挙動なので別 Issue で (b) を扱う。`crates/markdown/src/list_editing.rs` の `preferred_line_terminator` が使える可能性があるが可視性は未確認 |
| U-7 | 複数ウィンドウ時の同期 | (a) 切り替えたウィンドウだけ即時反映、他は次回起動時 (b) 全ウィンドウへ通知 | 既存の `theme` / `autosave` と同じ扱いにする。既存の挙動は未確認 |
| U-8 | プレーン + Reading を許すか | (a) 許す (b) プレーン中は Reading トグルを無効化 | **(a)**。直交軸の自然な帰結。R10 のテストで固定 |
| U-9 | トグルボタンの見た目・文言 | アイコン、ツールチップ（例「プレーンテキスト表示」/「Markdown 表示に戻す」） | 既存の Reading ボタンに揃える。アイコンは新規作成が要る |
| U-10 | 行末空白・タブの可視化 | (a) しない (b) する | **(a)**。#738 か別 Issue |

### 未確認の一覧（実装前に読むこと）

- `set_editor_mode`（`reading_mode.rs` L121-148）内の代入と `rebuild…` の順序、および `capture_mode_anchor` が旧表示の幾何を必要とするか（4.2 (5)）。
- `reading_visible_row`（L86）が使うポリシー（4.2 (15)）。
- `ShiftNewline`（`actions.rs` L394）と `Backspace`（L427）のハンドラが `apply_list_edit_intent` 以外のリスト処理を持つか。
- Lines 粒度での `item_heights()` の値。
- 表編集の入力経路が `table_projection` 以外から表を認識するか。
- `on_row_mouse_down` のダブルクリック処理。
- `MemoryStateStore` の実装形。
- GPUI の `\t` 描画。
- 複数ウィンドウ時の `settings` の共有。
- `crates/presentation/src/source_map.rs` の詳細、`image_paste.rs` / `link_hover.rs` / `footer.rs` の細部。
- `docs/adr` の本文（0004 source-visual-mapping、0007 ime-composition、0013 undo-redo、0018 block-index、0020 block-virtualization、0021 layout-lines、0022 layout-cache-invalidation、0029 source-first-list-editing）。新しい ADR が要るかは Commander の判断に委ねる。
- 性能測定の既存手段。

---

## 9. #738（ハイライト）との接続点

- プレーン行は恒等写像なので、ソース範囲 = visual 範囲。`VisualLine.style_runs`（`StyleRun { visual_range, kind: StyleKind }`）にそのまま色付け情報を載せられる。
- 注入点は `present_block_plain` の 1 か所。ここに「ブロック種別と行のソースから `style_runs` を作る」処理を足せば、`shape.rs`（L60-100）が既存経路で描く。
- ハイライトで文字サイズ・行高・字幅を変えないこと。変えると 4.3 節の高さの不変条件が崩れる。色と太さ（字幅が変わらない範囲）に限る。
- `style_runs` を入れるとキャッシュ一致条件（`matches_plain`）にハイライト設定の世代が要る。#738 側で `mode_generation` の加算か別の世代番号で扱う。
- 本 Issue では `style_runs` を空で出す。#738 の ON / OFF 設定はプレーンモードの設定とは独立にする。

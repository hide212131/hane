# Issue #737 実装引き継ぎ: プレーンテキストモード（全フェーズ）

## 位置づけ

- 対象 Issue: #737 feat(editor): プレーンテキストモード（普通のエディタのような振る舞い）
- 採用済み設計: `docs/implementation/issue-737-plain-text-design.md`（本書と一緒に読むこと。設計の決定・仕様・テスト計画はそちらが正本）
- 基準 SHA: `6d45d29a86a8c04692705ef7b869570b35067443`（v0.47.0 リリース時点の main）
- 状態: **未実装・未検証**。この文書と設計書だけの準備 commit であり、製品コードはまだ何も変わっていない

## 今回の範囲（設計書 §7 の全フェーズを 1 PR で実装する）

- **P1 表示と切替**: 設計書 4.2 の (1)〜(13)・(15)。`present_block_plain` の新設、`DisclosurePolicy::Plain`、プレーン専用の高さ関数、`matches_plain`、切替トグルボタン、`settings.conf` への `plain_text_mode` 永続化、貼り付けのプレーン化を含む
- **P2 設定画面**: 4.2 (14)。設定画面「表示」カードのチェックボックス。トグルボタンと同じ状態を示し、IME 合成中の拒否で表示がずれないこと
- **P3 Tab インデント**: 4.2 (16)。プレーン時の Tab / Shift+Tab 空白インデント（空白4個、1 Undo 単位、選択の付け直し規則は設計書どおり）

## 受入条件

- 設計書 §3（期待挙動の仕様表）の全項目を満たすこと。特に最重の不変条件3つ:
  1. 切替の前後で `(Revision, 本文長, dirty, can_undo)` と本文バイト列が変わらない
  2. 切替の前後でキャレットと選択のソースオフセットが変わらない
  3. プレーン表示中、各行の `visual_text` はソース行（行末 `\r\n` を除く）と完全一致し、SourceMap は恒等
- 設計書 §6 のテストを、表に挙げられた名前・入力・期待結果で追加すること。代表例:
  - presentation: `plain_lines_are_identity_mapped`、`plain_lines_carry_no_markdown_decoration`、`plain_block_has_no_zero_height_lines`、`plain_block_strips_crlf_from_visual_text_only`、`plain_block_height_matches_presented_block`、`plain_block_heights_sum_to_line_count`、`matches_plain_ignores_kind`
  - session: `plain_text_mode_survives_a_store_reload`、`plain_text_mode_defaults_to_false_when_missing`、`plain_text_mode_ignores_invalid_value`
  - ui: `plain_toggle_preserves_document_snapshot`、`plain_toggle_preserves_caret_and_selection_offsets`、`plain_toggle_keeps_caret_row_on_screen`、`plain_toggle_does_not_enter_undo_history`、`plain_toggle_is_refused_while_composing`、`plain_rows_show_source_verbatim`、`plain_table_block_uses_text_layout`、`plain_height_index_matches_line_count`、フェーズ3の `plain_indent_*` / `plain_outdent_*`
- 回帰の入力・期待結果の例（設計書 §3.4）: プレーン ON で `- abc` の途中にキャレットを置いて Enter → `\n` が1つ挿入されるだけ（マーカー継続・番号振り・空項目解除をしない）。`# 見出し` は記号を含めそのまま・本文と同じサイズと太さで表示される
- 既存テストが全部通ること（Commander が `cargo test -p hane-presentation -p hane-session --no-fail-fast`、`cargo test -p hane-ui --no-fail-fast`、`cargo check --workspace` を実行して確認する。Clippy だけを動作確認にしない）

## 対象外

- #738 の構文色付け（`style_runs` は空で出す。注入点だけ用意する）
- 行番号・ガター、等幅フォントへの切替、自動インデント、タブ単位・ファイル単位のモード記憶、切替ショートカット
- CRLF 文書での改行コード整合（両モード共通の既存挙動。本 Issue では現状維持）
- `.md` 以外のファイルを開けるようにすること
- 既存の投影経路・高さ算術のリファクタリング（分岐追加のみ。式の書き換えをしない）
- `.github/`、`AGENTS.md`、`CLAUDE.md` 等の保護パスの変更

## 実装時の注意

- 設計書末尾の「未確認の一覧」は実装時にコードで確認してから進めること（特に `set_editor_mode` 内の代入と `rebuild_presentation_for_mode_change()` の順序、`ShiftNewline` / `Backspace` ハンドラが `apply_list_edit_intent` 以外のリスト処理を持つか、表編集の入力経路）
- 新しい分岐はすべて `Plain` のときだけの早期分岐にし、投影モードの既存の式を書き換えないこと（設計書 §5 R7）
- アイコンは新規作成が要る（設計書 4.2 (6)。既存の Reading ボタンの2アイコン方式に揃える）

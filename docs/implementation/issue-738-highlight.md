# Issue #738 実装引き継ぎ: ハイライト（見出しなどへの色付け）

## 位置づけ

- 対象 Issue: #738 feat(editor): ハイライト（見出しなどへの色付け）
- 採用済み設計: `docs/implementation/issue-738-highlight-design.md`（本書と一緒に読むこと。設計の決定・仕様・色の値・テスト計画はそちらが正本）
- 基準 SHA: `32e9d187029b7d573fd345c835f777f1e49a1837`（#737 マージ後の main）
- 状態: **未実装・未検証**。この文書と設計書だけの準備 commit であり、製品コードはまだ何も変わっていない
- 先行条件: #737 P1（プレーンテキストモード）は PR #739 で実装済み。本書の設計書は #737 実装前の main を調査対象とするため、行番号や前提が実コードとずれる箇所は実コードを正として読み替えること

## 今回の範囲（設計書 §8 の P1＋P2 を 1 PR で実装する。P3 は対象外）

- **P1 色付けの本体**: 設計書 5.2 の (1)〜(10)。プレーン表示専用の分類（既存 pulldown-cmark 解析の再利用、新規レクサは作らない）、`StyleKind::Syntax(SyntaxTone)` と `InlineDisplay.tone` の追加、新規色トークン4個（値は設計書 §4.1 の表どおり固定）、検索一致セグメントでの構文色の抑制、`settings.conf` の `syntax_highlight` キー（既定 true、プレーンモードとは独立）とセッタまで（UI なし）
- **P2 設定画面**: 5.2 (11)。設定画面にハイライトのチェックボックスを追加し、`settings_checkbox_reflects_syntax_highlight` を満たす

## 受入条件

- 設計書 §3（期待挙動の仕様）の全項目と §4.1（色の割り当て表の値）を満たすこと。特に次の不変条件:
  1. ハイライトは `style_runs` だけを変える。`visual_text`・SourceMap・ブロック種別・高さは ON/OFF で完全一致する
  2. 投影モード（編集・閲覧）では `StyleKind::Syntax` を一切出さない
  3. ハイライト切替の前後で文書スナップショット（Revision・本文・dirty・Undo）とキャレット・選択のソースオフセットが変わらない
  4. 色だけのランは幾何を変えない（ON/OFF で全オフセットの `x_for_offset` が一致する）
- 設計書 §7.1 の単体テストを、表に挙げられた名前・入力・期待結果で追加すること。対象: `crates/presentation/src/projection/highlight.rs`（`atx_heading_splits_marker_and_text`、`heading_levels_use_two_tones`、`inline_code_is_code_tone_without_monospace_or_background`、`inline_link_colors_text_and_dims_destination`、`quote_marker_is_structure_and_body_is_quote`、`list_markers_are_structure`、`task_checkbox_is_structure`、`table_cell_ranges_exclude_pipes`、`plain_highlight_runs_are_on_char_boundaries` 等の表の全件）、`plain_code_line_runs`、`crates/presentation/src/display.rs`（`syntax_style_sets_only_tone`、`union_keeps_higher_ranked_tone` 等）、`crates/presentation/src/projection/block.rs`（`plain_highlight_changes_only_style_runs`、`plain_highlight_keeps_block_height_invariants`、`plain_highlight_none_matches_issue_737_output` 等）、`crates/ui/src/theme.rs`（`syntax_colors_keep_contrast`）、`crates/ui/src/line.rs`（`syntax_tone_color_maps_every_tone_in_both_themes`、`syntax_tone_is_suppressed_on_find_match`、`line_segments_split_at_syntax_run_edges`）、`crates/session/src/store.rs`（`syntax_highlight` の往復・欠損時 true・不正値時 true）
- 設計書 §7.2 の統合テストを表の名前で追加すること（`syntax_highlight_toggle_preserves_document_snapshot`、`syntax_highlight_toggle_rebuilds_plain_presentation`、`syntax_highlight_toggle_in_projection_mode_changes_nothing_visible`、`projection_mode_never_emits_syntax_tone`、`syntax_highlight_toggle_is_refused_while_composing`、`plain_highlight_bold_reaches_shape_runs`、`plain_highlight_does_not_move_color_only_glyphs`、`plain_heading_caret_x_matches_painted_text`、`plain_highlight_edit_reparses_only_edited_block`、`plain_highlight_block_cache_is_reused_across_frames`、`plain_highlight_typing_keeps_caret_offset`、`plain_highlight_over_budget_block_uses_window_parse`、`plain_highlight_large_code_block_is_exact`、`plain_highlight_survives_theme_change`、`settings_checkbox_reflects_syntax_highlight`）。P3 の `plain_highlight_uses_joined_parse_when_available` は今回追加しない
- 既存テストが全部通ること（Commander が `cargo test -p hane-presentation -p hane-session --no-fail-fast`、`cargo test -p hane-ui --no-fail-fast`、`cargo check --workspace` を実行して確認する。Clippy だけを動作確認にしない）

## 対象外

- P3（巨大ブロックの正確化、`PlainHighlightInput::Joined` の利用）。設計書 §8 のとおり実使用で必要性を確認してから別途行う
- 投影モードへの色付け適用（設計書 §3.6・U-6 で「しない」と決定済み）
- 色のカスタマイズ、等幅フォント、コードブロックの言語別ハイライト、参照リンクの解決
- `.github/`、`AGENTS.md`、`CLAUDE.md` 等の保護パスの変更

## 実装時の注意

- **テストの隔離（必須）**: 設定を切り替える新規テストは、必ず隔離したインメモリストア（`StateStores::memory()` と `EditorView::from_sessions`）でビューを構築すること。`EditorView::new` は `StateStores::from_environment()`（プロセス共有の実ファイル設定）を使うため、そこで `syntax_highlight` や `plain_text_mode` を切り替えると並行実行中の他のテストへ設定が漏れて一斉に失敗する（#739 で実際に発生し、司令塔が実証した）。`plain_text_mode.rs` のテストモジュールの `open_view` パターンを流用すること
- 設計書 §9 の未確定事項は推奨どおりに確定済みとして扱う（U-1 錆色、U-2 2段、U-5 本文フォント、U-7 既定 ON、U-8 独立設定、U-10 検索一致は本文色、等）。迷ったら §9 の推奨が決定である
- 設計書末尾の「未確認の一覧」は実装時にコードで確認してから進めること
- 色の値は §4.1 の表の16進値をそのまま使うこと。微調整が必要と判断した場合は §7.1 のコントラストテスト（背景比 4.5 以上、選択背景比 3.0 以上）を通る範囲でのみ行い、変更した値を実装報告に明記すること

# Issue #709 実装ハンドオフ（trailing-blank-caret フェーズ）

- 対象 Issue: #709「リスト直後の末尾空白行でカーソルがリストのインデント位置に描画される」
- フェーズ: trailing-blank-caret（単一フェーズ。分割しないことは Jev 判断済み）
- 基準 SHA: 40528347e225946990043008cc93ebdcc0b377b5（main / v0.44.0 リリース準備）
- 採用済み設計: `docs/implementation/issue-709-trailing-blank-caret-design.md`（本 branch に同梱。根本原因・観測結果・Jev 判断の追補を含む）
- 状態: **未実装・未検証**。この文書と設計書だけの準備 commit であり、製品コード・テストはまだ変更していない。

## 今回の範囲

設計書「修正方針」のとおり、block presentation 層で末尾空白行を本文から分離する。

- `crates/presentation/src/projection/block.rs` の `disclosure_runs` を変更し、`content_end = span.end - trailing_blank_lines` で本文と末尾空白行を分離する。末尾空白行は一行ずつ処理する。
- `present_block_with_table_projection` では、末尾空白行に cached `JoinedParse` を適用しない。`joined.is_some()` による単行の共有解析も対象外にする。
- 末尾空白行は `LineContext::Normal`、`list_projection = None` で単行表示する。本文側の共有解析も末尾空白行を含めない。
- `expected_disclosures` を同じ分岐に合わせる。末尾空白行へ本文の merged disclosure を伝播させない。
- ソース範囲・SourceMap・既存の改行除去・行高は維持する。

不変条件: 末尾空白行に `list` / `quote` の所有情報を付けず、空白なしの行頭を通常段落と同じ x=0 にすること。

## 受入条件（要点。正本は PR の handoff コメント）

- 対象文書 `# 2026-11-10_test\n\n- abc\n\n` を実 BlockIndex / BlockWindow から表示し、offset 25 と 26 の両方で `list/quote = None` かつカーソル x = 0 になること。SourceMap と逆方向 hit-test も確認する。
- formal 投影あり/なし、cached joined parse あり/なし、全行表示/末尾のみ表示で結果が一致すること。
- 本物の継続行・lazy continuation・loose list 内部の空白行では、従来の所有情報とインデントが保持されること。
- 箇条書き・番号付き・タスク・ネストしたリスト、引用、後続段落との間の空白行、複数空行、CRLF でも回帰がないこと。
- UI 側に、Enter×2 → offset 25/26 へ移動 → formal index 公開後、という経路の回帰テストを追加すること。

## 対象外

- タイリング（`tiled_blocks` のブロック所属）の変更
- リスト編集（Enter の継続・終了、Backspace）のテキスト挙動の変更
- レイアウト側の継続行規則の変更、「空に見える行は一律インデントゼロ」のような描画側の回避
- 引用内の `>` を持つ空行（whitespace-only ではない行）の扱いの変更

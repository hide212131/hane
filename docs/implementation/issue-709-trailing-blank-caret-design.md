# Issue #709 設計書

調査対象: ローカル HEAD `27b5ebc2`。読み取り専用で調査し、コード変更・テスト実行はしていない。

## 根本原因（データの流れに沿って）

1. **末尾空白行の所属と、構文の所属が分離されていない。**  
   [block_index.rs](/home/hatch/workspace/projects/hane/crates/markdown/src/block_index.rs:839) の `tiled_blocks` は、次のブロックまでの空白行を上のブロックへ含める。`block_line_span` は EOF の空行も表示対象に加える。

2. **末尾空白行がリスト本文と同じ共有解析へ渡される。**  
   [line.rs](/home/hatch/workspace/projects/hane/crates/ui/src/line.rs:175) の `presented_block` → `block_context` が `trailing_blank_lines` を計算し、`BlockWindow` に渡す。しかし [block.rs](/home/hatch/workspace/projects/hane/crates/presentation/src/projection/block.rs:520) の `disclosure_runs` は、本文と末尾空白行を `LineContext::Normal` 同士として結合する。**リスト自体も Normal なので、現在の文脈切替では境界を作れない。**

3. **共有解析から継続行のメタデータが付く。**  
   `present_joined_run_with_list_projection` → `present_markdown_from_parse` → [mod.rs の `list_row_metadata`](/home/hatch/workspace/projects/hane/crates/presentation/src/projection/mod.rs:2030) は、共有解析の項目範囲、または formal な `ListProjection::item_for_range` から所有者を求める。項目と交差し、開始マーカーを持たない行は `Continuation` となる。末尾空白行を構文外として除外する処理が、この経路にはない。

4. **レイアウトはその情報に従ってインデントする。**  
   [layout.rs の `line_geometry`](/home/hatch/workspace/projects/hane/crates/presentation/src/layout.rs:1533) は `VisualLine.list` がある行にマーカー列の幅を確保する。文字が空でも位置は右へずれ、`BlockLayout::point_for_source` がその座標をカーソル位置として返す。`context = Normal` への変更だけでは解消しない。

### 編集直後と開き直しの差

まず、指定文書は **26 bytes** あり、次の二行を区別する必要がある。

| offset | 行のソース | 現在の処理 |
|---|---|---|
| 25 | `"\n"` | 空文字ではないため、共有解析・リスト所有者判定へ進む |
| 26 | `""`（EOF 空行） | `source.is_empty()` により `present_plain` へ進み、`list = None` |

Enter×2 後は通常 offset 26 に着く。この違いで「編集直後は左端、移動後はインデント」を説明できる。

加えて、`BlockIndex::update` は formal なリスト投影を破棄し、全体解析はそれを再構築する。キャッシュされた `parse_joined_span` は末尾空白行を除外する一方、その場の `parse_joined_block` は渡された行全体を結合する。これらも表示が解析経路へ依存する要因となる。**同じ offset 25 での経路差は、実行による追加確認が必要。**

## 修正方針（具体的な変更点のリスト）

修正層は **block presentation** とする。

- `disclosure_runs` を変更し、`content_end = span.end - trailing_blank_lines` で本文と末尾空白行を分離する。末尾空白行は一行ずつ処理する。
- `present_block_with_table_projection` では、末尾空白行に cached `JoinedParse` を適用しない。`joined.is_some()` による単行の共有解析も対象外にする。
- 末尾空白行は `LineContext::Normal`、`list_projection = None` で単行表示する。本文側の共有解析も末尾空白行を含めない。
- `expected_disclosures` を同じ分岐に合わせる。末尾空白行へ本文の merged disclosure を伝播させず、空文字行の実際の `None` とも一致させる。
- ソース範囲・SourceMap・既存の改行除去・行高を維持する。タイリング、編集処理、レイアウトの継続行規則は変更しない。

修正後の不変条件は、**末尾空白行に `list` / `quote` の所有情報を付けず、空白なしの行頭を通常段落と同じ x 座標にすること**。

## 影響範囲とリスク

- 本物の継続行、lazy continuation、loose list 内部の空白行は `content_end` より前なので、既存の所有情報・インデントを維持する。
- 引用の末尾空白行も同様に分離する。`>` を持つ引用内の空行は whitespace-only ではないため、この対象に入らない。
- fenced code・table の末尾空白行にも共通処理が作用する。既存の境界行・行高処理を維持する。
- 空白文字が実在する行は、その文字幅を保持する。レイアウトで「空に見える全行」のインデントをゼロにすると、正当な継続行まで壊すため採用しない。

## テスト計画

主な回帰テストは [projection/tests.rs](/home/hatch/workspace/projects/hane/crates/presentation/src/projection/tests.rs) に置く。**presentation 単体でカーソル座標まで検証できる。** `FixedAdvanceShaper` と `layout_block` → `point_for_source` を使う。

- 指定文書を実際の `BlockIndex`・`BlockWindow` から表示し、offset **25 と 26 の両方**で `list/quote = None`、カーソル `x = 0` を検証する。SourceMap と逆方向の hit-test も確認する。
- formal 投影あり／なし、cached joined parse あり／なし、全行表示／末尾だけの表示で結果が一致することを確認する。
- 箇条書き・番号付き・タスク・ネストしたリスト、引用、後続段落との間の空白行、複数空行、CRLF を対象にする。
- 本物の継続行とリスト内部の空白行では、従来の所有情報・インデントが保持されることを確認する。

UI 側は `crates/ui/src/view.rs` に、Enter×2 → offset 25/26 へ移動 → formal index の公開後、という経路の回帰テストを追加する。macOS の対象 head の Debug build でも、読み込み・編集・再読み込み後のカーソル位置と文字入力位置を focused scenario として確認する。

## 未決事項

- 実機で「編集直後」と「開き直し後」のカーソル offset が同一だったかは未確認。実装前に offset・`VisualLine.list`・formal 投影／joined cache の有無を観測し、経路差を確定する。
- Jev CLI は認証未設定で応答を取得できなかったため、本設計はコード調査に基づく。
## 追補: 観測結果と Jev 判断（2026-11-10、Commander）

### Jev 判断（jev run、モデル jev-1.14.0）
- design_sound（noul）: 0.84 — 設計方針は妥当
- next_action（choice）: observe_then_implement（確率 0.99、confidence 0.98）→ 観測を先に行い、その結果を反映してから実装へ

### 観測（presentation 層の実測、対象文書 `# 2026-11-10_test\n\n- abc\n\n`、実 BlockIndex / BlockWindow から表示）
- List ブロック: range 19..26、line span 2..5、trailing_blank_lines = 2
- offset 25 の行（src 25..26）: **全バリアント（formal 投影あり/なし × joined cache あり/なし）で `list = Some(Continuation)`**、`point_for_source(25) = (row 1, x = 16.0)`（FixedAdvanceShaper でのマーカー列幅ぶん右）
- offset 26 の行（src 26..26、EOF 空行）: 全バリアントで `list = None`、`point_for_source(26) = (row 2, x = 0.0)`
- 見出しブロック側の末尾空白行（src 18..19）は list = None で正常

### 結論
- 経路差の正体は offset の非対称で確定した。Enter×2 の終了後はカーソルが offset 26（x=0、正常）に着くが、カーソル移動や開き直しで offset 25 の行に触れると Continuation として描画される。
- formal 投影 / joined cache の有無は結果に影響しない（設計の当該仮説は棄却）。修正方針（disclosure_runs で本文と末尾空白行を分離し、末尾空白行は list_projection=None で単行表示）は不変。
- 設計書末尾の「Jev CLI は認証未設定」は誤記。Jev は導入済みで、本判断はラッパー経由で取得した（設計実行時の取得失敗は Codex 読み取り専用サンドボックスのソケット遮断が原因とみられる）。

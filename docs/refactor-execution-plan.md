# Hane リファクタリング実施計画

`docs/refactor-plan.md` に定義された各フェーズを、依存関係と手戻りの少なさを基準に並べた
実施計画。フェーズ番号順ではなく、後続作業の前提を先に確定できる順序を採用する。

## 進捗ステータス（最終更新: 2026-08-28）

**現在地: R4B（Block→LayoutLine→Run）完了。次は R4C（レイアウトキャッシュ・差分更新）に着手する。**

| 実施順 | フェーズ | 状態 |
|---|---|---|
| 1 | R0 基準線の確立 | ✅ 完了（タグ `refactor-baseline` = `0111cdc`） |
| 2 | R0.5 契約テスト固定 | ✅ 完了 |
| 3 | R1 死蔵・重複コード削除 | ✅ 完了 |
| 4 | R2 前半（計測ハーネス分離） | 🔶 ほぼ完了（下記残タスクあり） |
| 5 | R3 Markdown 解析の単一化 | ✅ 完了（fixture 一部は R3.25 で拡充） |
| 6 | R3.25 Markdown 拡張契約 | ✅ 完了 |
| 7 | R3.5 revision 付き BlockIndex | ✅ 完了 |
| 8 | R3.75 DocumentSession / FileService | ✅ 完了 |
| 9 | R4A ブロック仮想化・描画 | ✅ 完了 |
| 10 | R4B Block→LayoutLine→Run | ✅ 完了 |
| 11 | R4C レイアウトキャッシュ・差分更新 | ⬜ 未着手 ← **現在地** |
| 12 | R2 後半（スクリプト統合・文書整理） | ⬜ 未着手（計画上 R4C 後） |
| 13 | R5 型・API・ドキュメント整理 | ⬜ 未着手 |

### R3.25 の完了内容

- ✅ 表示契約の漏れ解消（第一段）: fenced-code の表示決定を `presentation::present_polished_line`
  へ集約し、UI 側（`ui::line`）の `block.kind = CodeBlock` 直接パッチと style run 手注入を撤去。
  UI へ渡す block 文脈を型付き `LineContext`（Normal/FencedCode/Table）に統一。
- ✅ `Unsupported` / raw-source fallback を正式な表示契約に含める（未実装構文でも source を失わない）。
  `presentation::BlockKind::Unsupported` と `present_raw_source` を追加し、`present_markdown_with_disclosure`
  は marker 導出が source range を tile できない場合に raw-source へ降格。契約テスト
  `unsupported_and_edge_constructs_never_lose_source`（raw HTML / autolink / footnote 参照 /
  task list / escape / entity）で source 復元と往復を固定。
- ✅ parser 構文種別 / presentation 表示種別 / UI 描画方針の型を3層に分離。
  `markdown::NodeKind`（構文のみ）→ `presentation::BlockKind` / `StyleKind`（表示種別）→
  `presentation::BlockDisplay` / `InlineDisplay`（描画方針: font scale・weight・surface/tint role・
  monospace 等）。UI は `BlockDisplay` / `InlineDisplay` だけを適用し、`BlockKind` を一切 match しない
  （`crates/ui` `crates/app` に `NodeKind` / `BlockKind` / `StyleKind` の出現なし）。
  `visual_offset_at_x` の shape 経路と描画経路も `inline_display_for` の1経路に統合。
- ✅ flat `blocks`/`spans` を `markdown::MarkdownTree`（block/inline node ツリー）へ置換。
  親子・document 順・depth・source range を全ノードが保持し、list→item→paragraph、quote→paragraph、
  table→head/row→cell、task list の checkbox 状態（`ListItem { task }`）、nested list の
  `list_depth` を表現できる。未モデル構文は `NodeKind::Unsupported` として range を保持し、
  event stream を取りこぼさない。`Options::ENABLE_TABLES` を有効化し parser 設定は `parser_options()` の1か所。
- ✅ Markdown feature 共通 fixture 形式（`crates/presentation/tests/support/mod.rs`）。
  1 fixture が parse tree のチェーン・marker range・表示種別・visual text・全カーソル位置での
  SourceMap 往復と正規化の冪等性・保存後 bytes を同時に検証する。harness は `EditorView` と同じ
  3 呼び出し（`parse_block_context` → `LineContext::from_document_context` → `present_polished_line`）
  だけを行い feature 固有分岐を持たないため、fixture が通ること自体が UI 非依存の証明になる。
- ✅ 初期拡張対象（task list、nested list、複数行 quote、複数行 fenced code、image、table、link）で
  API 試行（`crates/presentation/tests/markdown_features.rs`）。UI crate の変更は不要だった。
  副産物として、文書全体 parse 時に fenced code の marker がコードブロック全体を飲み込む
  marker 導出のバグを発見・修正（開始/終了 fence 行のみを marker とする）。

### R3.5 の完了内容

- ✅ `hane_markdown::BlockIndex`。top-level block が文書を tile し（block 間の空行は上の block に
  属する）、stable `BlockId`・構文種別・解析 revision・`Confidence`（Formal/Provisional）を持つ。
  絶対 offset ではなく byte 長を保持し、chunk（128 block）単位の合計だけを Fenwick tree に載せる
  ため、block 内編集は1つの長さ更新で済み、後続 block へ書き込みが発生しない。設計判断は
  ADR-0018。
- ✅ 増分更新（`BlockIndex::update`）。編集はそれを含む block に吸収し、再解析の窓は
  dirty run の前後1 block。窓の最後の block が既存の終端 block と同じ開始位置・種別で
  終わったときに再同期成立とみなす（CommonMark の継続規則を再実装しない）。不一致なら
  窓を後ろへ伸ばし、256 KiB / 512 block で打ち切って後続を保守的に `Provisional` へ落とす。
  invalidation は必ず接尾辞なので開始 ordinal を1つ持つだけで表現する。
- ✅ publish 優先順位（`BlockIndexState`）。新しい revision 優先、同一 revision では Formal が
  Provisional に勝ち、逆は起きない。古い候補は破棄、受理した候補は現在 revision へ持ち上げてから
  publish し、履歴が無く rebase できない候補は stale のまま publish しない。遅れて到着した
  背景 job の結果は ADR-0005 の部分 publish 規則どおり rebase して publish する。
- ✅ UI 配線。`EditorView` が `BlockIndexState` を持ち、入力経路で増分更新、背景 job（旧
  `schedule_block_context` を `schedule_document_parse` へ改称）が line context と Formal 索引を
  同時に生成する。`EditorView::block_at_line` で各行の正式/暫定 block を取得できる。
  副産物として、前の文書向けに走っていた背景 job が新しい文書へ publish しうる問題を
  document generation で塞いだ。
- ✅ 計測。`BlockIndexUpdate` が更新時間・再解析 byte 数・invalidate block 数を返し、
  instrument build は metrics CSV の `block_index` record（3列追加）へ出力する。
  `hane-bench` に再現シナリオを追加。100k block の文書で打鍵 median 2.5 µs / p95 4.2 µs、
  block 分割 median 5.4 µs / p95 6.4 µs、1編集の再解析は 1 KiB 未満、invalidate 0。

### R3.75 の完了内容

- ✅ 新 crate `hane-session`（GPUI 非依存、`document` / `editor` のみに依存）。ファイル状態・
  永続化・I/O 境界をここへ集約した。設計判断は ADR-0019。
- ✅ `FileIdentity`。読み書きに使う path と同一性判定用の canonical path を分離し、表示名は
  判定に関与しない。`.` / `..` の正規化は fs に触れずに行うため、存在しない Save As 先も
  同一ファイル判定に載る。rename/move は path だけを差し替え、削除・外部変更は
  `FileStamp`（長さ + 更新時刻）比較の `ExternalChange` として表す。
- ✅ `FileService`（`load` / `save` / `stamp`）が唯一のファイル境界。atomic write（一時ファイル +
  rename）はここだけが行う。上書き可否は `OverwriteGuard` を job が運び、`ExpectStamp` は
  書き込み直前に disk を確認して不一致なら書かずに `ExternalChange` を返す。起動時の初回読み込み
  以外の open / save はすべて background executor へ出すため、入力処理がファイル I/O を待たない。
- ✅ `DocumentSession`。editor・`FileState`・保存済み revision・autosave 世代・実行中/待機中の
  保存を持ち、I/O はせず要求（`SaveDecision` / `OpenDecision` / `FileEventOutcome`）を返す。
  document 差し替えごとに増える `generation` で、派生状態と実行中 job の有効性を判定する。
  保存は同時に1つで、実行中の追加要求は最後の1つだけを queue する。`SaveTicket` に一致しない
  結果は `Superseded` として捨てる。外部変更・削除は自動解決せず、dirty な session の内容を
  失わない。
- ✅ `SessionSet`。open 要求を「既に開いている → 切替」「clean な active → 差し替え」
  「dirty な active → 拒否」へ振り分ける。同じファイルを開き直しても未保存編集は破棄しない。
  最後の1 session を閉じると untitled へ置き換わり、window が空にならない。
- ✅ 永続設定を `SettingsRepository` / `RecentFilesRepository` の2 trait に分け、`StateStores`
  が `Arc` handle として渡す。store は view より長生きしてよく、view の生成・破棄に従属しない。
  filer tree state は3つ目の repository として足せる。
- ✅ `ResourceResolver`。相対画像の解決を `EditorView::document_directory` の計算から外し、
  session の file identity を基準に行う。untitled 文書は base を持たず working directory を借りない。
- ✅ UI 配線。`EditorView` は `sessions: SessionSet` と `files: Arc<dyn FileService>` を持ち、
  `PathBuf` フィールド・atomic save・recent-files 永続化を所有しない。`crates/ui` から
  `storage.rs` を撤去し、`editor` フィールドは `editor()` / `editor_mut()` 経由になった。
  `activate_session` で scroll 位置を持ち回しながら session を切り替えられる。
- ✅ 競合規則の UI 非依存テスト（`crates/session/tests/conflict_rules.rs`、16 本）。
  未保存時の open 拒否、既に開いているファイルの切替、保存の直列化と queue 畳み込み、
  document 差し替え後の結果破棄、autosave の世代・revision 判定、外部変更時の上書き拒否と
  確認後の上書き、rename/delete の追従、読み込み失敗、close 拒否、session 切替時の状態保持。
- ✅ 性能。`hane-bench buffer` は R3.5 記録と同水準（打鍵時 block index median 3 µs / p95 4 µs、
  block 分割 median 5 µs / p95 7 µs、再解析 980 bytes 以下、invalidate 0）。

### R4A の完了内容

- ✅ 型を「ブロック」と「行」に分離。従来の1行単位 `VisualBlock` を `VisualLine` へ改名し、
  `VisualBlock` を Markdown ブロックの単位として作り直した。`BlockId`・表示種別・複数行に
  またがる `source_range`・revision・`Confidence` と、その中の `VisualLine` を持つ。
  R4A の互換レイヤはこの `lines` で、カーソル・選択・IME・クリックは物理行を指したまま。
  設計判断は ADR-0020。
- ✅ 表示文脈をブロック種別だけから決める。`presentation::block_line_context` が唯一の判定点で、
  `BlockContextIndex` / `parse_block_context` / `LocalBlockContext` / `local_block_context` と
  行走査（`scan_block_context` / `is_pipe_row`）を削除。`crates/ui` はフェンスもパイプも見ない。
  ブロック末尾の空行列（タイル化が上のブロックへ寄せた分）だけは Normal で描くため、
  閉じ fence 直後の空行はコード背景にならず、コードブロック内部の空行はコードのまま。
- ✅ 正式 `BlockIndex` が無い間の境界を `hane_markdown::local_block_index` に置き換え。
  viewport の上に固定行数の lookback を取った窓を `parse_document` で解析してタイル化する。
  100 MB 文書で 1 回 0.25〜0.44 ms、全ブロック `Provisional`。
- ✅ ブロック単位の仮想化。`render` はブロックを反復し、`HeightIndex` は 1 エントリ 1 ブロック。
  正式索引が無い起動直後だけ行粒度で持ち、粒度切替は viewport 上端の source offset を
  掛け直すので読んでいる位置が動かない。
- ✅ 可視行でのクリップ。ブロックに大きさの上限は無く、空行を含まない文書は1段落になる
  （`paragraphs_100k.md` は10万行で1ブロック）。`present_block` は viewport と交差する行だけを
  構築し、上下の行数を `lines_before` / `lines_after` として高さに算入する。
- ✅ ブロックの行数を索引が持つ（`IndexedBlock::line_count`）。タイル化時に数えるので
  高さ索引の再構築が rope 走査を伴わない。10万ブロックで 21.1 ms → 0.635 ms（行単位だった
  従来の再構築 20万行 約1.5 ms より速い）。`hane-bench buffer` に
  `block height index rebuild` シナリオを追加。
- ✅ 正式索引公開時の高さ索引構築（100 MB・約265万ブロックで約 39 ms）を全文解析と同じ
  背景 job へ移し、main thread は差し替えるだけにした。
- ✅ 契約。`block_line_counts_tile_the_document_through_edits`（増分更新後も行数が文書を
  タイルする）、`a_block_taller_than_the_viewport_presents_only_the_visible_lines`、
  `one_block_covers_every_physical_line_of_its_construct` を追加。R3.25 の fixture harness は
  `BlockIndex` → `present_block` の 2 呼び出しへ更新し、`EditorView` と同じ経路を保った。
  fixture の期待値は変更していない。
- ✅ 検証。`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` 緑。
  `hane-bench buffer` は R3.5 と同水準。同一文書の本文領域の描画は master と一致
  （画面キャプチャ比較。100 MB / 10万段落・1ブロック文書でも正しく描画される）。

### R4B の完了内容

- ✅ ブロックと run の間に「画面上の1行」`LayoutLine` を入れた（`hane_presentation::layout`）。
  物理行1本、または折り返された物理行の1断片で、行ローカル/ブロックローカルの visual range・
  受け持つ source range・`y`/`height`・`Hard`/`Soft` の切れ目を持つ。行はその物理行の source を
  タイルし、物理行はブロックをタイルするので、source の1 byte は必ず1つの行に属する。
  設計判断は ADR-0021。
- ✅ フォントに聞くのは折り返し位置と x だけにした（`LineShaper` の3メソッド）。UI は window の
  text system（`ui::shape::WindowShaper`、`shape_text` の wrap boundary と `shape_line` の
  `x_for_index` / `closest_index_for_x`）、テストは1文字固定幅の
  `hane_presentation::testing::FixedAdvanceShaper` で実装する。行の始端・終端・受け持つ source・
  位置はレイアウト側が決めるため、座標契約が window 無しでテストできる。
- ✅ カーソルの質問を `BlockLayout` の1か所へ集約。`row_for_source` / `point_for_source` /
  `source_for_point` / `source_at_x` / `vertical_target` / `visual_range_on_row` /
  `row_bounds_for_source`。soft の切れ目の offset は次の行の先頭に属し、行末位置を持てるのは
  `Hard` で終わる行だけ、という規則で caret の描画位置とヒットテストが一致する。
- ✅ 上下移動を preferred grapheme column から preferred x へ移行。`Editor::preferred_visual_x` と
  `move_vertical_to` が入口で、移動先の決定は `EditorView::move_vertical` が caret のブロック
  （前後1行を含む）をレイアウトして行う。ブロックの端に達したときだけ隣のブロックの1行を
  present する。索引がまだ無い起動直後は `EditorCommand::MoveUp` / `MoveDown` がフォールバック。
- ✅ クリック・ドラッグは行断片の中だけを測る（`offset_at_row_x`）。折り返し行の右端より先を
  クリックしても次の行のテキストへは届かない。選択と IME 下線は `visual_range_on_row` で
  行ごとに切り出すので、折り返しをまたぐ選択が両方の行に出る。
- ✅ 高さと caret 矩形をレイアウトから取るようにした。`HeightIndex` の更新値は
  `BlockLayout::height()`（行粒度では `line_height_of`）で、折り返した行はその行数分の高さを
  占める。`bounds_for_range` は最後のフレームが描いた caret 矩形（`CaretGeometry`）を返すため、
  IME 候補ウィンドウがカーソル位置に出る。
- ✅ レイアウトをブロック単位でキャッシュ（`layout_cache`、保持規則は `block_cache` と同じ）。
  presentation を再利用できたフレームでは幅と revision が一致する限りレイアウトも再利用し、
  ブロックに触れない編集では `BlockLayout::rebase` で source range だけを移送する。
  上下移動も、caret のブロックが直前のフレームで描かれていれば parse も shape もしない。
- ✅ 契約テスト `crates/presentation/tests/layout_contract.rs`（8本）。折り返し段落・引用・
  リスト・コード・表を含む文書の全 source offset で「source → 点 → source」が同じ offset に
  戻ること（source map が隠す offset は編集可能位置ではないため対象外）、行が物理行の source と
  visual text をタイルすること、`Soft`/`Hard` の区別、折り返し位置の caret が次の行の先頭に
  出ること、短い行を通過しても preferred x が戻ること、ブロック端が `PastEdge` を返すこと。
  UI 側にはブロックをまたぐ上下移動（`neighbor_row_target`）と行断片の描画範囲のテストを追加。
- ✅ 計測。`hane-bench buffer` に `viewport block layout` を追加（10万ブロック文書の viewport
  14 ブロックを present + layout、固定幅シェイパで median 0.048 ms / p95 0.051 ms）。
  既存シナリオは R4A・R3.5 と同水準（打鍵時 block index median 3 µs / p95 4 µs、block 分割
  median 6 µs / p95 8 µs、再解析 980 bytes 以下、invalidate 0、block height index rebuild 0.647 ms）。
- ✅ 検証。`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` 緑。
  instrument build を 100 MB 文書と `paragraphs_100k.md` で起動し、新しい描画経路で
  `hane_ready` とペイントまで到達することを確認した。

### R4B の残メモ

- 🔶 レイアウトは1フレームに1回テキストを shape し、GPUI も描画時に shape する。可視行の
  shape が二重になっている。解消は R4C（cache entry に shape 結果を持たせる）。
- 🔶 GUI の画面キャプチャ比較と `keystroke_to_paint` 実測は未実施。この環境では window を
  前面にできず、`CGWindowListCopyWindowInfo` も screen recording 権限が無いため window を
  取得できなかった（25 秒の autoscroll で paint record 2 件）。R4A・R2 前半の残タスクと
  同じ枠で回収する。折り返し・caret・選択の目視確認もそこに含める。
- 🔶 `HeightIndex` の初期値は折り返しを知らない（`block_heights` は行数 × 行高）。描画された
  ブロックから実測値へ置き換わるので、スクロール範囲は読み進めるにつれて正確になる。
  ブロック内の可視行の見積もりは、そのブロックを一度描いていれば実測の平均行高を使う。

### R4A の残メモ

- 🔶 GUI の `keystroke_to_paint` / scroll frame interval の master 比較は未実施。window が
  前面でない環境では OS 側の throttle に支配され、frame 数が同条件で 145〜1304 と振れるため
  判定に使えなかった。アプリ内計測の `layout_ms` は 100 MB / 10万段落とも master 以下
  （中央値 0.16〜0.20 ms 対 0.24〜0.26 ms）。R2 前半の残タスクと同じ枠で回収する。
- ✅ ブロック内の行位置とスクロール位置の対応は R4B の `LayoutLine` が持つようになった
  （描画済みブロックは実測の平均行高、未描画は行高一定の見積もり）。
- 🔶 ブロック数が変わる編集では `HeightIndex` を作り直す。差分 splice は R4C の担当。

### R3.75 の残メモ

- 🔶 GUI 経由の `keystroke_to_paint` 実測（instrument build + 入力注入）は未実施。R2 前半の
  残タスクと同じ枠で回収する。
- 🔶 外部変更の検出は `FileEvent` を受け取る API までで、ファイル監視そのものは未実装。
  filer 実装時に watcher を足す。

### R3.5 の残メモ

- ✅ ブロック境界の二重管理は R4A で解消した（`BlockContextIndex` 系を削除）。
- 🔶 背景の全文解析は `full_text()` の String を作る。100 MB 文書での CPU / RSS 影響は
  R4C 前の実測で確認する。R4A 時点の実測では `BlockIndex::from_buffer` が 100 MB で
  約 1.16 秒（背景 job・1回）。

### R2 前半の残タスク

- 🔶 `keystroke_to_paint` timing hook の instrument on/off 実測比較が未実施
  （コード上は両ビルド同一 paint 経路に維持済み）。

### R2 後半に回すタスク（計画上 R4C 完了後）

- ⬜ `metrics` と `benchmark` の役割再定義・`Distribution`/`percentile` 統合。
- ⬜ スクリプト引数化統合（`scripts/measure.sh <scenario>` 等）。
- ⬜ 歴史文書の `docs/history/` 移動と ADR 索引整備。

> 各フェーズの詳細チェックボックスは `docs/refactor-plan.md` が正。
> このステータス欄はフェーズを着手・完了するたびに更新する（状態と「最終更新」日付、
> 「現在地」行を必ず直す）。`refactor-plan.md` のチェックボックスと本表の状態が
> 食い違ったら本表を修正して同期する。

## 推奨実施順

1. **R0 — 性能・API基準線の確立**
2. **R0.5 — source↔visual、IME、複数行構文の契約テスト追加**
3. **R1 — 死蔵コード・重複ヘルパの削除**
4. **R2 前半 — timing hookを残し、合成入力・CSV・開発APIだけ分離**
5. **R3 — Markdown解析とマーカ導出を `hane-markdown` に統合**
6. **R3.25 — Markdown機能拡張用の解析・表示APIを確定**
7. **R3.5 — revision付き `BlockIndex` を導入**
8. **R3.75 — `DocumentSession` / `FileService` を `EditorView` から分離**
9. **R4A — ブロック単位の仮想化・描画へ移行**（完了）
10. **R4B — `Block → LayoutLine → Run` とvisual座標移動を実装**（完了）
11. **R4C — レイアウトキャッシュと差分更新を実装**
12. **R2 後半 — スクリプト統合、環境変数整理、歴史文書移動**
13. **R5 — 型、公開API、ドキュメントの最終整理**

## 実施方針

- R0とR0.5で性能・公開API・編集表示契約を固定し、以降の全フェーズの回帰ゲートにする。
- R1で不要コードと重複を減らしてから、責務境界やデータモデルの変更に着手する。
- R2は前半と後半に分割する。前半では製品経路に必要な低オーバーヘッドのtiming hookを残し、
  合成入力、CSV出力、開発専用APIを隔離する。スクリプトや歴史文書の整理は、構造変更後の
  実態に合わせられるようR4C完了後に行う。
- R3からR3.5までで、Markdown解析・表示契約・ブロック境界を順に確定する。
- R3.75でファイル状態と永続化を描画責務から分離し、ファイラー実装の土台を作る。
- R4AからR4Cは、仮想化、visual座標系、差分キャッシュの順に独立して導入する。R4Aで
  ブロック境界が唯一の表示文脈になり、要素生成が可視ブロック数（ブロック内では可視行数）に
  比例するようになった。R4Bでカーソル・選択・IME・クリック・上下移動が同じ座標変換
  （`BlockLayout`）を通るようになり、折り返しが表示・入力の両方で成立している。
- R5は構造変更が完了した後の実装を正として、型、公開API、ドキュメントを整理する。

## 依存関係

```text
R0 → R0.5 → R1 → R2前半 → R3 → R3.25 → R3.5 → R4A → R4B → R4C
               └────────────────────────→ R3.75

R4C → R2後半 → R5
```

R3.5とR3.75は並行実施できる。順番に進める場合は、ブロック境界の基盤を先に確定できる
`BlockIndex → DocumentSession / FileService` の順とする。

## 機能追加の開始条件

| 機能追加の種類 | 開始できる地点 | 理由 |
|---|---|---|
| ファイラー | R3.75完了後 | ファイル状態とI/Oを `EditorView` に再び密結合させずに実装できる |
| 単純なインラインMarkdown機能 | R3.5完了後 | 解析・表示APIとrevision付きブロック境界が確定している |
| リスト、引用、コード、表などの複数行機能 | R4B完了後 | 複数行ブロックのレイアウトとvisual座標移動が成立している |
| 大規模な機能追加 | R4C完了後を推奨 | レイアウトキャッシュと差分更新を含む最終的な性能構造が安定している |

開始条件に達する前でも調査やspikeは実施できる。ただし、製品経路に暫定的な文字列判定、
物理行前提の表示分岐、`EditorView` が直接所有するファイル状態を追加しない。

## 各段階のゲート

各フェーズは独立してコミット・検証できる単位とし、次の条件を満たしてから次段階へ進む。

- R0で固定したテスト、性能原本、公開APIスナップショットとの差分を確認する。
- R0.5で追加したsource↔visual、IME、複数行構文の契約テストを通す。
- `keystroke_to_paint` p95/p99などの主要指標が、定義した許容回帰率を超えていないことを確認する。
- 回帰がある場合は次フェーズで吸収せず、原因を当該フェーズ内で特定して解消する。

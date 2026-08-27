# Hane リファクタリング実施計画

`docs/refactor-plan.md` に定義された各フェーズを、依存関係と手戻りの少なさを基準に並べた
実施計画。フェーズ番号順ではなく、後続作業の前提を先に確定できる順序を採用する。

## 進捗ステータス（最終更新: 2026-08-27）

**現在地: R3.5（revision 付き BlockIndex）完了。次は R3.75（DocumentSession / FileService）に着手する。**

| 実施順 | フェーズ | 状態 |
|---|---|---|
| 1 | R0 基準線の確立 | ✅ 完了（タグ `refactor-baseline` = `0111cdc`） |
| 2 | R0.5 契約テスト固定 | ✅ 完了 |
| 3 | R1 死蔵・重複コード削除 | ✅ 完了 |
| 4 | R2 前半（計測ハーネス分離） | 🔶 ほぼ完了（下記残タスクあり） |
| 5 | R3 Markdown 解析の単一化 | ✅ 完了（fixture 一部は R3.25 で拡充） |
| 6 | R3.25 Markdown 拡張契約 | ✅ 完了 |
| 7 | R3.5 revision 付き BlockIndex | ✅ 完了 |
| 8 | R3.75 DocumentSession / FileService | ⬜ 未着手 ← **現在地** |
| 9 | R4A ブロック仮想化・描画 | ⬜ 未着手 |
| 10 | R4B Block→LayoutLine→Run | ⬜ 未着手 |
| 11 | R4C レイアウトキャッシュ・差分更新 | ⬜ 未着手 |
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

### R3.5 の残メモ（R4A で回収する）

- 🔶 UI は依然として行描画で、fenced/table の line context も `BlockContextIndex` から取っている。
  BlockIndex と二重に持っている状態は R4A（`fenced_code_context` / `table_context` 撤廃）で解消する。
- 🔶 背景の全文解析は `full_text()` の String を作る。100 MB 文書での CPU / RSS 影響は
  R4C 前の実測で確認する。

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
9. **R4A — ブロック単位の仮想化・描画へ移行**
10. **R4B — `Block → LayoutLine → Run` とvisual座標移動を実装**
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
- R4AからR4Cは、仮想化、visual座標系、差分キャッシュの順に独立して導入する。
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

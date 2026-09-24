# ADR-0030: gpui-kit / gpui-base の段階的・局所的採用

## ステータス

提案

GPUI 世代整合を更新理由として認める方針は本 ADR で決定する。基盤更新と部品採用は
別 PR・別検証とし、両者の採用条件が満たされるまでは、部品採用の状態を「提案」のまま維持する。

## 日付

2026-09-24

## 関連

- [ADR-0009: Performance Harness と測定基準](0009-performance-harness.md)
- [ADR-0014: GPUI Memory Baseline](0014-gpui-memory-baseline.md)
- [ADR-0020: ブロック単位の仮想化と描画](0020-block-virtualization.md)
- [ADR-0021: LayoutLine と visual 座標系](0021-layout-lines-and-visual-coordinates.md)
- [ADR-0022: レイアウトキャッシュの無効化と高さ差分更新](0022-layout-cache-invalidation.md)
- [ADR-0025: 複数行 inline presentation の shared parse](0025-shared-parse-for-multiline-inline-presentation.md)
- [ADR-0029: source-first list editing](0029-source-first-list-editing.md)
- [Hane architecture](../architecture.md)

## 背景

Hane の価値は、巨大な Markdown 文書でも入力と表示が止まらない「羽のような軽さ」にある。
本 PR の基準となる `main`（2026-09-24、`383b166c`）は `gpui = "=0.2.2"` を固定し、さらに
`[patch.crates-io]` で `vendor/gpui` を同じ `0.2.2` のローカル実装へ差し替えている。Hane 本体は、
次の経路を分離している。

```text
Markdown source / RopeBuffer
        ↓
BlockIndex / Markdown parse
        ↓
VisualBlock / BlockLayout / LayoutLine
        ↓
visible rows の GPUI rendering
```

`gpui-kit` と `gpui-base` には、Tree のような仮想化された階層リストや Tabs のような再利用可能な
UI 部品がある。一方、異なる世代の GPUI を依存させると、同名でも互換性のない GPUI 型がプロセス内に
入り、単一部品の導入が GPUI 本体の更新や Hane の描画経路の置換へ拡大する可能性がある。

2026-09-24 に crates.io の公開メタデータとパッケージ manifest を再確認した結果、依存関係は次のとおり
である。

| 対象 | 確認した依存関係 | Hane との関係 |
|---|---|---|
| Hane 移行前 `main` `383b166c` | `gpui = "=0.2.2"`、`vendor/gpui` を `[patch.crates-io]` で使用 | 比較対象の型世代。`HANE-PATCH.md` に macOS の合成斜体と IME 再同期のパッチを記録している |
| [`gpui-kit 0.6.6`](https://crates.io/crates/gpui-kit/0.6.6) | `gpui-base = 0.6.6`、`gpui-pre = "=0.3.6"`、`gpui-pre-platform = "=0.3.6"`、`gpui-pre-web = "=0.3.6"` | Hane の `gpui 0.2.2` とは別の GPUI 配布系列 |
| [`gpui-base 0.6.6`](https://crates.io/crates/gpui-base/0.6.6) | `gpui-pre`、`gpui-pre-macros`、`gpui-pre-sum-tree` をいずれも `=0.3.6` に固定 | Tree 等の基盤部品を使う場合も `0.3.6` 世代が境界になる |

したがって、gpui-kit の継続利用を選ぶ場合に必要なのは、`gpui 0.2.2` の数字だけを上げる変更ではなく、
`gpui-pre` 一式を含む GPUI 世代の移行である。これは、gpui-kit を将来も利用可能にするための GPUI 基盤更新を
正当化する十分な理由とする。ただし、その基盤更新は gpui-kit の Tree や Tabs などの採用とは別の PR・別の
検証として実施する。

したがって、見た目の改善を理由に最新の部品をまとめて導入するのではなく、Hane が所有する Model・編集・
Markdown presentation・layout を維持したまま、性能と境界を確認できる部品だけを段階的に試す必要がある。

## 決定

### 1. 性能を最優先し、Hane 本文エディタは置き換えない

`gpui-kit` は Hane の標準 UI framework にはしない。採用するとしても、対象を一つの局所的な描画・
相互作用部品に限定し、次の Hane の責務は維持する。

- `RopeBuffer` を唯一の Markdown source Model とする。
- selection、caret、IME、undo/redo、source edit、`DocumentSession`、File I/O を Hane が所有する。
- `BlockIndex`、presentation、`BlockLayout`、`LayoutLine`、revision-bound cache、visible-row
  virtualization を Hane の既存経路として維持する。
- Hane 本文の Editor と Markdown rendering pipeline を `gpui-kit` の Editor や完成済み画面で置き換えない。

`gpui-base` の動作部品を使う場合も、行やタブの見た目は Hane 側の renderer で構成できることを優先する。
完成済みの styled component を使う場合も、Hane 固有のテーマ・操作・状態を失わない局所的な範囲に限る。

### 2. 対象ごとに採用境界を分ける

| 対象 | 方針 | Hane 側に残す責務 |
|---|---|---|
| ファイル・フォルダ Tree | GPUI 世代を合わせられた後の将来候補。まず `gpui-base` の Tree の仮想化と階層操作だけを検証する | `WorkFolder` の Model、展開状態、選択、検索結果による展開、インライン名前変更、アイコン、日付表示、行 renderer |
| ファイル Tabs | 将来候補。セッション管理と見た目の描画を分離できる場合だけ、Tabs の表示・overflow 操作を局所採用する | `DocumentSession`、active session、close/context menu、選択状態、キーボード操作、タブ内容の identity |
| コードブロックのハイライト | `gpui-kit` の Editor は埋め込まない。Hane の表示中ブロック処理へ統合する | source、code block の block identity/revision、visible-block 判定、fallback 表示、layout、caret/selection |
| Hane 本文 Editor / Markdown 描画 | 置き換えない | 全責務 |

Tree の行は、概念上次のように Hane の要素をそのまま描画できるものとする。

```text
file/folder icon │ name │ Hane の date badge
```

日付バッジは `gpui-kit` の汎用 `Badge` に置き換えない。`本日` や曜日を含む Hane の日付表示は、Tree の
行 renderer の子要素として維持する。

コードブロックの色分けは、次の Hane の表示経路に統合する。

```text
visible code block
        ↓
revision-bound highlighter（必要なら Tree-sitter 系の parser）
        ↓
Hane presentation の style spans
        ↓
BlockLayout / LayoutLine
        ↓
GPUI rendering
```

画面付近のコードブロックだけを解析し、解析結果が届くまでは通常のコード表示へ fallback する。全文を
別 Editor に複製せず、gpui-kit Editor の選択・バッファ・caret・レイアウトを Hane の本文へ持ち込まない。
ハイライト結果は source revision と block identity に結び付け、stale な結果を現行表示へ適用しない。

### 3. GPUI 世代差を導入ゲートにする

`gpui-kit` / `gpui-base` の導入前に、候補部品と Hane が同じ GPUI 型を使えることを確認する。

- Hane の固定版、`vendor/gpui` のパッチ、候補部品の直接・推移依存を一覧化する。
- `gpui` と `gpui-pre` など別系列の型を同一の Hane UI 境界へ持ち込まない。
- gpui-kit を継続利用するために、その依存する GPUI 世代へ Hane を合わせる必要が生じた場合は、GPUI 更新の
  正当な理由とする。その他の機能・修正上の理由による更新も同様に扱う。
- GPUI を更新する場合は、まず GPUI 基盤だけを更新する独立した PR として、API 追随、依存グラフ、性能、
  GUI、macOS/Windows の検証を完了する。GPUI 更新 PR と gpui-kit 部品採用 PR を一つの変更で同時に進めない。
- `vendor/gpui/HANE-PATCH.md` に記録された macOS の合成斜体フォールバックと入力ソース切替後の IME 再同期を、
  新しい GPUI 世代へ移植して再検証する。上流で修正済みと判断してパッチを削除する場合も、実字形と実 IME を
  用いた同等の回帰確認を先に行う。
- 同じ世代へ合わせられない候補は、性能を測る前に不採用とし、現在の Hane 実装を継続する。

### 4. ベンチマークを採用ゲートにする

候補部品は、見た目が改善しただけでは採用しない。現行経路と候補経路を、同じ hardware、OS、Rust/GPUI
version、release profile、fixture、入力源、refresh-rate 条件で比較する。測定は既存の
`hane-benchmark`、`scripts/measure.sh`、[baseline](../baseline/README.md) の手順と回帰基準を使う。

対象部品に応じて、少なくとも次を測定する。

- 起動、file open、通常入力、100 MB 入力、100k paragraphs、scroll 中の入力。
- `keystroke_to_frame` の p95/p99、frame interval、visible layout、parse / presentation、
  block-index / cache 更新時間。
- 条件ごとの RSS と、空 Editor の GPUI baseline との差分。
- Tree では大量の展開済みファイル・フォルダ、Tabs では多数の session と overflow、コードハイライトでは
  大きな visible code block の scroll / input / background parse 中の表示。

既存の baseline に対する判定ルールを適用し、p95/p99、startup、file-open、RSS の回帰が同条件の再測定でも
解消しない候補は採用しない。新しい指標が必要な場合は、候補だけに都合のよい閾値を作らず、control と
candidate の結果、commit、環境、fixture、サンプル数を記録してから判断する。性能 evidence が不足する
場合は「採用可能」とみなさず、ADR のステータスを提案のまま保持する。

### 5. 段階的な導入順序と撤回可能性

実装は次の順序で進める。

1. 現行 GPUI 0.2.2 のまま、Hane 側の見た目だけを改善する。
2. gpui-kit の継続利用のために世代整合が必要になった場合、または別の明確な理由で更新が必要になった場合は、
   GPUI 基盤だけを更新する独立 PR を作る。ここでは gpui-kit の部品を導入しない。
3. GPUI 基盤 PR で `gpui-pre` 一式への依存整合、`vendor/gpui` パッチの移植・再検証、性能・GUI・macOS/Windows
   検証を完了し、現行 Hane の本文 Editor と Markdown 描画が成立することを確認する。
4. 世代が整った後、別 PR で `gpui-base` Tree を小さな adapter と Hane の行 renderer 越しに検証する。
5. Tree の結果が基準を通った場合だけ、別 PR で Tabs を同じく局所的に検証する。
6. コードハイライトは Editor の埋め込みとは独立に、Hane の visible-block presentation へ追加する。

各段階で既存経路を control として残し、候補部品を外しても Hane の Model・編集・表示が成立する構造にする。
採用後に回帰が確認された場合は、対象 adapter または候補部品を撤回し、Hane の既存 renderer へ戻せることを
必要条件とする。

## 結果

- Hane の本文編集、Markdown source、selection/IME、revision/cache、visible-block rendering は
  `gpui-kit` の導入から隔離される。
- Tree は仮想化の恩恵と Hane 固有の日付バッジ・行表示を両立できる可能性があるが、GPUI 世代整合後まで
  保留される。
- Tabs はセッション管理を Hane に残したまま見た目を改善できる可能性がある。ただし Tabs のためだけに
  GPUI 本体を更新する場合も、先に GPUI 基盤更新 PR として独立した検証を完了する。
- コードハイライトは別 Editor の複製状態を増やさず、表示中のブロックだけを処理できる。
- GPUI 更新、候補部品、Hane の既存実装のどれが性能へ影響したかを、段階ごとの control/candidate 比較で
  分離できる。
- 一方、候補ごとに adapter、行 renderer、ベンチマーク fixture を保つ追加コストが発生する。性能 evidence
  が取れない候補は、そのコストを受け入れて採用しない。

## 採用時の確認項目

提案を採用済みに変更し、候補を実装へ進める前に、次を記録する。

- GPUI と候補部品の正確な version / commit、`vendor/gpui` パッチとの互換性、重複 GPUI 型がないこと。
- `vendor/gpui` の合成斜体について、macOS の実フォントで通常字形・斜体字形・字送り・キャッシュ分離・文字切れを
  再確認すること。IME 再同期について、macOS の実 IME で入力ソース切替後の preedit と commit を確認すること。
- macOS と Windows の release build、起動、ウィンドウ表示、キーボード入力、スクロール、対象 UI の GUI smoke
  検証を、それぞれの実行環境または同等の CI 環境で記録すること。
- Hane が Model、編集、session、presentation、layout、cache、行 renderer を引き続き所有すること。
- control/candidate の測定条件、fixture、commit、サンプル数、median/p95/p99/max、RSS。
- GPUI 基盤更新 PR と gpui-kit 部品採用 PR を分け、基盤更新単体の性能・メモリ回帰と、部品採用単体の回帰を
  それぞれ識別できること。
- 既存の test、clippy、source↔visual 契約、および対象 UI の必要な GUI 検証結果。
- 候補部品を外して既存経路へ戻せる撤回手順。

この確認を満たすまでは、`gpui-kit` / `gpui-base` を依存へ追加したり、Hane 本文 Editor を置き換えたりしない。

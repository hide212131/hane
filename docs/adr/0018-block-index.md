# ADR-0018: revision 付き Block Index

## ステータス

承認済み

## 日付

2026-08-27

## 背景

Phase 4 までの UI は「1物理行 = 1ブロック」で描画しており、Markdown のブロック境界を
表示側が持っていない。R4A 以降でブロック単位の仮想化・レイアウトへ移すには、その前に
「どの byte がどのブロックに属するか」を編集のたびに更新できる索引が必要になる。

ADR-0005 と ADR-0008 は、入力経路で全文解析を行わないこと、stale な解析結果で新しい表示を
上書きしないことを既に決めている。Block Index はこの2つの規則を、ブロック境界という
具体的なデータ構造の上で満たす必要がある。

## 決定

`hane-markdown` に `BlockIndex` を置く。保持するのは top-level block（parse tree の root の子）
だけで、入れ子構造は `MarkdownTree` 側に残す。

### tiling 不変条件

block `i` は「自分の解析開始位置から次の block の開始位置まで」を占める。

```text
# head\n\npara\ncont\n\n\n- a\n- b\n
└─ Heading ──┘└─ Paragraph ──┘└─ List ─┘
```

- block 間の空行は上の block に属する。
- block 0 は offset 0 から始まり、最後の block は文書末で終わる。
- したがって全 byte はちょうど1つの block に属し、offset → block が常に決まる。
- 索引が空になるのは文書に block が1つも無いとき、すなわち文書が空白だけのときに限る。

### 保持する情報と計算量

各 block は stable な `BlockId`、構文種別（`NodeKind`）、解析時 `Revision`、
`Confidence`（Formal / Provisional）を持つ。source range は保持せず、**byte 長**を保持する。

byte 長は chunk（既定 128 block）単位でまとめ、chunk 合計だけを Fenwick tree に載せる。

- block 内編集: 1つの長さと1つの chunk 合計の更新（`O(log chunk数)`）。以降の block の
  開始位置は自動的にずれるため、触れていない block へ書き込みが発生しない。
- block 数が変わる編集: 触れた chunk だけを作り直す。文書全長の memmove を行わない。
- offset → block、ordinal → source range: Fenwick 探索 + chunk 内走査。

block ごとに絶対 offset を持つ flat 配列は、1打鍵ごとに後続全 block の offset 更新が必要になり、
block ごとに Fenwick node を持つ flat 実装は Return 打鍵のたびに文書長比例の
memmove と tree 再構築が必要になる。どちらも「通常の局所編集で文書サイズ比例の同期処理を
発生させない」という要件を満たせないため採用しない。

### 再解析の開始点と再同期条件

編集は、それを含む block に吸収される（byte 長の変更）。再解析の窓は次のように決める。

1. 編集が触れた block の run を求める（dirty run）。
2. その1つ前の block を窓に含める。空行削除や setext 下線の追加で、編集した block が
   上の block と結合しうるため。結合は高々1 block 上までしか届かない。
3. dirty run の1つ後ろの block（未編集）を窓の終端として含める。

窓を再解析した結果の**最後の block が、索引が既に持っている終端 block と同じ開始位置・
同じ種別で終わる**とき、再同期が成立したと判定する。窓が文書末に達している場合は、
後続が無いので自明に成立する。

この判定は CommonMark の継続規則を再実装せず、構造の一致だけで確認する。未閉じの
fence や未閉じの HTML block が後続を飲み込んだ場合は、最後の block の開始位置が
ずれるため、そのまま不一致として検出される。

不一致なら窓を1 block ずつ後ろへ伸ばして再試行する。伸長は `RESYNC_BYTE_BUDGET`（256 KiB）
と `RESYNC_BLOCK_BUDGET`（512 block）で打ち切る。入力経路のコストを文書サイズではなく
定数で抑えるための上限である。

### 再同期できない場合の保守的 invalidation

打ち切った場合、窓より後ろの block をすべて `Provisional` にする。

- source range は rebase 済みなので、offset 解決と表示は継続できる。
- 種別は古い可能性があるため、正式（Formal）とは主張しない。
- invalidation は必ず接尾辞になるため、開始 ordinal を1つ持つだけで表現し、
  block ごとにフラグを書き込まない。

`Provisional` を含む索引は `has_provisional_blocks()` を通じて背景の全文解析を要求する。

### publish 優先順位

`BlockIndexState` が publish 規則を1か所で持つ。

1. revision が新しい候補が優先される。
2. 同一 revision では Formal（全文解析）が Provisional（増分更新・rebase 済み）に勝ち、
   Provisional が Formal を置き換えることはない。
3. publish 済みより古い revision の候補は破棄する。
4. 受理した候補は publish 前に現在 revision へ持ち上げる。編集履歴が失われていて
   rebase できない候補は、stale のまま publish せず破棄する。

背景 job の結果が遅れて到着した場合は、ADR-0005 の部分 publish 規則どおり、非交差 block を
revision delta で rebase し、編集が触れた窓だけを再解析して publish する。rebase を経た
結果は現在 revision の全文解析ではないため、`Provisional` として publish する。

### 計測

`BlockIndexUpdate` が更新時間、再解析 byte 数、invalidate した block 数を返す。UI は
instrument build でこれを metrics CSV の `block_index` record として出力し、`hane-bench` は
同じ値を再現可能なシナリオとして測る。

## 結果

- 入力経路は全文解析を行わず、編集の周辺だけを再解析する。
- 遠くまで影響する編集（fence を開くなど）でも、表示は止まらず、古い種別を正しいと
  主張しない。
- stale な解析結果が新しい表示を上書きしない規則が、呼び出し側ではなく索引側にある。
- 一方で、再同期できない編集の後は背景の全文解析が完了するまで tail が暫定表示になる。
  これは受け入れる。

## 検討した代替案

### block ごとに絶対 offset を持つ

採用しない。1打鍵ごとに後続全 block の offset 更新が必要になる。

### block ごとに Fenwick node を持つ flat 配列

採用しない。block 数が変わる編集のたびに文書長比例の memmove と tree 再構築が発生する。
500k block の文書で 1 回の分割が 2.7 ms かかり、chunk 化により 27 µs へ下がることを実測した。

### 再同期条件を CommonMark の継続規則で判定する

採用しない。空行・遅延継続・HTML block の終端規則を parser の外側で再実装することになり、
parser との差異がそのまま表示のバグになる。窓の再解析結果と既存境界の一致で判定する。

### 編集のたびに全文を解析し直す

採用しない。ADR-0008 の通り、100 MB 文書で入力遅延が文書サイズに比例する。

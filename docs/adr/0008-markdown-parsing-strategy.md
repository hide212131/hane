# ADR-0008: Markdown 解析方針

## ステータス

承認済み

## 日付

2026-08-24

## 背景

本エディタは Markdown ソースを唯一の正とし、画面表示は source range と対応していなければならない。

RFP では、Markdown の意味解釈には原則として `pulldown-cmark` を使い、解析結果と source range を結び付けることを求めている。一方で、巨大文書を入力ごとに全文解析することは禁止している。

## 決定

Markdown 解析は二段階に分ける。

```text
Document Buffer
├── active block local parse
└── full document background parse
```

Phase 0 では full Markdown parse は実装しない。太字だけの局所 presentation 実験を行い、後続の parser integration に必要な invalidation と revision model を検証する。

Phase 2 以降では、正式な Markdown 解析に `pulldown-cmark` を使う。

`markdown` crate は以下を提供する。

```text
MarkdownParser
├── parse_document(revision, source_snapshot)
├── parse_blocks(revision, source_ranges)
└── events_with_source_ranges()
```

parser result は Document Buffer を直接変更しない。`presentation` crate が parser result から `VisualBlock` と `SourceMap` を生成する。

## Snapshot とメモリ制限

100 MB 文書の `source_snapshot` をバックグラウンド job ごとに完全コピーしない。

`source_snapshot` は、以下のいずれかで表現する。

- Text Buffer 実装が提供する immutable snapshot / shared rope snapshot。
- Arc 共有された chunk tree。
- 対象 block / range だけの owned text。

full document parse job のために、100 MB の owned `String` を job ごとに作らない。

同時に保持できる full-document snapshot / full-document parse job は最大1つとする。新しい full parse request が来た場合は、未開始 job を置き換え、実行中 job には cancel signal を送る。実行中 job が cancel できない場合でも、2本目の full-document snapshot を追加で保持しない。

local parse と visible presentation job は、対象 range の小さな snapshot だけを保持してよい。ただし、range snapshot の総量が fixture サイズに比例して積み上がらないよう、同種 job は ADR-0005 の coalescing 規則に従う。

この制限は、メモリ目標の測定値が解析用コピーによって歪むことを避けるための Phase 0 からの契約とする。

## 局所解析

入力直後は active block 周辺だけを局所解析する。

局所解析の目的は、現在見えている編集位置の表示を早く更新することであり、文書全体の構造を完全に確定することではない。

局所解析結果は、同じ revision の正式解析結果が到着したら置き換えられる。

正式解析結果が古い revision で完了した場合は、ADR-0005 の revision / range check に従い、非重複 block だけを部分 publish できる。

## Tree-sitter

初期版では Tree-sitter Markdown を唯一の parser として採用しない。

将来的に差分解析のために補助的に評価する余地は残すが、Markdown の意味解釈と source range の正確性を優先する。

## 結果

入力処理は parser を待たず、表示は段階的に正確になる。

一方で、局所解析と全文解析の結果が一時的に異なる可能性がある。差異が出た場合は、同一 revision の全文解析結果を優先する。

## 検討した代替案

### 入力ごとに全文 `pulldown-cmark` を実行する

採用しない。

100 MB 文書で入力遅延が文書サイズに比例しやすい。

### 独自 Markdown parser を最初から作る

採用しない。

Markdown の互換性と source range の正確性を同時に実現するコストが高い。

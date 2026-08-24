# ADR-0005: Revision とバックグラウンド処理

## ステータス

承認済み

## 日付

2026-08-24

## 背景

Markdown 解析、presentation 更新、画像読み込み、保存、性能計測は入力処理より重い。

これらがキー入力や IME composition を待たせると、製品の最重要価値である「入力が引っ掛からない」ことを満たせない。

## 決定

Document は edit ごとに単調増加する `Revision` を持つ。

すべてのバックグラウンド処理は、開始時点の revision を持つ job として実行する。

```text
Document revision 100
  └── parse job starts with revision 100

Document revision 101
Document revision 102

parse job for revision 100 finishes
  └── current revision is 102, so result is stale and discarded
```

バックグラウンド job は、完了時に current revision と一致する場合のみ無条件で publish できる。

revision が一致しない場合でも、job の入力 snapshot 以降の edit set と job の出力範囲が重ならないことを証明できる場合に限り、部分 publish を許可する。

それ以外の stale result は破棄する。

## Job Coalescing と Stale Result

継続入力中に全文解析 job が永遠に publish されない問題を避けるため、job 種別ごとに以下の方針を採用する。

| job 種別 | 方針 |
|---|---|
| visible presentation | 最新 revision へ coalesce し、古い未開始 job は削除する |
| active block local parse | 最新 revision へ coalesce し、入力ごとに active block 周辺だけ再実行する |
| full document parse | 低優先度で1本だけ実行し、完了時に非重複範囲だけ部分 publish する |
| layout preparation | visible range と overscan に限定し、scroll / edit ごとに coalesce する |
| autosave | 保存対象 revision を固定し、保存完了時に current revision と別に記録する |

Document は revision 間の edit summary を短期間保持する。

```text
RevisionDelta
├── from_revision
├── to_revision
├── edited_source_range_before
├── edited_source_range_after
└── byte_delta
```

stale job の result range が、`from_revision + 1` から current revision までの `edited_source_range_before` / `edited_source_range_after` と重ならない場合、その result は current revision へ rebase して部分 publish できる。

ここでの rebase は、単に「範囲が非重複ならそのまま publish する」ことではない。job result に含まれるすべての `SourceOffset`、`SourceRange`、`SourceMap` segment、`VisualBlock.source_range`、style run の source range、diagnostic / metadata の source range を、対象 revision 間の全 `RevisionDelta` で順に変換する。

例えば、job result の対象 block より前に挿入または削除が発生した場合、result range 自体は編集範囲と重ならなくても、後続 block の source offset は `byte_delta` 分だけ移動する。この offset 変換を適用できない result は publish しない。

rebase は以下の順で判定する。

1. job の base revision から current revision までの `RevisionDelta` を取得する。
2. result の semantic range がいずれかの edit range と重なる場合は破棄する。
3. 重ならない場合、すべての source position を各 delta で前方から変換する。
4. 変換後の range が current Document Buffer の UTF-8 境界と範囲条件を満たすことを検証する。
5. 検証に通った result だけを current revision の partial result として publish する。

重なる場合は破棄する。Phase 0 では rebase 実装を presentation block 単位に限定し、全文 parser result の細かい merge は Phase 2 以降で扱う。

未開始 job は積み上げず、同種 job は最新条件で置き換える。実行中 job は可能なら cancel signal を送るが、cancel 成否に関係なく完了時の revision / range check を必須とする。

Phase 0 では以下の job 種別を用意する。

- presentation update for bold experiment。
- visible block layout preparation。
- benchmark measurement aggregation。
- optional file fixture loading。

Phase 1 以降では以下を同じ model に乗せる。

- Markdown 全体解析。
- syntax highlight。
- image metadata loading。
- autosave。

## 入力処理の規則

入力処理経路では以下のみを行う。

1. IME / keyboard event を editor command に変換する。
2. Document Buffer に edit を適用する。
3. Cursor / Selection / IME state を更新する。
4. 影響範囲の presentation cache を invalid にする。
5. UI の再描画を要求する。

入力処理経路では以下を行わない。

- 全文 Markdown 解析。
- 全文レイアウト。
- 全文 style run 再生成。
- 同期ファイル保存。
- 画像読み込み。

## 結果

古い解析結果で新しい表示を上書きする事故を避けられる。

入力 path が短くなるため、p95 / p99 の遅延を測定しやすい。

一方で、UI は一時的に古い presentation を表示する可能性がある。これは許容するが、Document Buffer と cursor state は常に最新 revision を参照する。

## 検討した代替案

### 入力ごとに同期的に Markdown 解析する

採用しない。

巨大文書で入力遅延が parser の最悪ケースに引きずられる。

### 最新 job だけを実行し、古い job を必ずキャンセルする

一部採用に留める。

キャンセル可能な job はキャンセルしてよい。ただし、すべての処理が即座にキャンセルできるとは限らないため、完了時の revision check は必須とする。

### revision 完全一致の result だけを publish する

採用しない。

継続入力中の 100 MB 文書では、全文解析が常に stale になり publish されない可能性が高い。非重複範囲の部分 publish と job coalescing を併用する。

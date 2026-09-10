# ADR-0025: 複数行 inline presentation の shared parse

## ステータス

提案

## 日付

2026-09-11

## 関連

- Issue #101
- Pull Request #102
- Issue #110
- Issue #111
- ADR-0005: Revision and background work
- ADR-0006: Presentation blocks and virtual scroll
- ADR-0008: Markdown parsing strategy
- ADR-0016: Phase 3 Typora-style Editing 実装計画
- ADR-0020: Block virtualization
- ADR-0021: Layout lines and visual coordinates
- ADR-0022: Layout cache invalidation

## 背景

Hane の presentation は physical line を基本単位として発展してきた。一方、CommonMark の emphasis / strong emphasis / code span は soft line break をまたいで成立するため、1 physical line だけを自己完結に parse すると、対応する delimiter が別行にある構文を正しく解釈できない。

PR #102 ではこの問題に対応するため `JoinedParse` による複数行 shared parse を導入した。しかし review の過程で、通常描画、1 physical line だけを描画する経路、caret の上下移動・隣接 block navigation、marker disclosure、quote/list marker derivation、巨大 block の virtualization が必ずしも同じ parse snapshot を利用していないことが判明した。

その結果、ある経路を局所修正しても別経路が line-local parse へ戻る、viewport の大きさによって同じ source の marker visibility が変わる、navigation 用 layout と通常描画で caret の x 座標解釈がずれる、巨大 block を UI thread で全文同期 parse する、といった同根の不整合が review のたびに現れた。

これは個別バグの集合ではなく、line-oriented presentation から block-oriented semantic parse への移行が部分的な状態にあることが原因である。

## 決定

### 1. joinable block の inline semantics は shared parse を唯一の正とする

Paragraph、inline paragraph を含む Quote / List など、複数 physical line にまたがって inline 構文を解釈する必要がある block では、同一 revision / source range に結び付いた完全な inline parse snapshot を正とする。

通常描画、1 physical line だけを描画する場合、caret の上下移動、隣接 block navigation、marker disclosure、hit testing に必要な presentation は、同じ shared parse snapshot から projection する。

描画対象が1行だけであることを理由に line-local parse へ fallback しない。

### 2. viewport と parse boundary を分離する

viewport / render window は、作成する `VisualLine` と layout work の量を制限するための境界である。Markdown の意味解析境界ではない。

同一 source / revision について、次の違いによって構文解釈を変えてはならない。

- viewport の位置・高さ
- scroll 位置
- 描画される physical line 数
- caret が同一 block 内にあるか隣接 block から移動してきたか
- 通常描画か navigation 用 layout か

したがって、delimiter の片側が viewport 外にあっても、shared parse が認識した syntax、marker visibility、style、SourceMap の意味は維持する。

### 3. parse と presentation projection を分離する

概念上の流れを次に統一する。

```text
Markdown source / Rope snapshot
        |
        v
revision-bound block parse
        |
        v
JoinedParse / shared semantic snapshot
        |
        +--> viewport presentation projection
        +--> single-line presentation projection
        +--> disclosure projection
        +--> caret/navigation layout projection
        +--> hit-test / SourceMap projection
```

navigation、disclosure、single-line presentation のために独自に Markdown を再 parse する経路を増やさない。

### 4. UI thread の synchronous full-block parse を有界にする

同期 parse の可否を physical line 数だけで判断しない。総 source byte 数にも上限を設ける。

行数上限または byte 上限のどちらかを超える joinable block は、UI/render path で全文を copy・連結・parse しない。

上限超過時は background parse / cache を利用する。background result は、この ADR で定義する `JoinedParse` については source revision と block identity/range が現在値と厳密一致する場合だけ publish / reuse する。stale result の扱いは「8. `JoinedParse` は strict-match-only とする」に従う。

具体的な閾値は実測と benchmark で調整可能とし、この ADR は特定の数値を固定しない。ただし「行数だけの上限」は不可とする。

### 5. marker derivation を absolute source position と構造に結び付ける

Quote / List のように各 physical line に source marker を持つ構造では、shared parse の block range と marker range を同じ source coordinate system で扱う。

特に nested quote の continuation line では、最初の行から流用した relative offset だけで inner marker を推定しない。各 physical line の開始 source position と quote depth / ancestor structure に基づいて marker range を導出する。

### 6. disclosure は shared syntax range と editor state の交差として扱う

caret、selection、IME marked range が shared syntax construct に入った場合、その construct に属する開閉 marker の disclosure が viewport 内外で不整合にならないようにする。

可視 physical line だけから block-wide disclosure を再構成しない。editor state と shared parse snapshot を使って construct の active range を決定する。

### 7. cache key と invalidation は shared snapshot の意味に合わせる

shared parse / presentation cache は、少なくとも source revision、対象 block identity/range、構文解釈に影響する editor state を明示的に扱う。

同一 revision で有効な `JoinedParse` が存在する場合、projection 経路ごとに別の parse を作らない。

revision が変わった snapshot を新 revision へ暗黙に流用しない。

### 8. `JoinedParse` は strict-match-only とし、ADR-0005 の rebase 対象にしない

ADR-0005 は一般の background result について、入力 snapshot 以降の edit と semantic range が非交差で、かつ result が持つすべての source position を `RevisionDelta` で正しく変換できる場合に限り stale result の rebase / partial publish を認めている。この契約自体は変更しない。

一方、現在の `JoinedParse` は joined source と Markdown parse tree、marker / style に使う source range を **一つの block の exact source snapshot** に結び付けた semantic cache であり、ADR-0005 が要求する全 source position の delta 変換を実装していない。block 前方の edit だけでも absolute source coordinate がずれるため、「編集範囲と非交差」という理由だけで current revision へ持ち上げてはならない。

したがって `JoinedParse` の background result / cache entry は次のすべてが一致する場合だけ publish / reuse する。

- document revision
- stable block identity
- block source range

いずれかが一致しなければ stale として破棄し、再baseしない。これは `JoinedParse` に限定した strict policy であり、`BlockIndexState::publish` 等が持つ既存の rebase 契約を一般に狭めるものではない。

将来 `JoinedParse` を rebase 可能にする場合は、parse tree、markers、style/source ranges、joined sourceとの対応を含む全 source coordinate を `RevisionDelta` で変換し、semantic equivalence を証明する必要がある。その実装を導入するときに本 ADR を更新する。

## 禁止する実装パターン

次を局所回避策として追加しない。

- 「1 physical lineだけだから」line-local parse を使う。
- viewport に入っている行だけを parse して multi-line delimiter を推定する。
- navigation 用に通常描画とは別の Markdown parse を行う。
- disclosure を visible lines だけから再構成する。
- 「4096行以下」など行数だけを根拠に巨大 source を同期全文 parse する。
- nested quote marker を block 先頭の relative offset の単純流用で求める。
- `JoinedParse` を revision/range不一致のまま「非交差だから」と current revision へ流用する。

例外が必要な場合は、同一 source/revision の semantic equivalence をテストで証明し、この ADR を更新する。

## 回帰テストの不変条件

PR #102 および後続実装では、少なくとも次を固定する。

1. delimiter の開閉が viewport 境界をまたいでも通常描画結果が変わらない。
2. shared parse が存在する block は、描画対象が1 physical lineだけでも同じ marker visibility / style を保つ。
3. 通常描画と adjacent-block vertical navigation が同じ visual/source mapping を使い、同じ x 座標から caret がずれない。
4. caret / selection / IME が multi-line construct にある場合、対応 marker disclosure が off-screen line を含む構文全体で整合する。
5. nested quote の各 physical line で各 depth の marker range が正しい。
6. 行数は少ないが byte 数が巨大な block を synchronous full parse path に入れない。
7. background `JoinedParse` result は revision / block identity / source range のいずれかが mismatch なら publish / reuse されない。
8. viewport、scroll位置、rendered line count を変えても同一 source/revision の semantic result が同じである。

## 移行方針

PR #102 の残件は、個別 review comment に対する局所 workaround として処理せず、本 ADR の不変条件へ収束させる。

修正開始前に対象 exact head の non-outdated / unresolved substantive review threads を全件 snapshot し、root-cause cluster を把握する。独立した finding を1回のClaude invocationへ無造作に混ぜず、原則1 finding / 1 repair invocation とするが、各修正は本 ADR の共通設計を壊さないことを条件とする。

各修正後は新しい exact head で review thread snapshot を取り直し、outdated 化した指摘、新規 finding、残存 finding を再分類する。

review 状態の canonical snapshot と Claude fix worker / merge gate の不一致は Issue #110 で別途恒久修正する。

## 結果

Markdown の semantic parse と viewport presentation を分離し、joinable block の意味を revision-bound shared snapshot に一元化する。これにより、スクロール・表示行数・navigation 経路によって同じ Markdown の表示や caret mapping が変化することを防ぎながら、大文書では UI thread の work を可視範囲に対して有界に保つ。

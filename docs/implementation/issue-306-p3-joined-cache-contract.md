# Issue #306 P3: 背景解析の開始判定も共通の鮮度契約を使う

## 目的・基準

対象は Issue #306 / RF4、台帳 RF0-03/04/05 のうち保存済み JoinedParse の参照条件の重複である。基準 main は `093eca703c04b0cedfdc7c5a45a9f172959c247d`。要求の正本は Issue #306、前段仕様は `docs/implementation/issue-306-p1-joined-parse-lookup.md` と `docs/implementation/issue-306-p2-block-presentation-request.md`、契約は ADR-0005/0018/0025/0029 とする。

P1 #540 と P2 #541 は受入済み。P2のmerge commitは `d9050c1f02e06c8748399757f5131d29a0aea41a`。#301全体や#305のGUI残件を一律の先行条件とせず、受入済みのbackground parse分離 #419 とprojection境界を使う。Jev CLIの実応答 `jev-1.14.0` は、背景開始判定を既存helperへ寄せ、focused回帰テストと経路表を同じ限定PRに置く候補を選択した。

初回準備commitはこの文書だけであり、製品実装・検証は未実施。現在の状態は実装PRのcurrent headとCI/review evidenceを参照する。

## 確認した経路・契約

| 利用者・入口 | 保存済み解析／projectionと鮮度 | 今回の扱い |
| --- | --- | --- |
| `EditorView::layout_around` / `neighbor_row_target` / `cached_block` / `disclosures_are_current` | P1の `current_joined_parse` がBlockIdをkeyに検索し、現在の文書revisionと外側のblock全体source rangeが一致する場合だけ再利用 | 維持 |
| `view/background_parse.rs::schedule_joined_parse` の開始判定 | joinableで両sync budget内なら開始しない。in-flightあり、または同じid/revision/外側rangeのcacheありなら開始しない。cache判定がP1と重複 | cacheありの判定だけ同じhelperに接続 |
| 同メソッドの結果受理 | DocumentKey、job identity、現在revision、current indexのBlockId/rangeを別々に確認。旧文書jobも完了まで実行数に含む | 変更しない。再利用判定と結果受理は異なる契約 |
| `line::presented_block` → `block_context` | joinable・joinedなし・4,096行かつ256 KiB以内なら全spanを同期処理。それ以外はrender範囲。fence端点やheight metadataは別の必要範囲として読む | P2 request・条件・処理量を維持 |
| `presentation::present_block_with_table_projection` → `disclosure_runs` / `run_uses_shared_parse` | 複数物理行またはjoinedありなら共有parse。同じrunとdisclosureを `expected_disclosures` も使用。joinedありの単一表示行も共有parseを使用 | 維持。独自のviewport意味解析を追加しない |
| 正式解析待ちの大block | boundedな表示の後、背景JoinedParse到着で該当表示cacheを無効化し再表示 | 暫定表示を正式解析済みとは扱わない |
| `BlockIndexState::publish` | 正式・暫定の優先順位と条件付きrebaseがある | JoinedParse同様の一律strictに変えない |
| `BlockIndex::list_edit_projection_at` | bufferと同revisionのindexからcaret blockのsourceを同期parseし `ListEditProjection` を作る。正式表示用 `ListProjection` を待たず編集可能 | 用途の異なる経路として維持。表示用と統合しない |
| fence/tableの表示と未対応・未確定source | block kind・専用projection・boundedなfallbackによりsourceを保持 | 本P3では変更しない。全経路の最終照合はIssue全体の残件 |

この表はcurrent sourceの入口の対応であり、Issue全体のGUI・性能受入や全fallbackの完全性を立証するものではない。

## 実装する一つの変更

`crates/ui/src/view/background_parse.rs` の `schedule_joined_parse` で、次の既存判定の後半だけを `self.current_joined_parse(block).is_some()` に置き換える。

- 前半: `self.joined_parse_jobs.contains_key(&block.id)` をそのまま先に評価する。
- 後半: `self.joined_parse_cache.get(&block.id).is_some_and(|cached| cached.revision == revision && cached.source_range == block.source_range)` を既存helper呼出しへ置換する。

`current_joined_parse` は親moduleのprivate methodなので、子moduleからの呼出しのために可視性やpublic APIを広げない。helper本体は変更しない。既存revision変数はjob生成と結果受理に必要なので残す。loop途中のrevision変更やstate更新を追加せず、順序、短絡、cache key、外側rangeの比較を維持する。

変更する製品処理はこの一か所だけ。新helper、wrapper、cache/state、trait、featureを追加しない。

## 直接関連する検証

`crates/ui/src/view.rs` の既存 `#[gpui::test]` に沿い、schedulerのcache有効性を外から観測できるfocused testを追加する。例として `joined_parse_scheduler_uses_the_current_cache_contract` のような名前でよい。

同じ実文書の両sync budgetを超えるjoinable blockを用意する。byte数だけを超える低行数fixtureを使える。末尾空行を持ち、内側parse rangeと外側block rangeの違いをassertする。人工的なsleepやOS画面を使わず、既存のTestAppContext/背景executorの制御を使う。

- cacheなし: job mapとrunning数が開始を示す。
- id/revision/外側rangeがすべて一致: jobを作らず、cacheのparseを保持する。
- cache keyのid不一致: 他idのcacheで抑止されず、対象blockのjobを開始する。
- cache revision不一致: 同じid/rangeでもjobを開始する。可能なら後続blockへの非交差編集でrevisionだけ進める。
- cacheの外側rangeのstartまたはend不一致: 同じid/revisionでもjobを開始する。
- 一致cacheの内側parse rangeが末尾空行を除く: 内側rangeとの差を理由に不要jobを作らない。

各caseは別Viewなどで隔離し、in-flightと実行数上限によってcache判定まで到達しない偽の成功を避ける。開始直後（背景完了前）のjob map/running数をassertし、必要な完了待ちは既存の `run_until_parked` を使う。既存testを削除・弱体化しない。既存のP1 helper test、byte-large背景共有意味、job上限・文書切替・旧結果拒否・job identity保持・単一表示行・隣接block移動も維持する。

```sh
cargo test --workspace --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
git diff --check
```

macOS/Windowsのcurrent-head CIと最終full reviewを別に取得する。workerは検証を実行できないため、trusted finalizer後のexact-head verifyを最初の実行検証とする。通常featuresも受入前に確認する。

P3の差分が同等のcache条件を既存helperへ置換しただけで、描画・入力・job開始条件・解析量が同じことを最終差分で確認できる場合、新規GUI・性能測定は不要と判断できる。その判断はCommanderがPRへ記録する。変更や不明があれば必要なevidenceを追加する。Issue全体のGUI/性能条件は免除しない。

## 対象外・受入・復旧

背景jobの開始/結果受理の新ルール、DocumentKey、実行数、cache寿命・無効化、BlockIndexState、parse/fallbackの統合、P2 request、同期上限、Markdown解釈、geometry、入力、保存、依存、保護パス、他PRの機能は変更しない。#548等の未受入機能を移植しない。

P3受入は、重複判定が除去され、上記cache境界testと既存test・Clippy・required CI・reviewがcurrent contextで確認されること。開始判定と結果受理の違いを曖昧にしない。これだけで#306/#297をcloseしない。残る全経路・fallback照合、Issue全体のGUI/性能検証は引き続き追跡する。

永続形式・外部設定は変えない。本PRの一か所の接続と直接testを一単位でrevertする。後続が依存するときは後続から逆順に戻し、P1/P2の受入済み経路は保持する。

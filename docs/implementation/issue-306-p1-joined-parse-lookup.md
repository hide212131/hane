# Issue #306 P1: 保存済み JoinedParse の参照条件を一つにまとめる

## 目的と開始時点

対象は [Issue #306](https://github.com/hide212131/hane/issues/306) の RF4 の最初の小PRである。`EditorView` の4利用者がそれぞれ持つ同一の参照条件を一つにまとめ、現在の挙動を保つ。解析の最適化や表示仕様の変更は含めない。

- 調査基準: `main` @ `c5ffb83824925bf09c04ccf2d5610eec05ee750e`。
- 計画: `docs/refactor-plan.md` RF4、`docs/refactor-execution-plan.md` 第2節・第9.2/9.3節。対象台帳は RF0-03/04/05。
- 契約: ADR-0005、ADR-0018、ADR-0025、ADR-0029。既存の判断を変更しないため、新しいADRは作らない。
- この文書の初回commitは実装担当を起動する準備であり、製品コード・テストの実装、実行結果の取得はまだ行っていない。現在の実装・検証状態はPRを参照する。

`#301` 全体は完了ではなく `not_planned` でcloseされているが、今回必要な背景解析の分離はPR #419で受け入れ済み。presentation分離（#302、最終PR #475）、Markdown分離（#303、最終PR #466）、引用と表の修正（PR #292/#295）もmainへ反映済みである。これらを再実装しない。#305の残る実画面確認と本P1は独立している。

開始時のPR #538（フッター変更）は `view.rs` を変更するが、今回対象の4か所のキャッシュ参照は変更していない。統合前にcurrent mainとPR差分を再確認する。

## 現在の構造と今回の変更

`joined_parse_cache` の所有者は `EditorView` であり、`BlockId` をkeyとして `JoinedBlockCache` を保持している。背景処理が結果を登録し、文書差し替え時には既存の処理がキャッシュを破棄する。所有者・生成・破棄の経路は変えない。

次の4か所は、同じkey取得、revision比較、source range比較、parse参照の取り出しを繰り返している。

| 利用者 | 今回まとめる部分 | そのまま残す部分 |
| --- | --- | --- |
| `EditorView::layout_around` | 対象blockの保存済みparseの選択 | 表示範囲、レイアウト、projection、検索用の強制表示を渡さない判断 |
| `EditorView::neighbor_row_target` | 隣接blockを解決した後のparseの選択 | 隣接block/行の決定、座標計算、検索用の強制表示を渡さない判断 |
| `EditorView::cached_block` | 再表示に使うparseの選択 | `VisualBlock`/layoutの条件付きrebase、再利用、格納、検索対象の強制表示 |
| `EditorView::disclosures_are_current` | 表示中の記号の整合確認に使うparseの選択 | render範囲、`expected_block_disclosures`、検索対象の強制表示、比較処理 |

`crates/ui/src/view.rs` の `cached_block` の近くに、privateな `EditorView::current_joined_parse(&self, indexed: &IndexedBlock) -> Option<&JoinedParse>` を追加する。既存の条件をそのまま移し、上の4か所をこのメソッドへ接続する。

処理は `joined_parse_cache.get(&indexed.id)` → 現在の文書revisionと `JoinedBlockCache.revision` の一致確認 → `JoinedBlockCache.source_range` と `indexed.source_range` の一致確認 → `&cached.parse` の返却だけとする。不一致・未登録では `None` を返し、削除・更新・parse・cloneは行わない。

### 維持する条件

| データ | 現行の役割と今回の扱い |
| --- | --- |
| `JoinedParse` | 文書revision、stable block identity、block全体のsource rangeが一致した保存済み結果だけを参照する。編集範囲が非交差でも旧revisionへ戻らず、rebaseしない。 |
| `BlockIndexState` | 条件付きrebaseと正式/暫定結果の優先順位を持つ。今回のhelperを適用せず、現在の条件を変えない。 |
| `ListProjection` / `ListEditProjection` | 前者は表示、後者は現在のsourceに対する同期編集の判断を担う。両者を統合せず、編集を背景解析待ちにしない。 |
| fence/table projection | 現在のindexから取得する。引数、所有者、有効性の判定、表示結果を変えない。 |

`JoinedBlockCache.source_range` はblock全体の範囲である。一方、内部の `JoinedParse.source_range` は末尾空行を除いた範囲になり得る。比較対象を内部parseのrangeへ置き換えない。`IndexedBlock.revision` や内部parseのrevisionを使った新しい判定も足さない。

`cached_block` の `revision` は既存の表示キャッシュのrebase処理にも使うため残す。他の3か所で不要になるローカル変数は除去できる。必要最小限のコメントで、lookupのkeyもidentity確認に含まれることを示す。

## 変更するファイルと対象外

製品と回帰テストは `crates/ui/src/view.rs` だけを変更する。テストは既存の同ファイル内テストに追加する。この文書には実装との対応に必要な修正だけを加えられる。全体整形や他領域の修正を混ぜない。

`view/background_parse.rs::schedule_joined_parse` にも似た条件があるが、これは背景jobの開始判断なので今回の4利用者とは分けて維持する。背景結果の受理・破棄、`DocumentKey`、実行数上限、cache寿命と所有権、文書差し替えの処理は変えない。

`line.rs` の引数整理、テスト用wrapper撤去、異なるparse/fallbackの統合、同期処理量と上限、Markdown解釈、geometry、入力・IME、保存・rename、#538のフッター、配布、依存関係、workflowとその他の保護パスは対象外である。

## このP1の受入条件と検証

1. 対象4利用者が同じhelperを使い、従来のpredicateと参照を保っている。背景jobの開始判断は独立して残る。
2. 正しいentryでは同一parse参照を返す。未登録、別BlockId、range開始だけの不一致、range末尾だけの不一致、現在の文書revisionだけの不一致を、それぞれ拒否する。
3. 末尾空行を持つblockで内側/外側のrangeが異なっていても、正しいentryを再利用する。別blockの編集でrevisionだけが進んだ場合も、対象rangeに非交差だからと再利用しない。
4. 既存の表示、隣接block移動、記号表示、巨大blockの同期量制限、背景結果の競合処理が変わらず、current headのworkspace testsとClippyが成功する。

追加テストはprivate helperの境界契約を一つのfocusedなGPUIテスト（必要なら読みやすい少数のテスト）で検証する。各不一致を独立に作り、一致時は `std::ptr::eq` などで同じ参照を返すことも確認する。既存の背景完了テストは削除・書き換えない。

既存の主な回帰対象は、`joined_parse_work_is_bounded_across_scrolling_and_document_switches`、`joined_parse_completion_*`、`disclosures_are_current_reuses_a_joined_runs_shared_disclosure`、`cached_block_reuses_a_joined_runs_presentation_across_frames`、`moving_into_a_joinable_neighbor_beyond_the_sync_budget_uses_the_cached_parse`、`block_context_rejects_stale_revision` である。

通常の検証経路で次を実行し、対象headと結果をPRへ記録する。

```sh
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
git diff --check
```

Claude workerは既存制限どおりshell/test/build/lintを実行せず、trusted finalizerの後にcurrent headの検証を行う。macOS/Windows CIの結果を確認し、実行していない確認を成功と記載しない。

本P1が上の参照条件の抽出だけに収まる場合、nativeの入力・表示・座標計算・parse量を変更しないため、実画面と性能の再測定は本P1の追加条件にはしない。最終差分がこの範囲に収まることを確認してから判断する。#306全体に必要な実画面・性能確認を免除する意味ではない。

## 残作業と戻し方

本P1だけで #306 や親 #297 をcloseしない。多数引数の具象requestへの整理、parse経路全体の照合、役割の異なるfallbackの記録・整理、旧wrapper撤去とIssue全体の受入は後続で扱う。新たなAPIや仮の互換wrapperはこのP1では導入しない。

永続データと公開APIは変更しない。戻す場合はこのPRをrevertする。後続がprivate helperに依存している場合は、その依存を確認し後続から逆順に戻す。

# Issue #414 実装・検証記録

## 着手時の状態と対象

- 着手時に確認した `main` の完全SHA: `5be310c6136e662119e2d208322f832b317e14c4`。
- 着手時の作業branch: `codex/issue-414-p1-search-core`。Issue #414の既存実装PR／書き込み担当は確認されず、その時点では重複PRを作らなかった。#412／#397は別責務。#415のeditor findも変更範囲外。#297全体やAI設定の完了は待たない。
- latest Issue本文・コメント、default branchの `AGENTS.md` と `docs/aadw-command-policy.md`、添付指示書を着手時に確認した。source tree内の `sources/` は変更していない。
- 作業中にdefault branchが `145188eef1f1353cfd903b0277458ece71ea8831` へ進んだことを再取得で確認した。PR #412のmergeによる `session_save` 分離が `view.rs` と重なるため、変更範囲を比較したうえでbranchをcurrent mainへrebaseし、既存の `mod session_save;` と新しい `mod content_search;` を両方維持した。移動済みのdraft retire経路も更新し、以下の最終検証をrebase後のコードで再実行した。
- 対応範囲: P1検索コア、P2 sidebar/Inputとworker管理、P3既存open完了・選択経路への接続。変更はdocument/sessionの読み取り境界、UIのcontent search用moduleと最小のrouting、テスト、本文書に限定。
- 検索依存: `grep-searcher 0.1.17`、`grep-regex 0.1.14`、`grep-matcher 0.1.8`。
- 引き渡し先: [Draft PR #417](https://github.com/hide212131/hane/pull/417)、`Refs #414`。検証した実装SHA `a7ffddd1ff4bf001a63ee45f3f55ffca99652d6c`、base `main`=`145188eef1f1353cfd903b0277458ece71ea8831`。merge／Issue closeはしていない。

### 変更ファイル

- `crates/document/src/lib.rs`: Rope共有スナップショットのchunk reader。
- `crates/session/src/search.rs`, `service.rs`, `testing.rs`, `workfolder.rs`, `lib.rs`: streaming reader、検索型・制限・UTF-8検証とripgrep検索、テスト用reader、既存scannerの対象規則確認。
- `crates/ui/src/view/content_search.rs`: sidebar検索状態、入力確定後のdebounce、検索世代、最大2worker、channel容量128件のbounded delivery、128一致行/frame、結果反映とstale判定、既存open完了後の検証・選択。
- `crates/ui/src/view.rs`, `view/sidebar.rs`, `view/sidebar_filter.rs`, `view/inline_rename.rs`, `actions.rs`: 画面接続、キーボードrouting、編集／workspace変更時の失効。
- `README.md`: 本文検索のショートカットとWork folder modeでの使い方。
- `Cargo.toml`, `Cargo.lock`, `crates/session/Cargo.toml`: 依存固定。

検索のために外部 `rg`、別の列挙器、全件 `FileService::load`、全文String複製、Markdown解析、新規session生成、保存は追加していない。既存WorkFolder一覧を通常検索で使い、「再検索」は既存scannerで更新する。既存のfile bufferとdraft snapshotをdiskより優先する。

## 対象headの更新経緯とcurrent main（2026-10-07更新）

- 本記録の初版時点のcurrent main: `5be310c6136e662119e2d208322f832b317e14c4`→rebase後 `145188eef1f1353cfd903b0277458ece71ea8831`。CodeRabbit full review時点のPR head: `3eb30f2a58e4afc5cb7a723ce39d63262868cd37`。S10のnative UI実測時点のPR head: `06f79289758d3d7189cb65ab230cc194b93621b2`。
- 今回のCommander指示時点のcurrent main/base: `b01e4dc421f0399c600733c58c4287dd559b2ea3`。対象PR head（修正前）: `628ac0eca1521911d745b3b8e355a9bb7fc8758b`。current-head CI run [37664567624](https://github.com/hide212131/hane/actions/runs/37664567624) ではmacOS／Windows双方のworkspace testsがPASSし、双方のClippyが `crates/ui/src/view/content_search.rs:907` の `clippy::collapsible_if` でFAILした。
- 本更新では、stale-result budget返却のnested `if let`/`if` を `Option::filter` で一段にまとめ、条件・予約返却の意味は変更していない。この修正を適用した最終SHAはfinalizerのpush後にCommanderが確認する（この worker はshell実行不可のためSHA算出・テスト再実行はできない）。この修正はテスト対象コードの振る舞いを変えないbehavior-preserving changeであるため、以下S01〜S08のcommit `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` 時点のテスト結果は、その後のrebase・依存更新を経ても引き続き有効な根拠として扱う。push後のverify run（exact worker result verify run [37663000698](https://github.com/hide212131/hane/actions/runs/37663000698) は本指示時点で処理中）で最終確認する。

## S01〜S10

各行のテスト／観測対象は実装commit `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705`。全10項目に対象SHAを明記した。証拠ログはそのcommitのソースで取得した。S09／S10はその後のheadで更新されており、各行に最新の状態を記載する。

| ID | テスト／観測 | 対象SHA | 結果 | 証拠 |
|---|---|---|---|---|
| S01 | `literal_search_reports_non_overlapping_utf8_byte_ranges`、`overlapping_candidates_are_reported_once_from_their_non_overlapping_starts`、`literal_search_does_not_interpret_regex_syntax`、`case_sensitive_search_can_be_disabled`、`query_validation_keeps_whitespace_and_rejects_newlines_and_oversize` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。文字列一致、大小文字、日本語、複数・重複範囲、空白・改行・query byte上限を確認。 | （履歴）ローカルログ原本は非公開 `/tmp` パスのため失効。[current-head CI run 37664567624](https://github.com/hide212131/hane/actions/runs/37664567624) のworkspace testsがこのsession側テスト群を含めてPASS。 |
| S02 | `markdown_files_are_discovered_and_non_markdown_files_are_ignored`、`the_root_recovery_journal_directory_is_not_scanned`、`dotfile_directories_other_than_the_root_recovery_journal_are_still_scanned`、`scanner_keeps_gitignored_and_hidden_notes_but_skips_root_state` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | macOS／Windows CI PASS。`.MD`、hidden/gitignored markdown、root `.hane`除外、nested `.hane`保持を確認。Unix symlink targetの検査はmacOSのみ。filter／tree展開状態はscanner入力と別stateであることを実装経路で確認。 | （履歴）[CI run 36740685239](https://github.com/hide212131/hane/actions/runs/36740685239)（head `a7ffddd1ff4bf001a63ee45f3f55ffca99652d6c`）／現在は[current-head CI run 37664567624](https://github.com/hide212131/hane/actions/runs/37664567624) |
| S03 | `disk_search_does_not_load_or_save_a_document`、`search_prefers_an_existing_buffer_snapshot_over_disk`、`draft_search_uses_its_existing_session_snapshot`、`rope_snapshot_keeps_its_source_when_the_buffer_is_edited`、`search_sources_include_hidden_buffer_and_work_folder_draft_without_disk_duplicate` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。inactive tabのbuffer、同じpathのdisk除外、WorkFolder draftを含むsource一覧をGPUI testで確認。 | （履歴）ローカルログ原本は非公開 `/tmp` パスのため失効。[current-head CI run 37664567624](https://github.com/hide212131/hane/actions/runs/37664567624) で再確認。 |
| S04 | `line_endings_preserve_source_byte_ranges_and_line_numbers`、`crlf_split_across_reads_is_one_line_and_bare_cr_is_a_line_break`、`bom_bytes_are_included_in_source_ranges`、`memory_reader_can_split_utf8_at_every_byte_boundary`、`rope_snapshot_streams_original_utf8_bytes_across_small_reads` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。LF／CRLF／CR、read境界のCRLF、BOM、EOF改行なし、UTF-8分割後も元のbyte範囲を保持。 | （履歴）ローカルログ原本は非公開 `/tmp` パスのため失効。[current-head CI run 37664567624](https://github.com/hide212131/hane/actions/runs/37664567624) で再確認。 |
| S05 | `cancellation_is_observed_while_scanning_a_large_zero_match_file`、`a_full_result_queue_stops_sending_after_cancellation`、`cancelled_workers_still_use_the_two_controller_slots_until_removed`、`returning_to_a_query_after_an_intermediate_query_gets_a_new_epoch`、`a_late_terminal_event_cannot_complete_a_newer_query`、`dropping_the_search_controller_cancels_current_and_active_workers` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。zero-hit reader、満杯queueの取消、未停止workerが2 slotを占めること、A→B→A epoch、stale terminal、controller dropを確認。channel容量128（`MAX_SEARCH_QUEUED_FILES`）、最大128一致行／frame（`MAX_SEARCH_ROWS_PER_FRAME`）を実装（旧記録の「容量4」は誤りのため訂正）。実GUI frame待ちのoccupancyは未計測。 | [current-head CI run 37664567624](https://github.com/hide212131/hane/actions/runs/37664567624) |
| S06 | `per_document_limit_is_reported_and_does_not_claim_completion`、`invalid_utf8_discards_earlier_matches_in_the_same_file`、`a_truncated_utf8_scalar_at_eof_is_a_warning`、`nul_byte_is_reported_as_a_binary_warning`、`read_failure_discards_earlier_matches_in_the_same_file`、`changed_reader_stamp_discards_the_results`、`a_line_over_the_searcher_heap_limit_is_not_reported_as_complete`、`a_missing_disk_file_is_a_warning_instead_of_an_empty_result`、`result_delivery_respects_frame_hit_and_cumulative_text_limits`、`reaching_a_global_delivery_limit_is_reported_as_partial` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。文書あたり1,000件と配送中の全体10,000件／32 MiB上限をhelper testで確認し、上限時のPartial遷移も確認。エラー／打切りを正常完了へ読み替えない。 | （履歴）ローカルログ原本は非公開 `/tmp` パスのため失効。[current-head CI run 37664567624](https://github.com/hide212131/hane/actions/runs/37664567624) で再確認。 |
| S07 | `a_stale_buffer_revision_does_not_move_to_an_old_search_result`、`a_superseded_navigation_id_cannot_select_a_search_hit`、`changed_disk_content_opens_without_reusing_the_old_search_position`、`a_late_terminal_event_cannot_complete_a_newer_query`、`a_late_open_completion_does_not_override_the_newer_search_result_selection` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。GPUI testで既存 `finish_open` 完了経路を新しい結果→古い結果の順に適用し、遅れて完了した古いopenが別文書の選択・active stateを上書きしないことを確認。buffer revision、navigation id、disk変更も確認。 | （履歴）ローカルログ原本は非公開 `/tmp` パスのため失効。[current-head CI run 37664567624](https://github.com/hide212131/hane/actions/runs/37664567624) で再確認。 |
| S08 | `a_draft_search_result_selects_the_original_utf8_byte_range`、`a_disk_search_result_opens_through_the_existing_path_and_selects_its_utf8_range`、`changed_disk_content_opens_without_reusing_the_old_search_position`、`selecting_a_draft_hit_preserves_dirty_text_and_undo_history` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。Draft／未読込diskの結果からbyte範囲を選択し、編集済みdraftのdirty本文とundo履歴を保つことをGPUI testで確認。リンク／表／コードの実画面表示はS09に残す。 | （履歴）ローカルログ原本は非公開 `/tmp` パスのため失効。[current-head CI run 37664567624](https://github.com/hide212131/hane/actions/runs/37664567624) で再確認。 |
| S09 | `Cmd/Ctrl+Shift+F`、Enter／Tab／Esc／上下キーのroutingと、検索sidebarが開くこと、検索結果からの貼り付け・保存が意図した文書だけに反映されることの実機確認。 | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` 以降のheadでの手動確認 | PASS（完了）。利用者からmacOS／Windows双方での実機確認完了の報告あり。検索sidebarが開くこと、検索結果の貼り付け・保存が意図した文書だけに反映されることを含めて完了として記録する。CUAによる自動AX取得は過去に`-10005 timeoutReached`で未実施のままだが、利用者の実機確認がこれに代わる完了根拠。IME変換・focus・sidebar復元の個別手順詳細は利用者報告に含まれない範囲のため、ここでは未確認の追加詳細を作らない。 | [PR #417](https://github.com/hide212131/hane/pull/417) 上の利用者報告（本記録時点でコメントURL未提供） |
| S10 | 性能目標（検索を開いてから結果表示、3秒目標）の実機計測。詳細は下記「S10 詳細」を参照。 | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705`（初期engine-onlyベンチ）／`06f79289758d3d7189cb65ab230cc194b93621b2`（native UI実測、3秒目標不合格）／`628ac0eca1521911d745b3b8e355a9bb7fc8758b`（current head、未測定） | 未完了（FAIL／未測定が混在）。現行 target head では production UI計時を実施できておらず、S10はPASSとして記録しない。 | 下記「S10 詳細」のリンク参照 |

### S10 詳細

- **初期standalone benchmark（historical、engine-onlyの参考値）**: commit `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` でのscanner・search・bounded collectorのみのベンチマーク（0-hit median 598.3 ms等）は、GPUI描画・actualなproduction UIパスを含まないengine-onlyの歴史的な参考値である。UI合格の証拠として使わない。
- **旧current-headのnative UI実測（FAIL、3秒目標不合格）**: head `06f79289758d3d7189cb65ab230cc194b93621b2` で、macOS 26.6.2 arm64／Apple M3 Pro／internal SSD、warm cache・10,000 files、0-hit検索のproduction UI計時は40.121〜41.002秒で、3秒目標を満たさなかった。訂正済みの測定方法とUI event cap（`MAX_SEARCH_EVENTS_PER_POLL`等）の根拠は[PRコメント](https://github.com/hide212131/hane/pull/417#issuecomment-6042203652)に記載。
- **S10後続実装の制限値**（現行コードで確認済み、[content_search.rs](../crates/ui/src/view/content_search.rs)・[search.rs](../crates/session/src/search.rs)）: channel容量128件（`MAX_SEARCH_QUEUED_FILES`）、1 pollあたりのSearchEvent処理上限128件（`MAX_SEARCH_EVENTS_PER_POLL`）、最大128 displayed rows/frame（`MAX_SEARCH_ROWS_PER_FRAME`）、最大2 workers（`MAX_SEARCH_CONCURRENT_WORKERS`）、全体10,000 hits／32 MiB result-text上限（`MAX_SEARCH_HITS_TOTAL`／`MAX_SEARCH_RESULT_TEXT_BYTES`）。aggregate budgetは`SearchWork`ごとの`remaining_hit_budget`／`remaining_text_budget`（`AtomicUsize`、CASで予約）で管理し、まだ表示されていないstale結果（revision／generation不一致で`pending_file`が破棄された結果）が破棄されたときは、`release_global_delivery_budget`で同じ`SearchWork`（`SearchKey`で一致確認）に予約分を返却する（本PRのClippy修正対象。条件・返却対象の意味は変更していない）。
- **current head（`628ac0eca1521911d745b3b8e355a9bb7fc8758b`、本Clippy修正適用後も含む）の実測**: 利用者のMacが現在ロック中のため、CUAによるproduction UI計時が完了していない。current headでのfirst-result表示時間、全体検索速度、メモリ使用量、cold cache等は測定済みではない。**S10はcurrent headで未完了（未測定）のまま記録する。** 測定が可能になった時点で別途Commanderへ報告する。

## ローカル検証

この worker（Claude Code, repository mutation worker）はshell実行が利用できないため、`cargo test`／`cargo clippy`／benchmarkの再実行はできない。以下は過去の実行記録（historical、ローカルログ原本の`/tmp`パスは非公開のため失効）と、公開されているCI run・レビューへの参照に置き換えた現在の根拠を示す。

### historical（commit `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705`時点、Apple M3 Pro / macOS 26.6.2 / arm64、`rustc 1.98.1`）

- `cargo test --workspace --locked`: PASS（822 passed, 0 failed）。
- `cargo test --workspace --all-features --locked`: PASS（822 passed, 0 failed）。
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`: PASS（この時点のコード。後続のrebase・依存更新で発生したClippy指摘は本記録のCI run節を参照）。
- `git diff --check origin/main...HEAD`: PASS。
- macOS／Windows CI: [run 36740685239](https://github.com/hide212131/hane/actions/runs/36740685239) PASS。対象head `a7ffddd1ff4bf001a63ee45f3f55ffca99652d6c`、base `145188eef1f1353cfd903b0277458ece71ea8831`。両OSでworkspace testsとClippyがPASS。macOSではfallback glyph rasterizationとinput source reactivationもPASS。Windowsは最初の実行でClippyがUnix専用fixture変数を指摘したため、宣言に `#[cfg(unix)]` を付けて修正し、このcurrent-head runで再確認した。

### current head（`628ac0eca1521911d745b3b8e355a9bb7fc8758b`、本Clippy修正適用前）

- [current-head CI run 37664567624](https://github.com/hide212131/hane/actions/runs/37664567624): macOS／Windows双方のworkspace testsはPASS。双方のClippyが `crates/ui/src/view/content_search.rs:907` の `clippy::collapsible_if` でFAIL。
- 本修正（nested `if let`/`if` を `Option::filter` で一段にまとめ、条件・予約返却の意味は変更しない）をpr-head配下のソースへ適用済み。finalizerのpush後、exact worker result verify run（本指示時点では[run 37663000698](https://github.com/hide212131/hane/actions/runs/37663000698)が処理中）を含むcurrent-head CIの再実行結果をCommanderが確認する。
- macOS／Windows実画面（S09）: 利用者から完了報告あり（上表S09参照）。
- CodeRabbit full review: [review 5446292891](https://github.com/hide212131/hane/pull/417#pullrequestreview-5446292891)（レビュー対象head `3eb30f2a58e4afc5cb7a723ce39d63262868cd37`、本記録中の旧状態を指摘するfinding 1件、本更新で対応）。このレビューは`628ac0e`より前のheadを対象としたものであり、current head（本Clippy修正適用後の最終SHA）に対するfull reviewは別gateとして未実施。

## Commanderへ渡す残件

- S09はmacOS／Windows双方で利用者の実機確認完了の報告により完了として記録済み。残件なし。
- current head `628ac0eca1521911d745b3b8e355a9bb7fc8758b` のCIは、本記録更新時点でmacOS／Windows双方のworkspace testsがPASS、Clippyが`collapsible_if`でFAILしていた（[run 37664567624](https://github.com/hide212131/hane/actions/runs/37664567624)）。本workerはこのClippy指摘を修正済みだが、push・再実行はfinalizer側の作業であり、再実行結果の確認はCommanderに引き渡す。
- CodeRabbit full review（[review 5446292891](https://github.com/hide212131/hane/pull/417#pullrequestreview-5446292891)、対象head `3eb30f2a58e4afc5cb7a723ce39d63262868cd37`）のfinding 1件（本記録の古いqueue容量・S09 BLOCKED・性能判定・レビュー状態・`/tmp`限定根拠）は本更新で対応済み。current head（本Clippy修正適用後の最終SHA）に対するfull reviewは別gateとして未実施であり、current-head CI成功後にCommanderが明示的に実行・確認する。
- S10は現行実装の制限値（channel容量128、per-poll SearchEvent budget 128、最大128 displayed rows/frame、最大2 workers、10,000 hits／32 MiB上限）は確認済みだが、current head `628ac0e`（本Clippy修正適用後を含む）でのproduction UI計時は、利用者のMacがロック中のため未完了。旧head `06f792897` native UI実測（0-hit検索40.121〜41.002秒）は3秒目標を満たしていない。current headでの再測定が可能になるまでS10は未完了のまま記録する。
- 初期standalone engine-onlyベンチマーク（0-hit median 598.3 ms等）はproduction UI測定ではなく、UI合格の証拠として採用しない。

PRは `Refs #414` として [#417](https://github.com/hide212131/hane/pull/417) で共有済み。S09は完了。S10のproduction UI計時未完了、current-head CIのClippy修正後再実行、current-head CodeRabbit full reviewが残件。本workerはrepository mutationのうちClippy修正と本記録更新のみを行い、push／再実行結果の確認／merge／Issue closeは行わない。Commanderの再観測に委ねる。

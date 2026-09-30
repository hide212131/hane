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
- `crates/ui/src/view/content_search.rs`: sidebar検索状態、入力確定後のdebounce、検索世代、最大2worker、4件のbounded delivery、128一致行/frame、結果反映とstale判定、既存open完了後の検証・選択。
- `crates/ui/src/view.rs`, `view/sidebar.rs`, `view/sidebar_filter.rs`, `view/inline_rename.rs`, `actions.rs`: 画面接続、キーボードrouting、編集／workspace変更時の失効。
- `README.md`: 本文検索のショートカットとWork folder modeでの使い方。
- `Cargo.toml`, `Cargo.lock`, `crates/session/Cargo.toml`: 依存固定。

検索のために外部 `rg`、別の列挙器、全件 `FileService::load`、全文String複製、Markdown解析、新規session生成、保存は追加していない。既存WorkFolder一覧を通常検索で使い、「再検索」は既存scannerで更新する。既存のfile bufferとdraft snapshotをdiskより優先する。

## S01〜S10

各行のテスト／観測対象は実装commit `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705`。全10項目に対象SHAを明記した。証拠ログはそのcommitのソースで取得した。

| ID | テスト／観測 | 対象SHA | 結果 | 証拠 |
|---|---|---|---|---|
| S01 | `literal_search_reports_non_overlapping_utf8_byte_ranges`、`overlapping_candidates_are_reported_once_from_their_non_overlapping_starts`、`literal_search_does_not_interpret_regex_syntax`、`case_sensitive_search_can_be_disabled`、`query_validation_keeps_whitespace_and_rejects_newlines_and_oversize` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。文字列一致、大小文字、日本語、複数・重複範囲、空白・改行・query byte上限を確認。 | `/tmp/hane-414-evidence/workspace-default-dc6fde2.log` |
| S02 | `markdown_files_are_discovered_and_non_markdown_files_are_ignored`、`the_root_recovery_journal_directory_is_not_scanned`、`dotfile_directories_other_than_the_root_recovery_journal_are_still_scanned`、`scanner_keeps_gitignored_and_hidden_notes_but_skips_root_state` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | macOS／Windows CI PASS。`.MD`、hidden/gitignored markdown、root `.hane`除外、nested `.hane`保持を確認。Unix symlink targetの検査はmacOSのみ。filter／tree展開状態はscanner入力と別stateであることを実装経路で確認。 | `/tmp/hane-414-evidence/workspace-default-dc6fde2.log`、[current-head CI](https://github.com/hide212131/hane/actions/runs/36740685239) |
| S03 | `disk_search_does_not_load_or_save_a_document`、`search_prefers_an_existing_buffer_snapshot_over_disk`、`draft_search_uses_its_existing_session_snapshot`、`rope_snapshot_keeps_its_source_when_the_buffer_is_edited`、`search_sources_include_hidden_buffer_and_work_folder_draft_without_disk_duplicate` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。inactive tabのbuffer、同じpathのdisk除外、WorkFolder draftを含むsource一覧をGPUI testで確認。 | `/tmp/hane-414-evidence/workspace-all-features-dc6fde2.log` |
| S04 | `line_endings_preserve_source_byte_ranges_and_line_numbers`、`crlf_split_across_reads_is_one_line_and_bare_cr_is_a_line_break`、`bom_bytes_are_included_in_source_ranges`、`memory_reader_can_split_utf8_at_every_byte_boundary`、`rope_snapshot_streams_original_utf8_bytes_across_small_reads` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。LF／CRLF／CR、read境界のCRLF、BOM、EOF改行なし、UTF-8分割後も元のbyte範囲を保持。 | `/tmp/hane-414-evidence/workspace-all-features-dc6fde2.log` |
| S05 | `cancellation_is_observed_while_scanning_a_large_zero_match_file`、`a_full_result_queue_stops_sending_after_cancellation`、`cancelled_workers_still_use_the_two_controller_slots_until_removed`、`returning_to_a_query_after_an_intermediate_query_gets_a_new_epoch`、`a_late_terminal_event_cannot_complete_a_newer_query`、`dropping_the_search_controller_cancels_current_and_active_workers` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。zero-hit reader、満杯queueの取消、未停止workerが2 slotを占めること、A→B→A epoch、stale terminal、controller dropを確認。容量4、最大128一致行／frameを実装。実GUI frame待ちのoccupancyは未計測。 | `/tmp/hane-414-evidence/workspace-all-features-dc6fde2.log` |
| S06 | `per_document_limit_is_reported_and_does_not_claim_completion`、`invalid_utf8_discards_earlier_matches_in_the_same_file`、`a_truncated_utf8_scalar_at_eof_is_a_warning`、`nul_byte_is_reported_as_a_binary_warning`、`read_failure_discards_earlier_matches_in_the_same_file`、`changed_reader_stamp_discards_the_results`、`a_line_over_the_searcher_heap_limit_is_not_reported_as_complete`、`a_missing_disk_file_is_a_warning_instead_of_an_empty_result`、`result_delivery_respects_frame_hit_and_cumulative_text_limits`、`reaching_a_global_delivery_limit_is_reported_as_partial` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。文書あたり1,000件と配送中の全体10,000件／32 MiB上限をhelper testで確認し、上限時のPartial遷移も確認。エラー／打切りを正常完了へ読み替えない。 | `/tmp/hane-414-evidence/workspace-all-features-dc6fde2.log` |
| S07 | `a_stale_buffer_revision_does_not_move_to_an_old_search_result`、`a_superseded_navigation_id_cannot_select_a_search_hit`、`changed_disk_content_opens_without_reusing_the_old_search_position`、`a_late_terminal_event_cannot_complete_a_newer_query`、`a_late_open_completion_does_not_override_the_newer_search_result_selection` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。GPUI testで既存 `finish_open` 完了経路を新しい結果→古い結果の順に適用し、遅れて完了した古いopenが別文書の選択・active stateを上書きしないことを確認。buffer revision、navigation id、disk変更も確認。 | `/tmp/hane-414-evidence/workspace-all-features-dc6fde2.log` |
| S08 | `a_draft_search_result_selects_the_original_utf8_byte_range`、`a_disk_search_result_opens_through_the_existing_path_and_selects_its_utf8_range`、`changed_disk_content_opens_without_reusing_the_old_search_position`、`selecting_a_draft_hit_preserves_dirty_text_and_undo_history` | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | PASS。Draft／未読込diskの結果からbyte範囲を選択し、編集済みdraftのdirty本文とundo履歴を保つことをGPUI testで確認。リンク／表／コードの実画面表示はS09に残す。 | `/tmp/hane-414-evidence/workspace-all-features-dc6fde2.log` |
| S09 | `Cmd/Ctrl+Shift+F`、Enter／Tab／Esc／上下キーのroutingは実装済み。macOS appのaccessibility state取得を2回試行。 | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | BLOCKED。アプリ一覧ではHane稼働を確認したが、CUAのHane接続／accessibility取得が2回とも `-10005 timeoutReached`。macOS native操作、IME変換、focus、copy/paste、結果移動、sidebar復元は未実施。Windowsの同じ実画面確認は依頼者が担当。CIはnative GUI／日本語IMEを検証しない。 | `/tmp/hane-414-evidence/macos-gui-observation-dc6fde2.txt` |
| S10 | release benchmark: 1,000／10,000 markdown、各8 KiB、ASCII／日本語混在、0／1／4 hit/file、3回のwarm-cache run。scanner・search・bounded collectorを測定し、250 ms debounceと16 ms/128行frame近似を含めた。 | `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705` | 部分測定。10k files: fixture generation 844.7 ms、初回scanner 6.5 ms。0-hit median 598.3 ms（first resultなし）、1-hit median 2,336.2 ms／first-row estimate 266.6 ms、4-hit (10k global cap) median 1,893.8 ms／estimate 266.6 ms。1k end medianは0／1／4 hitで284.7／440.1／919.9 ms。benchmark process max RSS 11,010,048 bytes。実GPUI描画時間、増分RSS、cold-cacheとstorage別測定は未実施。Apple M3 Pro / macOS 26.6.2 / arm64 / Rust 1.98.1での初期値で、保証ではない。 | `/tmp/hane-414-evidence/search-bench-final-dc6fde2.log` |

## ローカル検証

Apple M3 Pro / macOS 26.6.2 / arm64、`rustc 1.98.1`。default system Rust 1.93.1では依存 `gpui-pre 0.3.6` の `std::hint::cold_path` を使えなかったため、repo指定のrustc/rustdoc/cargoを明示して実行した。すべて対象コードSHA `dc6fde26d5154a6d5a86d7dce014cb3f36ca2705`。

- `cargo test --workspace --locked`: PASS（822 passed, 0 failed）。
- `cargo test --workspace --all-features --locked`: PASS（822 passed, 0 failed）。
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`: PASS。
- `git diff --check origin/main...HEAD`: PASS。
- macOS／Windows CI: [run 36740685239](https://github.com/hide212131/hane/actions/runs/36740685239) PASS。対象head `a7ffddd1ff4bf001a63ee45f3f55ffca99652d6c`、base `145188eef1f1353cfd903b0277458ece71ea8831`。両OSでworkspace testsとClippyがPASS。macOSではfallback glyph rasterizationとinput source reactivationもPASS。Windowsは最初の実行でClippyがUnix専用fixture変数を指摘したため、宣言に `#[cfg(unix)]` を付けて修正し、このcurrent-head runで再確認した。
- macOS実画面: CUAからHaneのAX stateを読めずBLOCKED。Windows実画面: 依頼者が検証予定。

ログ: `/tmp/hane-414-evidence/workspace-default-dc6fde2.log`、`workspace-all-features-dc6fde2.log`、`clippy-dc6fde2.log`、`diff-check-dc6fde2.log`。性能実行ではrelease後に `rust-objcopy` がLLVM dylib不在でdebug strippingを失敗した警告が出たが、benchmark binaryは生成・実行され、終了code 0。

## Commanderへ渡す残件

- S09のmacOS/Windows実画面受入、日本語IME、focus、paste、sidebar復元。macOS CUA timeout、Windowsは依頼者による確認待ち。
- current-head macOS／Windows CIはPASS。PRはDraftのためCodeRabbit full reviewは未実施（自動reviewはDraftにつきskip）。実画面受入後のreview進行はCommanderへ引き渡す。
- S10は一環境のsearch/controller近似測定のみ。GPUI描画・操作可能性・incremental memory・cold cache・storage実体は未計測。
- 500 ms／3 sは初期改善目標として扱い、保証または全環境での合格とはしない。

PRは `Refs #414` としてDraftで共有済み。CIはPASS。依頼者のWindows実画面確認と、未実施のmacOS実画面確認・CodeRabbit full reviewを残す。Commanderのreview／GUI受入後にmerge／Issue closeへ進み、本作業ではmerge／Issue closeを行わない。

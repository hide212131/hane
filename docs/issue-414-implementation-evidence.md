# Issue #414 実装・検証記録

## 着手時の状態と対象

- 着手時に確認した `main` の完全SHA: `5be310c6136e662119e2d208322f832b317e14c4`。
- 作業branch: `codex/issue-414-p1-search-core`。Issue #414の既存実装PR／書き込み担当は確認されず、PRは作成していない。#412／#397はOpen／Draftで別責務。#415のeditor findも変更範囲外。#297全体やAI設定の完了は待たない。
- latest Issue本文・コメント、default branchの `AGENTS.md` と `docs/aadw-command-policy.md`、添付指示書を着手時に確認した。source tree内の `sources/` は変更していない。
- 作業中にdefault branchが `145188eef1f1353cfd903b0277458ece71ea8831` へ進んだことを再取得で確認した。PR #412のmergeによる `session_save` 分離が `view.rs` と重なるため、変更範囲を比較したうえでbranchをcurrent mainへrebaseし、既存の `mod session_save;` と新しい `mod content_search;` を両方維持した。移動済みのdraft retire経路も更新し、以下の最終検証をrebase後のコードで再実行した。
- 対応範囲: P1検索コア、P2 sidebar/Inputとworker管理、P3既存open完了・選択経路への接続。変更はdocument/sessionの読み取り境界、UIのcontent search用moduleと最小のrouting、テスト、本文書に限定。
- 検索依存: `grep-searcher 0.1.17`、`grep-regex 0.1.14`、`grep-matcher 0.1.8`。

### 変更ファイル

- `crates/document/src/lib.rs`: Rope共有スナップショットのchunk reader。
- `crates/session/src/search.rs`, `service.rs`, `testing.rs`, `workfolder.rs`, `lib.rs`: streaming reader、検索型・制限・UTF-8検証とripgrep検索、テスト用reader、既存scannerの対象規則確認。
- `crates/ui/src/view/content_search.rs`: sidebar検索状態、入力確定後のdebounce、検索世代、最大2worker、4件のbounded delivery、128一致行/frame、結果反映とstale判定、既存open完了後の検証・選択。
- `crates/ui/src/view.rs`, `view/sidebar.rs`, `view/sidebar_filter.rs`, `view/inline_rename.rs`, `actions.rs`: 画面接続、キーボードrouting、編集／workspace変更時の失効。
- `README.md`: 本文検索のショートカットとWork folder modeでの使い方。
- `Cargo.toml`, `Cargo.lock`, `crates/session/Cargo.toml`: 依存固定。

検索のために外部 `rg`、別の列挙器、全件 `FileService::load`、全文String複製、Markdown解析、新規session生成、保存は追加していない。既存WorkFolder一覧を通常検索で使い、「再検索」は既存scannerで更新する。既存のfile bufferとdraft snapshotをdiskより優先する。

## S01〜S10

各行の対象は、この文書を含む実装commitの完全SHA（タスク引き渡しに記載）である。通常workspace testsとall-features workspace testsは同一の最終ソースツリーに対して実行した。

| ID | テスト／観測 | 結果 | 証拠 |
|---|---|---|---|
| S01 | `literal_search_reports_non_overlapping_utf8_byte_ranges`、`overlapping_candidates_are_reported_once_from_their_non_overlapping_starts`、`literal_search_does_not_interpret_regex_syntax`、`case_sensitive_search_can_be_disabled`、`query_validation_keeps_whitespace_and_rejects_newlines_and_oversize` | PASS。文字列一致、大小文字、日本語、複数・重複範囲、空白・改行・query byte上限を確認。 | `/tmp/hane-414-evidence/workspace-default.log` |
| S02 | `markdown_files_are_discovered_and_non_markdown_files_are_ignored`、`the_root_recovery_journal_directory_is_not_scanned`、`dotfile_directories_other_than_the_root_recovery_journal_are_still_scanned`、`scanner_keeps_gitignored_and_hidden_notes_but_skips_root_state` | macOS PASS。`.MD`、hidden/gitignored markdown、root `.hane`除外、nested `.hane`保持、Unix symlink targetを辿らないことを確認。filter／tree展開状態はscanner入力と別stateであることを実装経路で確認。Windows CI未実施。 | `/tmp/hane-414-evidence/workspace-default.log` |
| S03 | `disk_search_does_not_load_or_save_a_document`、`search_prefers_an_existing_buffer_snapshot_over_disk`、`draft_search_uses_its_existing_session_snapshot`、`rope_snapshot_keeps_its_source_when_the_buffer_is_edited`。`content_search_sources` の既存session優先・path dedupe・folder draft収集をコード確認。 | コアPASS。hidden tabのsource収集と同一path dedupeは実装済みだが、その組み合わせを単独で検証するUI受入testは未作成。 | `/tmp/hane-414-evidence/workspace-all-features.log` |
| S04 | `line_endings_preserve_source_byte_ranges_and_line_numbers`、`crlf_split_across_reads_is_one_line_and_bare_cr_is_a_line_break`、`bom_bytes_are_included_in_source_ranges`、`memory_reader_can_split_utf8_at_every_byte_boundary`、`rope_snapshot_streams_original_utf8_bytes_across_small_reads` | PASS。LF／CRLF／CR、読取境界のCRLF、BOM、EOF改行なし、UTF-8分割後もsource byte範囲を保持。 | `/tmp/hane-414-evidence/workspace-all-features.log` |
| S05 | `cancellation_is_observed_while_scanning_a_large_zero_match_file`、`a_full_result_queue_stops_sending_after_cancellation`、`cancelled_workers_still_use_the_two_controller_slots_until_removed`、`returning_to_a_query_after_an_intermediate_query_gets_a_new_epoch`、`a_late_terminal_event_cannot_complete_a_newer_query`、`dropping_the_search_controller_cancels_current_and_active_workers` | PASS。zero-hit reader、full queue取消、未停止workerが2 slotを占めること、A→B→A epoch、stale terminal、controller dropを確認。delivery channel容量4と128行/frameは実装上限。実GUIのframe queue occupancyは未計測。 | `/tmp/hane-414-evidence/workspace-all-features.log` |
| S06 | `per_document_limit_is_reported_and_does_not_claim_completion`、`invalid_utf8_discards_earlier_matches_in_the_same_file`、`a_truncated_utf8_scalar_at_eof_is_a_warning`、`nul_byte_is_reported_as_a_binary_warning`、`read_failure_discards_earlier_matches_in_the_same_file`、`changed_reader_stamp_discards_the_results`、`a_line_over_the_searcher_heap_limit_is_not_reported_as_complete`、`a_missing_disk_file_is_a_warning_instead_of_an_empty_result` | core PASS。文書上限と不正UTF-8／NUL／I/O／stamp／長行／open失敗の分類を確認。global 10,000-hit・32 MiB UI反映上限の専用behavior testは未作成。 | `/tmp/hane-414-evidence/workspace-all-features.log` |
| S07 | `a_stale_buffer_revision_does_not_move_to_an_old_search_result`、`a_superseded_navigation_id_cannot_select_a_search_hit`、`changed_disk_content_opens_without_reusing_the_old_search_position`、`a_late_terminal_event_cannot_complete_a_newer_query` | 部分PASS。buffer revision、navigation id、検索後のdisk内容変更、遅い検索terminalを確認。delayを注入した2件の同時disk open競合／結果連打を直接試すtestは未作成。edit・rename・folder変更の失効経路は実装接続済み。 | `/tmp/hane-414-evidence/workspace-all-features.log` |
| S08 | `a_draft_search_result_selects_the_original_utf8_byte_range`、`a_disk_search_result_opens_through_the_existing_path_and_selects_its_utf8_range`、`changed_disk_content_opens_without_reusing_the_old_search_position` | 部分PASS。Draftと未読込disk結果が既存open完了経路を通ってUTF-8 byte位置を選ぶこと、disk変更時は選択しないことをGPUI testで確認。link/table/codeの実画面表示、未保存／Undo状態の全保持はnative GUI未確認。 | `/tmp/hane-414-evidence/workspace-all-features.log` |
| S09 | `Cmd/Ctrl+Shift+F`・Enter／Tab／Esc／上下キーはrouting実装済み。macOS GUI操作を要求したところ、画面操作APIが `The Mac is locked and automatic unlock could not unlock it. Ask the user to unlock the Mac manually before continuing.` を返した。 | 未実施。Macを解除したり制限を回避していない。Windows実画面／CIもこの作業環境から利用できない。日本語IME変換中Enter、focus、copy/paste、結果移動、sidebar復元を検証済みとは扱わない。 | 作業時の画面操作応答。CI runなし。 |
| S10 | releaseの一時benchmark `/tmp/hane-414-evidence/search-bench/src/main.rs`。1,000／10,000 markdown、各8 KiB、ASCII／日本語混在、0／1／4 hit/file、3回のwarm-cache run。scanner・generation・search+bounded collectorを分け、250 ms debounceを合算。16 ms/128行のcollector frame待ちを近似。 | 部分測定。10k files: fixture generation 783.7 ms、初回scanner 5.7 ms。0-hit end median 606.8 ms（first result N/A）、1-hit median 2,907.3 ms／first row estimate 266.4 ms、4-hit (global 10k cap) median 2,095.6 ms／first row estimate 266.9 ms。1k filesのend medianは0／1／4 hitで285.4／483.4／996.6 ms。benchmark process max RSS 11,796,480 bytes（全プロセス値。増分RSS未測定）。storage種別と実GPUI描画時間は未測定。この値はApple M3 Pro / macOS 26.6.2 / arm64 / Rust 1.98.1のwarm-cache一環境での初期測定であり、保証ではない。 | `/tmp/hane-414-evidence/search-bench.log`、`search-bench-time.log`。 |

## ローカル検証

Apple M3 Pro / macOS 26.6.2 / arm64、`rustc 1.98.1`。default system Rustはworkspaceのeditionを扱えなかったため、指定toolchainのrustc/rustdoc/cargoを明示して実行した。

- `cargo test --workspace --locked`: PASS（817 passed, 0 failed）。
- `cargo test --workspace --all-features --locked`: PASS（817 passed, 0 failed）。
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`: PASS。
- `git diff --check`: PASS。
- macOS/Windows CI: 未実施（このbranchにPRを作っていないため）。
- 実画面: Macがロック中で未実施。Windowsも未実施。

ログ: `/tmp/hane-414-evidence/workspace-default.log`、`workspace-all-features.log`、`clippy.log`。性能実行にはrelease link後の `rust-objcopy` がLLVM dylib不在でdebug strippingを失敗した警告が出たが、benchmark binaryは生成・実行され、終了code 0。これは性能実行結果に影響しない。

## Commanderへ渡す残件

- S09のmacOS/Windows実画面受入、日本語IME、focus、paste、sidebar復元。
- Windows CIとcurrent-head CI。
- S03のhidden-tab／draft dedupe UI test、S06のglobal result/text limit test、S07のdelayed open競合と結果連打test、S08の文書状態保護test。
- S10は一環境のsearch/controller近似測定のみ。GPUI描画・操作可能性・incremental memory・cold cache・storage実体は未計測。
- 500 ms／3 sは初期改善目標として扱い、保証または全環境での合格とはしない。

ローカルbranchの実装・検証資料までを提出する。PR作成、review依頼、GUI受入、merge、Issue closeはCommanderへ引き渡す。

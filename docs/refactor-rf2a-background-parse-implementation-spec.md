# RF2-A 第6作業: background parse の機械的分離 — 実装設計

作成日: 2026-10-01（JST）  
対象: [Issue #301](https://github.com/hide212131/hane/issues/301) / 親 #297  
状態: **設計。製品コードは未変更、実装・テスト・GUI検証は未実施。**

## 1. 今回の決定

次の製品作業は、`crates/ui/src/view.rs` に残る `schedule_document_parse` と `schedule_joined_parse` を、新規 `crates/ui/src/view/background_parse.rs` へ移す一つのPRとする。挙動、状態の所有、解析器、描画手順は変更しない。

実装PRのタイトル案は `refactor(RF2-A): 背景解析をview/background_parse.rsへ分離 (#301)`、branch案は `refactor/301-background-parse`。本文は `Refs #301` とし、`Closes #301` は使わない。本設計を保存するdocs branch/PRと、後で作る製品実装branch/PRを区別する。

## 2. 調査基準と選定理由

基準mainは `145188eef1f1353cfd903b0277458ece71ea8831`。調査した `view.rs` のblobは `95a5c02713198a0013ebf969bd4f06dda53da61b`。資料登録直前にもmainのrefが同SHAであることを確認した。

保存・draft・H1同期の分離はPR #412で受入済みで、上記mainはそのmerge commitである。既にある `view/sidebar.rs`、`sidebar_filter.rs`、`viewport.rs`、`inline_rename.rs`、`session_save.rs` の分離を再実装しない。Issue #301全体はopenで、残る責務は実行計画第9.12節に記録されている。

既存の全体案にはopen/scan/sessionの分離もあるが、今回はbackground parseを先にする。調査時に確認した並行PRの実差分は次のとおり。これは調査時点の観測であって、将来の無競合を保証する表ではない。

| PR | 読んだ実差分 | 今回の判断 |
| --- | --- | --- |
| #397 AI設定 | `view.rs`の初期化、外部open、settings、renderなど。対象2メソッド本体の変更なし | settings/open/renderを今回動かさない |
| #417 Work folder本文検索 | `view.rs`の状態・初期化、after_input、session切替、finish_open、sidebar幅など。対象2メソッド本体の変更なし | 呼出元の追加処理を消さない。open/scan/sessionは別作業 |
| #415 文書内Find | 変更ファイルは調査時点で `crates/editor/src/find.rs` と `crates/editor/src/lib.rs` | 現時点の直接重複なし。後続UI実装時は再確認 |
| #416 ファイルタブ | 調査時点の変更ファイル一覧は空 | 将来のタブ実装を今回の移動に混ぜない |

同じ `view.rs` へのmodule宣言追加など、機械的な競合は起こり得る。各PRの完了や#297全体を一律に待たせず、実装開始時のcurrent head/baseと実差分で判断する。未mergeの他branchからコードを持ち込まない。停止中の#395を再開・移植しない。

RF0の契約確認は `docs/refactor-execution-plan.md` 第9節の当該UI領域を読む。#298全体や無関係な運用Issueを待ち条件にせず、今回の解析・高さ・遅延結果に必要な契約が未確定なら、その範囲を明記して先に確認する。

## 3. 変更するファイルと厳密な移動範囲

製品実装PRの通常の変更ファイルは次の3件に限定する。

| ファイル | 変更内容 |
| --- | --- |
| `crates/ui/src/view.rs` | `mod background_parse;`を追加し、対象2メソッドをdoc comment・inline commentごと取り除く。それ以外の実行コードは変えない |
| `crates/ui/src/view/background_parse.rs` | `use super::*;` と `impl EditorView` を置き、2メソッドを元の相対順に移す |
| `docs/refactor-execution-plan.md` | 次の空き節へ実際の旧→新対応、可視性理由、残す責務を追記する。現基準では第9.13節が候補 |

移動する署名は以下の2件。変更するのは可視性の `pub(super)` 追加だけであり、引数・戻り値・名前は維持する。

```rust
pub(super) fn schedule_document_parse(&mut self, cx: &mut Context<Self>)
pub(super) fn schedule_joined_parse(&mut self, blocks: &[IndexedBlock], cx: &mut Context<Self>)
```

これは署名の指定であり、実装の雛形ではない。bodyを要約や再実装で置き換えず、着手時に受入済みのmainから移す。`Self::schedule_document_parse`を参照するdoc linkも保持する。

両メソッドは親の `impl Render for EditorView` 等から呼ばれるため、移動後は `pub(super)` が必要。`pub` / `pub(crate)`には広げない。既存兄弟moduleの `use super::*;` に揃え、親のimportを一斉整理しない。フィールドの公開、wrapper、trait、Manager、別Entityは追加しない。

### 親に残すもの

- `EditorView` の定義、すべてのfield、`from_sessions`の初期化、`on_document_replaced`のリセット。
- `DocumentKey`、`JoinedParseJob`、`JoinedBlockCache`、`Granularity`、`HeightBlocks`、`MAX_JOINED_PARSE_JOBS`。
- `document_key`、`block_context_revision_is_current`、`height_snapshot_matches_line_height`と、index・height・cacheの共有メソッド。
- `after_input`、`activate_session`、新規note作成、`finish_open`、`impl Render for EditorView`、既存の全テストとfixture。

以前の全体案に挙がった `JoinedParseJob` / cache型まで今回移さないのは、親のfield宣言・cache読出し・テストも利用しており、型とfieldの可視性拡大を避けるためである。第5作業の保存系分離と同じく、今回は「処理配置の分離」であって「状態所有者の移譲」ではない。

`actions.rs`、`input.rs`、`capture.rs`、既存 `view/*.rs`、`line.rs`、`shape.rs`、session/markdown/presentation crate、Cargo設定、lock、CI workflowは変更対象外。テスト追加など例外が必要なら、理由と差分を実装前に明示する。

## 4. 呼出関係と状態寿命

呼出しの書き方は `self.schedule_document_parse(cx)` / `self.schedule_joined_parse(&blocks, cx)` のまま維持できる。新module名を付けた関数呼出しや中継メソッドへ書き換えない。

| 経路 | 維持する順序・境界 |
| --- | --- |
| `after_input` | incremental index/height更新 → document parse要求 → boundedな開示高さ更新 → caret表示調整 → autosave/draft/H1要求。順序を入れ替えない |
| `activate_session` | session切替 → `on_document_replaced` → document parse要求 → notify |
| 新規note / `finish_open` | 文書差し替え後の既存parse要求を維持する。open結果受理や検索ナビゲーションを新moduleへ含めない |
| `render` | theme反映 → wheel zoom更新 → settings早期return判定 → document parse要求。その後、visible blocks決定 → joined parse要求 → cache保持範囲調整 →表示/layout取得 |
| background completion | 元のclosure、capture、clone、background executor、notify、detachをそのまま保持 |

`DocumentKey` は `SessionId` とactive sessionの `generation()` の組であり、`Revision`とは別の識別である。work-folderのgenerationへ置き換えない。

初期化では `document_parse_job_running=false`、joined running数は0、job/cacheのmapは空。文書差し替え時には文書依存のindex/cache/job map等を消すが、**`document_parse_job_running` と `joined_parse_jobs_running` はリセットしない**。実際に走っている旧文書の処理が終了するまで容量を消費するためである。新しいcleanupやDrop処理を追加しない。

## 5. document parseの保存すべき契約

この節は基準コードの説明であり、新しいアルゴリズムの指示ではない。実装では元の条件式とbodyを保持する。

1. formal parseの必要性に加え、非空の開示範囲の更新、selectionからcaretへのcollapse、`force_height_disclosure_snapshot`を検査する。単にrevisionが同じという理由で高さsnapshotを省略しない。
2. 実行中なら新しいdocument parseを開始しない。開始時のforce消費、runningフラグ設定、DocumentKey・revision・line-height bits・開示範囲・前回の完全高さsnapshotのcapture順を保持する。
3. 40msの待機後、重い解析を始める前にDocumentKey、revision、line-heightの一致を再確認する。不一致ならrunningを解放して現在の文書に再要求する。
4. `BlockIndex::from_buffer` と `HeightIndex::new(block_heights_with_disclosure(...))` はbackground executor上に残す。UI側へ全文parseや全量高さ計算を移さない。
5. 完了時はrunningを先に解放し、DocumentKeyまたはline-heightが不一致なら再要求してその結果を使わない。line-heightの比較は既存helperのbit単位一致を維持し、epsilon比較等へ変えない。
6. formal indexは `BlockIndexState::publish(..., IndexSource::Formal, current_document)` に渡す。`Published` と `Rebased(_)` を受け入れる既存契約を維持する。完了時に一律revision完全一致を追加すると、既存のrebaseによる受理を壊すので禁止する。
7. index更新時だけ `background_presentation_generation` 更新、`block_cache.clear()`、`joined_parse_cache.clear()`を行う。`PublishOutcome`の分岐をまとめない。
8. 背景で作った高さをそのまま利用できるのは、現在revision・現在の開示範囲・Blocks粒度・要素数の条件が成立する場合。**indexがRebasedで受理されたことと、古い高さsnapshotを採用できることは同義ではない。**
9. `NotMoreAuthoritative`時の `install_disclosure_heights_preserving_measurements` を維持する。selectionだけの変化で測定済み折返し・画像高さや表示cacheを無条件に捨てない。
10. 解析中にselection/caretが動いた場合は、現行の再snapshot要求、bounded endpoint更新、`force_height_disclosure_snapshot`を使うcollapse再試行を保持する。caret移動のたびに全文snapshotを作る処理へ置き換えない。

`finish_document_parse`等の新しい補助メソッドへの再分割は行わない。moveとcallback再構成を混ぜると、capture値・解放順・再要求の意味が確認しづらくなる。

## 6. joined parseの保存すべき契約

| 段階 | 変更してはいけない内容 |
| --- | --- |
| admission | `joined_parse_jobs_running >= MAX_JOINED_PARSE_JOBS`ならbreak。上限は現行の2。上限はブロック間・文書切替をまたぐview全体に効く |
| 対象選択 | joinable判定、line span取得、既存の同期parse budget判定の順序を保持。短いブロックまで背景job化しない |
| 重複回避 | 同BlockIdのpending job、またはrevision/source rangeが一致するcacheがあればskip |
| snapshot | trailing blank linesを除く範囲、RopeBuffer clone、BlockId、DocumentKey、revision、source rangeをそのままcapture |
| 起動 | job mapへ登録しrunning数を増やしてからspawn。満杯時の古いviewport要求をqueueへ積まない |
| 完了時の容量解放 | **running数を減らしnotifyしてから**鮮度を判定。旧文書の完了も容量解放が必要 |
| pending所有権 | DocumentKey一致と `joined_parse_jobs.get(&id) == Some(&job)`を確認した後にだけmapからremove。古い完了で新しいsnapshotのpendingを消さない |
| 結果の鮮度 | current revision、current indexで解決するBlockId/source rangeの完全一致を要求。ここへformal indexのrebase規則を流用しない |
| 結果受理 | Some(parse)だけcacheへ入れ、該当blockのpresentation cacheを除去してnotify。検証のための同期再parseを追加しない |

TaskのdetachをTask保持・cancel方式へ変えない。同期parseが既に実行中ならTaskを捨ててもCPU処理を直ちに止められるとは限らず、現行コードは実完了までrunning数に含めている。

## 7. 既存テストへの対応

以下は基準 `view.rs` で定義を確認した既存名であり、この設計作業で実行した結果ではない。テストは親に残し、名前・assert・fixture・実行条件を削らない。抽出後に同じテストが新配置のメソッドを通ることを確認する。

| 保護する契約 | 既存テスト名 |
| --- | --- |
| スクロール/文書切替をまたぐ容量制限 | `joined_parse_work_is_bounded_across_scrolling_and_document_switches` |
| 古いrevisionで現在の行cacheを壊さない | `joined_parse_completion_rejects_an_old_revision_without_evicting_current_rows` |
| BlockIdとsource range双方の照合 | `joined_parse_completion_requires_the_current_block_identity_and_range` |
| 別snapshotのpending entryを保持 | `joined_parse_completion_does_not_clear_another_snapshots_pending_job` |
| zoom等でline-heightが変わったsnapshotを拒否 | `background_height_snapshot_must_match_the_current_line_height` |
| caretが所有するfence blockの高さ | `background_formal_parse_keeps_the_caret_owned_fence_block_visible` |
| parse中の開示移動と高さ整合 | `background_formal_parse_rebuilds_heights_when_disclosure_moves_during_parse` |
| selection全域のfence高さ | `background_selection_height_snapshot_covers_the_selected_fence_range` |
| 測定済み高さ/表示cacheの保持 | `background_selection_snapshot_preserves_measured_height_and_cached_presentation` |
| selection終了時の中間block collapse | `background_selection_height_snapshot_collapses_middle_blocks_when_selection_ends` |
| collapse中のcaret移動の再試行 | `background_collapse_snapshot_retries_when_caret_moves` |

この一覧だけが回帰範囲ではない。既存のBlockIndexState/publish/rebase、layout/cache、table・fence・caret、zoom等のテストもworkspace testsで維持する。既存coverageの不足が判明した場合は、不足する条件と期待結果を明記し、必要最小限のtest-only追加を切り分ける。製品ロジック変更を「テストを通すため」に混ぜない。

## 8. 実装手順

### P0 — 開始時の事実固定

current main、#301の最新コメント、#412の受入、関連open PRとそのhead/diff、current `AGENTS.md` / Commander Policyを再取得する。mainが本設計のSHAから動いていたら、対象2メソッドとcaller/stateへの変更を確認して新しい基準SHAをPRへ記す。旧SHAを機械的にcheckoutして未受入コードを増やすのではない。

新たな仕様判断が必要な直接重複がなければ、受入済みmainから専用の製品branchを作る。この設計PRへ製品コードを足さない。現行AADWの実装担当・trusted finalizerを使い、workerへshell/test/push権限を足さない。本設計の登録だけではhandoffを発火しない。

### P1 — 純粋な移動

2メソッドをdoc commentから最終braceまで、元の相対順で新moduleへ移す。`mod background_parse;`と最小可視性だけを調整する。状態・helper・caller・tests・既存importはそのまま残す。無関係な整形をしない。

### P2 — 差分の静的検査と記録

元と移動後のそれぞれについて、bodyの文字列、doc/inline comment、属性を比較する。許容するコード上の差は配置・module/import追加・2件の `pub(super)` だけ。単なる `git diff --check` やコンパイル成功は意味同一の証拠にならないため、移動前後の比較も記録する。

`EditorView`全field、初期化、`on_document_replaced`、callerの順序、テスト定義が不変であることを差分で確認する。両メソッドの定義が新moduleに各1件だけあること、旧定義や一時stubが残っていないことを確認する。実行計画へ旧→新の2件と残す責務を追記する。

### P3 — 安定した候補で検証

通常/all-features、lint、必要なCIとレビューを同じ候補headに結び付ける。結果の記録はPRコメントを使い、CI結果だけを追記するdocs-only commitを繰り返して検証対象headを動かさない。failureは原因と適用範囲を区別し、未実施/blockedをpassへ読み替えない。

## 9. 実行する検証とGUI判断

以下は**実装後に信頼された実行環境で行うコマンド**であり、本設計作業の実行済み一覧ではない。

```sh
cargo test -p hane-ui --locked -- --list
cargo test -p hane-ui --locked joined_parse_
cargo test -p hane-ui --locked background_
cargo test --workspace --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
git diff --check
```

focused filterが0件で成功していないことを確認し、第7節の既存名と照合する。macOS/Windowsの関連CIをcurrent headと必要なbase contextで確認する。all-features成功だけを通常features成功の代わりにしない。

既存のformat/環境/不安定テストの問題が出た場合は、同条件のbaseと区別して原因を記録する。全体format修正や無関係なflake修正をこのPRへ混ぜず、required check失敗を独断で免除しない。対象外の問題を直す必要がある場合も別の原因単位として扱う。

この作業はrender登録やfocus配線を変更しないため、native GUIの追加要否は最終差分と既存証拠からCommanderが判断する。省略する場合も「移動body/caller/state不変、該当自動回帰の結果」という根拠をPRに残し、GUI実施済みとは書かない。必要と判断したGUIの未実施/fail/blockedを機械的移動という理由で免除しない。

focused GUIを行う場合の範囲は次とする。

| ID | 操作 | 確認点 |
| --- | --- | --- |
| G1 | 既存同期budgetを超える長いjoinable blockを含む文書を開き、遠くまでスクロールして戻る | 背景解析後の表示が現在のblockに反映され、行や装飾が他文書のものに置き換わらない |
| G2 | 解析が発生する文書AからBへ切替え、Bを編集する | 遅いAの結果がBの表示・内容を上書きせず、後続のBの表示更新も止まらない |
| G3 | 複数fenceをまたぐselectionを作ってcaretへ戻し、zoomも操作する | 開示/collapse、caret、scrollbarが整合し、古いline-heightのsnapshotで崩れない |

G2のraceや上限値の証明は画面観測だけに頼らず、第7節の自動テストで行う。実施OS、candidate SHA、build、操作、結果と未確認範囲を記録する。macOSの結果をWindows native確認済みとして扱わない。性能比較は本作業では主張しない。アルゴリズムや実行経路の変更が必要になれば本PRから分離し、#23等の同条件baselineの検証を別途計画する。

## 10. 実装PRの受入条件

- [ ] 移動先に2メソッドがあり、body・属性・既存コメントと相対順を保持している。
- [ ] 可視性変更は2件のprivate→`pub(super)`だけ。外部API、field、初期化/reset、caller、既存testが不変。
- [ ] 第5・6節の鮮度条件、rebaseの区別、容量解放順、40ms、同時実行上限2、background executor、detach/notifyの順序を変えていない。
- [ ] old→new対応と、型/state/helperを親に残す理由が実行計画・PRから分かる。
- [ ] 通常/all-features、lintとmacOS/Windowsの必要checksについてcurrent headの結果が揃い、未実施や無関係なfailureは区別されている。
- [ ] 最終候補のcurrent-head CodeRabbit full reviewと必要と判断したGUI evidenceを確認し、Issue成立を妨げるblockerが残っていない。指摘件数ゼロ自体を目的にしない。
- [ ] merge直前のexpected head、target/base、mergeabilityを確認する。今回のdocs PR登録ではmergeせず、実装完了後も#301全体は閉じない。

## 11. 後続に残すものと復旧

次回以降の#301にはopen/scan/session、height/layout cache、pointer/panel、settings、measurement、render等の残存配置を引き渡す。background parseの型/stateが親に残ることも明示し、全責務分離の最終判定で再確認する。

#306にはsnapshot/projectionと正式index・joined parseの所有権/契約整理、#368にはcache無効化・保持量・job寿命を残す。#307のgeometry、#308/#309の保存等の状態所有を今回先取りしない。Mermaid、表、検索、scroll inertia等の機能追加もこのPRに含めない。

データ形式・保存形式・設定形式は変わらず、移行処理は不要。復旧単位はこの機械的移動PR一つのrevert。後続が新配置を編集している場合は依存差分を確認して逆順に戻し、別機能の変更を消さない。

## 12. 根拠と設計時の確認範囲

- [対象Issue #301](https://github.com/hide212131/hane/issues/301) の本文・最新コメント。
- [基準view.rs](https://github.com/hide212131/hane/blob/145188eef1f1353cfd903b0277458ece71ea8831/crates/ui/src/view.rs): 対象2メソッド、DocumentKey、field/初期化/reset、after_input、render、既存テスト名。
- [既存session_save.rs](https://github.com/hide212131/hane/blob/145188eef1f1353cfd903b0277458ece71ea8831/crates/ui/src/view/session_save.rs): 子moduleと可視性の既存パターン。
- [実行計画](https://github.com/hide212131/hane/blob/145188eef1f1353cfd903b0277458ece71ea8831/docs/refactor-execution-plan.md): 第9節、特に第9.12節と残件。
- [AGENTS.md](https://github.com/hide212131/hane/blob/145188eef1f1353cfd903b0277458ece71ea8831/AGENTS.md)、[Commander Policy](https://github.com/hide212131/hane/blob/145188eef1f1353cfd903b0277458ece71ea8831/docs/aadw-command-policy.md)。実装時はcurrent default branchを再読する。
- [PR #397](https://github.com/hide212131/hane/pull/397)、[PR #417](https://github.com/hide212131/hane/pull/417)の変更ファイルとview.rs patch、[PR #415](https://github.com/hide212131/hane/pull/415)、[PR #416](https://github.com/hide212131/hane/pull/416)の変更ファイル一覧。

設計段階ではGitHubのsource・差分・Issueを読んだ。製品変更、Rustのbuild/test/lint、native GUI、性能計測は行っていない。表のテスト名は既存定義の確認であり成功証拠ではない。既定のlocal Jev CLIはこの実行環境に存在せず、返答を捏造せずCommander Policy第2.8節の未応答時規則に従って、今回の設計資料登録を選定した。実装workerの起動、PR #395の再開、既存PRの変更、merge、Issue closeは本設計登録に含めない。

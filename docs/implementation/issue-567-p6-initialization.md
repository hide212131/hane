# Issue #567 P6: EditorView初期化の機械的分離

## 目的・基準と今回の状態

Refs #567（RF2-A）、親 #297。P5のPR #635はmerge済み（`5676eb901d6d66bd658e0af2a4aa776d5a04d126`）。基準mainは `a50c44669d6a116dfbe13439bb258db39d9bf92b`。EditorViewを構築する一続きの処理をprivate moduleへ置き、初期化責務を見分けられる配置にする。

以下の準備状態・調査結果は設計時点の履歴である。当時は本書だけを追加する実装準備PRで、製品実装・test/build・GUIは未実施、実装依頼は未投稿だった。現在の実装・検証状態は末尾の同期記録とPR #638を参照する。着手時はcurrent main、PR head、Issue、関連PRと同じ対象のrunを取り直す。運用判断はdefault branchのCommander Policy、実装入口はClaude実行手順に従う。

調査時点のopen PRは0件。前回重なっていたproxy設定UI #633はmerge済みであり、現在の `network_settings` / `network_applier` の初期化もそのまま維持する。RF0全体を一律に待たず、#567の機械分割を先行する。Jev `jev-1.14.0` はcache/geometry共有helper、I/O意味整理、契約文書照合との比較で `initialization` を選択（probability 0.93、confidence 0.91）。

## 移動仕様

| 旧配置・シンボル | 新配置 | 可視性・呼出元 |
| --- | --- | --- |
| `crates/ui/src/view.rs::EditorView::new` | `crates/ui/src/view/initialization.rs` | `pub`維持。appと既存tests等の呼出し不変 |
| `crates/ui/src/view.rs::EditorView::from_sessions` | 同上、newの直後 | privateから `pub(super)` だけ変更。親のopenと親・兄弟の既存testsから使うため |

新moduleは `use super::*;` と `impl EditorView { ... }` で包む。親にprivateな `mod initialization;` を一つ追加する。再export・public module・trait・constructor wrapperは追加しない。

基準sourceは1671〜1944行（274行）。開始は `pub fn new(text:`、終了は ``/// Opens `path` as the first editor session.`` の直前。移動範囲はUTF-8で13,035 bytes、SHA-256 `97abca9a0daee050ee7029f86743560facda6f2c32a0dcd10e098233a82a9ae4`。行番号だけでなくシンボル境界を確認し、`open` のdoc commentを誤って移さない。

本文・signature・コメント・属性・cfg・field初期化の順序をそのまま移す。可視性調整は `from_sessions` の1件だけ。移動元の既存format差（例: `ai_selection:ai_selection::...`）を大量整形へ広げない。rustfmtが移動行の空白だけを変える場合は差分を明示し、識別子・式・処理順の変更と区別する。

## 維持する所有と契約

EditorViewの型・全field宣言・所有者は親のまま。新しい状態所有者を作らず、構築式を同じEditorViewの別implへ移す。親importは子moduleのglob経由利用も含め維持する。子moduleは祖先のprivate itemを参照できるため、親型・helper・fieldのpub化をしない。

`open`、`open_work_folder`、`begin_work_folder_scan`、`finish_work_folder_scan`、session accessor、`attach_ai_service`、非同期要求と結果受理、設定適用、render、既存testsは元の場所に残す。全callerを維持し、`Self::new` / `Self::from_sessions` / `EditorView::new` の呼出し方を変えない。タイマーやhookを呼出元へ分散しない。

- `new` はSessionSet::with_untitled、OsFileService、StateStores::from_environmentを同じ順に渡す。`from_sessions` は渡されたsessions/files/storesをそのまま使い、testの注入境界を維持する。I/Oや環境参照を増やさない。
- component初期化、DEFAULT_THEME、settings/recent読込、active文書行数に基づく初期HeightIndexを同じ順序で生成する。settingsをloadすることとthemeを後で適用することを統合しない。
- `on_app_quit` は未完draft debounceをflushする。`_quit_subscription` に保持する所有と寿命、futureの戻り値、hookの登録順を維持する。
- `on_release` はMermaidStoreのrelease_allとdrop_imageを同じ順に行う。reservationを含む解放処理、`_mermaid_release_subscription` による保持を変えない。
- keyboard-layout listenerはweak entityをcaptureし、既存のrefresh処理を呼ぶ。pollingへ置換せず、入力モードの初期値とsubscription保持を維持する。
- Escape interceptorはbindingの前にfind/AI入力のIME状態を観測する。Enter interceptorはrename pendingを先に読み、共有Rc<Cell<bool>>のarm/resetとEnter時の例外を保持してからfind/AI hookを呼ぶ。captureとclone、read/update順、early return、エラーの扱いを変えない。
- date badge taskは既存intervalでlocal dateを再観測し、view更新に失敗したらloopを止める。初期local_today、task保持、spawnとtimerの位置を維持する。
- settings/AI/network/update、external openのguard/generation、draft/title sync、folder watch、focus、scroll/zoom/anchor、height/cache/parse job、Mermaid、計測を含む全fieldの値・順序を維持する。`#[cfg(test)]` とinstrument/timing-probeの条件を保つ。Subscription/Taskを局所変数のままdropしたりdetachしたりしない。

初期値の整理、既存bug修正、constructor統合/分割、初期化の遅延化、依存注入API変更、hookの順序変更、型/field移動、公開面縮小、async I/O整理、機能追加・最適化は対象外。

## 検証・受入条件

実装後は対象head/base contextと結果URLをPRに残す。workerの権限を緩めず、trusted verification/CIとCommanderの実行を区別する。

1. 新moduleのwrapper/importと `from_sessions` の `pub(super)` を戻して旧範囲と比較する。基本はbyte一致とし、必要な移動行のformat差だけを列挙する。親の差分はmodule宣言追加と旧範囲撤去のみ。全field初期値・hook登録/capture・cfg・caller・test本文/名前/属性/入力/期待値を照合する。
2. 通常の `cargo test --workspace --locked` と `cargo test --workspace --locked --all-features`、`cargo clippy --workspace --locked --all-targets --all-features -- -D warnings`。macOS/Windowsのcurrent-head required CIを確認する。
3. 初期化のfeature分岐が対象のため `cargo check --locked -p hane-ui --features timing-probe` も確認する。通常/all-features成功をtiming-probe単独の結果へ読み替えない。既知baseline失敗が出たら基準mainと比較し、無関係な修正を混ぜない。
4. repository指定toolchainによる通常 `cargo build --release --locked -p hane` を確認する。fmt/diff検査は既存format差と分ける。安定候補のCI成功後にPolicy所定のcurrent-head最終reviewと、次表のfocused native GUIを実施する。

既存testは移動・削減しない。代表的な回帰確認は次のとおり。新規testで移動を写経せず、実装で意味差を入れないことを比較で確認する。

| 境界 | 既存test・位置 |
| --- | --- |
| layout listener | view.rs: `caret_mode_badge_updates_through_the_real_keyboard_layout_listener` |
| rename/IME | view.rs: `inline_rename_escape_cancels_ime_composition_before_the_rename`、`inline_rename_enter_commits_ime_composition_before_the_rename` |
| find interceptor | view/document_find.rs: `escape_during_find_input_composition_only_cancels_the_composition`、`find_input_composing_enter_does_not_navigate_but_committed_enter_does` |
| AI interceptor | view/ai_selection.rs: `enter_while_composing_in_the_free_text_input_does_not_submit`、`escape_that_cancels_the_ai_inputs_composition_does_not_leak_into_the_bodys_composition_cancel_or_after_input` |
| initial height / injected state | view.rs: `initial_line_heights_become_block_heights_when_parse_publishes_after_caret_move` と `from_sessions` を使う既存I/O/draft/session tests |
| date badge | view.rs: `apply_sidebar_date_badge_today_notifies_only_when_the_date_actually_changes`。これはtimerの実時間成功を単独で保証しない |

| GUI | macOS / Windowsでのfocused scenario |
| --- | --- |
| G1 起動・注入境界の実経路 | 隔離stateで空文書、既存日本語文書、work folderから起動。初期表示/focus、設定・recent、network設定の表示、編集と保存/再読込を確認する。実network認証や通信を追加で行う必要はない |
| G2 keyboard/IME hook | 本文の日本語IME・入力モード切替、find欄とAI指示欄のIME中Enter/Escapeを確認。合成確定/取消しが先行し、検索移動・AI送信・本文編集が同じキーで誤発火しない。AIは送信しない範囲で確認する |
| G3 pending rename guard | 隔離work folderでrename時のEnter/Escapeと続く本文入力を確認し、遅いIME文字がrename前後の本文へ漏れないことを観測する。制御可能なpending/late callbackは既存testsで併せて確認する |
| G4 quit/release | 隔離文書でdebounce内の未保存入力後に通常終了し再起動してdraft復旧を確認。Mermaid表示後に対象windowを閉じ、クラッシュがなく対象appが終了したことを確認。画像解放量や深夜timer成功を画面だけから断定しない |

GUIのhead/base、OS/toolchain、候補Release hash、fixture/期待値/実結果、実PID/実行パス、隔離stateを記録する。GUI所有と排他を確認し、Windowsは接続Windows環境で直接操作する。失敗は `docs/gui-test-recovery.md` に従い復旧する。旧P5のGUI Unknown受入はP6へ適用せず、未実施/unknownをPassへ変更しない。

機械移動であり性能改善を主張しない。追加の性能最適化・所有変更が必要なら本P6から分け、対象領域の変更前測定と契約を先に確認する。

## 後続と復旧

初期化の記述位置だけを分け、全field所有は親へ維持する。非同期I/O要求/受理の#309、H1の#308、cache/height寿命・geometryの#368/#307は独立した意味整理として残す。残る共有helperやtests配置も#567の棚卸しから消さず、親維持/移動/別担当を根拠付きで対応させる。

実装受入時に `docs/refactor-execution-plan.md` の§9.13配下へ移動・維持理由・検証を短く追記する。P5記録が未反映ならPR #635の実結果を参照して履歴を補い、未検証を成功へ書き換えない。rollbackはP6の機械移動PRを一単位でrevertし、永続データ移行は不要とする。

P6だけで#567/#297をcloseしない。次のRF Issue/フェーズを選定するのは本P6 PRがmergeされてからとする。


## 2026-10-27 main同期記録（P6の受入前）

対象はPR #638、branch `codex/issue-567-p6-initialization`。同期前headは `647c48255ed66608e0456747b9c04ad424016c1f`、旧baseは `a50c44669d6a116dfbe13439bb258db39d9bf92b`、今回取り込むmainは `497419203834f2a7aca413d1cb7758b37eb9dff6`。利用者の依頼に従い、製品修正と分離した通常mergeでmainのancestryを取り込む。merge-treeのpreviewと実mergeはいずれも競合なし。mainの運用文書、Issue #567、PR #638と実行中runを確認し、対象head一致を確認してから実行した。

mainの追加差分は以下の4ファイルだけであり、Rust source、Cargo設定・依存、assets、GUI validatorの実装には変更がない。

- `.github/scripts/rust_ci_path_filter.py`
- `.github/tests/test_rust_ci_path_filter.py`
- `.github/tests/test_windows_gui_lock_ci_wiring.py`
- `.github/workflows/ci.yml`

P6の実装は `new` / `from_sessions` のprivate moduleへの機械移動として維持する。同期前後の製品treeの一致と、旧baseに対するP6のRust差分と同期mainに対するRust差分の一致を確認する。追加変更はこの状態記録だけで、製品コード・既存testsには加えない。先行する実装・検証記録はPRの履歴として保持する。

旧headのCI・reviewは同期後headの証拠として扱わない。push後の新headに対する通常CIを別途確認し、結果URLとexact headをPRへ記録する。最終reviewは新headでは未取得とする。通常CIを超える追加の重いbuild/testは行わない。

macOSで既に得たfocused GUIの観測は、Rust source head `74095a005be73796428e3387ff7205b2b9bbb3d5` の通常Release候補（SHA-256 `88e938e8a4ba8e93fcf89a45b3da51d1dec72390bfe2ecf48548dfcf80386f88`）に結び付く。同期前headまでの追加変更は文書だけで、今回のmain差分もCI workflow/testだけである。製品treeが同一で、起動・IME・rename・quit/releaseの実行経路および候補生成入力に影響しないため、既存の観測範囲に限り保持する。新headでGUIを再実行したとは扱わない。

macOSのG2 AI入力IMEとG4通常終了/draft復旧/Mermaid release、およびWindows G1〜G4はUnknownのまま。Windows実機はcanonical lockのownerを直接照合できておらず、専用CIの成功を実機受入の代用にしない。この同期担当はWindows desktop、canonical lock、candidate、buildへ触れない。GUI確認までDraftを維持する既存の利用者判断を守り、受入・merge・Issue close・releaseは行わない。

Jev（要求 `jev-latest`、provider `typesafe`、応答 `jev-1.14.0`）は `sync_main` を選択（probability 0.86、confidence 0.72、入力873/output34 tokens）。Commanderはsame-repository/open/Draft、exact head、current mainと競合なしを確認して採用した。

# RF2-A / Issue #301 第4実装PR: インラインrenameの機械的分離

状態: 実装引き継ぎ用設計。製品コードの変更・テスト実行・GUI検証の完了報告ではない。

作成日: 2026-09-28（日本時間）  
対象: [Issue #301](https://github.com/hide212131/hane/issues/301) / 親 [#297](https://github.com/hide212131/hane/issues/297) / 台帳 RF0-05  
コード調査・設計ブランチの基準: `db7c5f4121be810addbd509a60415d2833284bf8`

## 1. 次に実施する作業と選定理由

次の作業は **#301 の第4実装PRとして、インラインrename専用の36メソッドを `crates/ui/src/view/inline_rename.rs` に機械的に移すこと** とする。新しいrename機能、入力モデル、非同期実行基盤は作らない。

調査時点で #301 はopen。第1PR [#387](https://github.com/hide212131/hane/pull/387) のsidebar表示、第2PR [#388](https://github.com/hide212131/hane/pull/388) のfilter入力、第3PR [#390](https://github.com/hide212131/hane/pull/390) の通常scroll/zoomはmainに反映されている。#390のmerge commitは `d1e35016bd8dfc3d72256ef786ff747a0e105c3c`。現在の `view` の子moduleは `sidebar`、`sidebar_filter`、`viewport` であり、rename専用メソッドはまだ親にある。[コード基準][view]

#390はホイールスクロール慣性 [#389](https://github.com/hide212131/hane/issues/389) のために先行した分離である。元の #301 設計にあったrename分離が未実施のため、ここへ戻る。#389そのものは機能Issueであり、今回の「refactorで始まるIssueの次作業」には含めない。

[RF1-B #300](https://github.com/hide212131/hane/issues/300) は [#386](https://github.com/hide212131/hane/pull/386) で完了し、使用中API・fallback・test helperを維持する整理が済んでいる。その調査をやり直したり、削除量を増やすためにAPIを消したりしない。RF0全体の未完と、この狭い移動に必要なRF0-05の引き渡しを区別する。[実行計画][execution]

#302/#303のpresentation/Markdown分離は、対応領域の前提が揃えば独立に進められるが、今回の一件には混ぜない。renameの境界をfilterと並べて読めるようにする方が、進行中の #301 を進め、後続 #304/#305 の入力責務整理にもつながる。#308/#309の保存・命名状態の移譲はまだ行わない。

### 着手時に再取得する事実

実装者は現在のmain SHA、#301本文・最新コメント、重なるopen PR、実行計画のRF0-05と9.7〜9.10節を再取得する。SHAが本書から進んでいたら、シンボルと呼出元の差分を確認し、実装PRに実際のbaseを記録する。古い行番号で切り取らない。

本書作成時のopen PRには、AI設定の設計文書 [#392](https://github.com/hide212131/hane/pull/392) と、releaseのlock/version整合修正 [#393](https://github.com/hide212131/hane/pull/393) がある。#393が扱う `--locked` の失敗をrename起因と誤認せず、反対に古い不具合として検証を免除もしない。実装検証時にはその受入状況とcurrent mainを確認する。Cargo.lock、Cargo.toml、version policy、CI修正を本PRへ取り込んで修正したり、`--locked` を外して通過扱いにしたりしない。無関係なrelease作業の完了を設計・配置作業全体の待ち条件にはしない。

## 2. 変更範囲を固定する

### 製品側の変更ファイル

| パス | 変更内容 |
| --- | --- |
| `crates/ui/src/view.rs` | `mod inline_rename;` を追加し、§3のメソッド定義だけを除去する。共有型・field・初期化・共有helper・共有dispatch・既存テストは残す。 |
| `crates/ui/src/view/inline_rename.rs` | 新規。`use super::*;` と `impl EditorView` に、元のメソッドを元の相対順で置く。 |
| `docs/refactor-execution-plan.md` | 実装PRで、実際の旧→新対応・可視性・head/base・検証・引き渡しを次の空いている節へ追記する。設計時点で実施済みと記録しない。 |

この設計文書自体の修正が必要になった場合は、設計との差を明示して更新する。それ以外の製品ファイルの変更は、移動に必要とする理由を説明する前に追加しない。

`lib.rs`、`actions.rs`、`input.rs`、`capture.rs`、`line.rs`、`shape.rs`、`view/sidebar.rs`、`view/sidebar_filter.rs`、`view/viewport.rs`、session crate、Cargo manifests/lock、workflowは変更しない設計である。新たな依存、feature、trait、Manager、状態機械、永続形式、互換wrapperは不要。

### 配置の骨格

```rust
// crates/ui/src/view.rs
mod inline_rename;
mod sidebar;
mod sidebar_filter;
mod viewport;
// 既存の定義・初期化・共有処理を維持する。
```

```rust
// crates/ui/src/view/inline_rename.rs
use super::*;

impl EditorView {
    // §3の36メソッドを元の実装のまま移す。
    // bodyの再実装、条件式の整理、共通化はしない。
}
```

既存の子moduleと同じ構成を使う。`EditorView` は一つのままであり、子moduleが別の状態所有者になるわけではない。親のprivate fieldを子から使用し、fieldを公開する必要を作らない。親のimportは他の子moduleからも使われるため、親ファイル内の検索だけで「未使用」として消さない。

## 3. 移動するメソッドの完全一覧

旧パスはすべて `crates/ui/src/view.rs`、新パスはすべて `crates/ui/src/view/inline_rename.rs`。関数名・引数・戻り値・receiver・属性・コメント・bodyは維持し、下表の2件以外は可視性も維持する。

| No. | メソッド | 移動後の可視性 |
| --- | --- | --- |
| 1 | `inline_rename_active` | `pub(crate)` 維持 |
| 2 | `inline_rename_render_state` | `pub(crate)` 維持 |
| 3 | `set_inline_rename_input_bounds` | `pub(crate)` 維持 |
| 4 | `inline_rename_character_index_for_point` | `pub(crate)` 維持 |
| 5 | `move_inline_rename_to_point` | `pub(crate)` 維持 |
| 6 | `inline_rename_has_composition` | `pub(crate)` 維持 |
| 7 | `inline_rename_text_for_range` | `pub(crate)` 維持 |
| 8 | `inline_rename_selection` | `pub(crate)` 維持 |
| 9 | `inline_rename_marked_range` | `pub(crate)` 維持 |
| 10 | `replace_inline_rename_text` | `pub(crate)` 維持 |
| 11 | `replace_and_mark_inline_rename_text` | `pub(crate)` 維持 |
| 12 | `commit_inline_rename_composition` | `pub(crate)` 維持 |
| 13 | `cancel_inline_rename_composition` | `pub(crate)` 維持 |
| 14 | `selected_inline_rename_text` | `pub(crate)` 維持 |
| 15 | `move_inline_rename_left` | `pub(crate)` 維持 |
| 16 | `move_inline_rename_right` | `pub(crate)` 維持 |
| 17 | `move_inline_rename_horizontal` | private維持 |
| 18 | `select_inline_rename_left` | `pub(crate)` 維持 |
| 19 | `select_inline_rename_right` | `pub(crate)` 維持 |
| 20 | `select_all_inline_rename` | `pub(crate)` 維持 |
| 21 | `select_inline_rename_home` | `pub(crate)` 維持 |
| 22 | `select_inline_rename_end` | `pub(crate)` 維持 |
| 23 | `move_inline_rename_home` | `pub(crate)` 維持 |
| 24 | `move_inline_rename_end` | `pub(crate)` 維持 |
| 25 | `move_inline_rename_to` | private維持 |
| 26 | `backspace_inline_rename` | `pub(crate)` 維持 |
| 27 | `delete_inline_rename` | `pub(crate)` 維持 |
| 28 | `delete_inline_rename_with_direction` | private維持 |
| 29 | `begin_inline_rename_from_selection` | `pub(crate)` 維持 |
| 30 | `begin_inline_rename` | private → `pub(super)` |
| 31 | `inline_rename_has_background_conflict` | private → `pub(super)` |
| 32 | `reserve_inline_rename_tickets` | private維持 |
| 33 | `confirm_inline_rename` | `pub(crate)` 維持 |
| 34 | `finish_inline_rename` | private維持 |
| 35 | `follow_inline_rename_paths` | private維持 |
| 36 | `cancel_inline_rename` | `pub(crate)` 維持 |

集計: 既存 `pub(crate)` 28件、`pub(super)` へ変更2件、private維持6件。

### 可視性変更の理由

`begin_inline_rename` は、すでに分離された兄弟module `view/sidebar.rs` のダブルクリック処理から呼ばれる。`inline_rename_has_background_conflict` は、親 `view.rs` の既存テストから直接呼ばれる。子moduleへ移すと両者には `pub(super)` が必要である。外部crateへの公開はしない。

`finish_inline_rename` は同じmodule内の `confirm_inline_rename` の完了closureから呼ぶ。`reserve_inline_rename_tickets`、`follow_inline_rename_paths`、文字編集の内部メソッドも同一module内の接続を維持する。移動後に将来使うかもしれないという理由で `pub(super)` / `pub(crate)` に広げない。実装着手時に利用者が増えていた場合は、その具体的な呼出元を示して表を更新する。

## 4. 親・既存moduleに残すもの

### 型、状態、初期化

次は `view.rs` に残す。

- `InlineRenameKind`、`InlineRename`、`InlineRenameComposition`、`InlineRenameRenderState`、`SidebarFilterComposition`。
- `EditorView` 本体と全field。特に `inline_rename`、`inline_rename_input_bounds`、sidebar/filter/focus状態、sessions、files、title-sync各map/set、loading path、work-folder generation。
- `new` / `from_sessions`、初期値、work-folder切替時のreset、subscriptionとtaskの生成・保持・破棄。
- `text_input_render_state` と `set_text_input_bounds`。renameとfilterの共有dispatchであり、名前の近さだけでrename moduleへ移さない。
- 親の既存 `mod tests` とfixture。テスト名、assert、属性、待ち条件、件数、配置は今回変えない。

`InlineRenameRenderState` は `input.rs` が `crate::view::InlineRenameRenderState` として利用している。これを子へ移してre-exportやfield公開を増やす必要はない。`sidebar.rs` もrenameの `from` / `fixed_extension` を読むため、型・fieldを親に残す方が機械的変更を小さく保てる。[入力実装][input] [sidebar実装][sidebar]

初回の #301 設計コメントで列挙された `InlineRename*` の移動候補は、本PRではこの境界に具体化する。型まで一緒に動かすことを本PRの完了条件にはしない。残す位置と理由は #301 の最終受入時にも追跡できるよう実行計画へ記録する。

### 純粋helper

次はすべて親に残し、bodyも名前も変えない。

```text
inline_rename_parts
valid_inline_rename_name
rebase_ui_path
byte_offset_from_utf16
utf16_offset_from_byte
byte_range_from_utf16
inline_rename_selected_range
range_to_utf16
previous_inline_rename_boundary
next_inline_rename_boundary
inline_rename_cursor
select_inline_rename_to
select_inline_rename_to_fields
```

UTF-16変換、grapheme境界、選択範囲のhelperはfilterも使用する。特に `inline_rename_selected_range` と `select_inline_rename_to_fields` は名前にrenameを含んでいてもrename専用ではない。rename専用の小さなhelperも、親に残す型・既存テストと一緒に据え置く。このPRでhelper群の再設計や汎用的な入力欄モデル化まで始めない。[filter実装][filter]

`inline_rename_label` はすでに `view/sidebar.rs` にある描画組立であり、ここには移さない。`shape_inline_rename_line` / `InlineRenameInput` / native入力handlerは `input.rs` に残す。本文の `InputCapture` も変更しない。[capture実装][capture]

## 5. 呼出関係と所有者

```text
既存のactions.rs ────┐
既存のinput.rs ──────┤ 同じEditorViewの同じメソッド名を呼ぶ
既存のcapture.rs ────┤
既存のview/sidebar.rs┤
view.rsの共有dispatch┘
                    ↓
view/inline_rename.rs の impl EditorView
  ├─ 親view.rsの状態・型・helperを利用
  ├─ 既存FileServiceへbackground jobを依頼
  └─ 同じ完了closureからfinish_inline_renameへ接続
                    ↓
既存のSessionSet / DocumentSession / WorkFolder / RecentFiles
親のsave_session / retry_deferred_title_sync
```

| 責務 | 今回の実行コード配置 | 状態・契約の所有者 |
| --- | --- | --- |
| renameの文字編集・IME・開始/確定/取消 | 新しい `view/inline_rename.rs` | 同じ `EditorView` と親に残す `InlineRename` |
| 入力先選択・キー優先順 | `actions.rs` / `input.rs` / 親の共有dispatchを維持 | 現行経路のまま。集約は #304 |
| filterとの共通文字編集 | 既存helperを親に維持 | 共通モデル化は #305 |
| rename ticketと保存直列化 | 既存session APIを呼ぶ | `DocumentSession` / `SessionSet` を維持 |
| ファイルシステム操作 | 既存background executor + FileService | 既存 `FileService` を維持 |
| H1同期の状態と保存再開 | 親に残る既存処理を呼ぶ | 所有権整理は #308/#309 |
| 描画、focus handle、候補位置のshape | 既存sidebar/inputを維持 | 既存EditorView/入力Elementを維持 |

ファイルを分けたことを、状態所有権の整理が完了した証拠にはしない。

## 6. 維持する挙動と処理順

以下は移動前の実装を比較するための要点である。新しいアルゴリズムや改修指示ではない。現在のコードを丸ごと移すことが基本であり、この説明だけから書き直さない。

### 6.1 入力・IME・selection

範囲置換は、明示されたUTF-16範囲、marked range、selected rangeの現在の優先順を維持する。入力のCR/LF除去、byte/UTF-16変換、grapheme単位の左右移動、選択方向を変えない。preedit開始時の元text/selection/directionの保存、preedit内の相対UTF-16選択、commit時のmark解除、cancel時の元状態復元を維持する。

renameの `replace_inline_rename_text` / `replace_and_mark_inline_rename_text` はpending時に文字列を更新しなくても `true` を返す。これを `false` にして別の入力経路へ流れる契機を作らない。pointer移動にあるpending/compositionのguardも維持する。

**pendingのguardを全メソッドへ機械的に追加してはいけない。** 例えば基準コードの `select_inline_rename_left` / `select_inline_rename_right` には、他の編集メソッドと同じpending検査がない。今回の契約は「全選択操作がpending中に禁止される」ではなく、各入口の現在のguard・呼出順を変えないことである。問題を発見した場合は、不具合の再現と修正を配置変更から分ける。

`actions.rs` のEnter/ShiftEnterは、renameのcompositionがあれば先にcompositionを確定し、compositionがなければrenameを確定する。Escapeもcompositionの取消とrenameの取消を区別する。settingsやtab context menuの優先処理も維持する。

キーactionにはfilterを先に判定する経路があり、native入力handlerにはrenameを先に判定する経路がある。今回、双方の順序を新しい共通ルールに書き換えない。#304へ現状として引き渡し、入力先判定の統合と動作表の確定は別PRで行う。[actions実装][actions] [入力実装][input]

### 6.2 開始と名前

F2からの開始はsidebar keyboard focus、filter focus、既存renameの有無を検査する現在の入口を維持する。sidebarのダブルクリックからの直接開始も、既存event closureのまま接続する。

ファイルのMarkdown拡張子は編集文字列から分離し、元の大文字・小文字を含む拡張子を保持する。folder名は同じ分離を行わない。非UTF-8名はlossy変換せず現在どおり拒否する。空文字、`.`、`..`、区切り文字の検査と、入力開始時の全文選択・focus・通知を維持する。

### 6.3 競合検査とticket予約

保存中session、H1同期のpending/in-flight/scheduled、対象配下のdraftや新規folder作成、loading pathを区別する。fileは対象path一致、folderは `rebase_ui_path` で判定する現在の包含関係を維持し、文字列のprefix比較に置換しない。

`reserve_inline_rename_tickets` は、影響sessionを列挙し、保存中の有無を確認してからticketを取得する。途中の予約失敗では取得済みticketを返す現在の処理を維持する。SessionIdとSaveTicketを新しいIDに統合しない。

### 6.4 確定・非同期処理

`confirm_inline_rename` の順序を維持する。

1. rename不在/pendingを検査し、from/kind/name/extensionを取得する。
2. 名前、loading/背景競合、親pathを検査し、extensionを付けたtargetを作る。
3. 同じ名前なら現在どおり編集状態を解除して戻る。この分岐に新たな保存・H1再試行を追加しない。
4. ticketを予約し、pending/statusを更新する。
5. FileServiceと操作pathのcloneを、既存の二段のspawnへ渡す。
6. `rename` / `rename_folder` の結果を、既存の `view.update` closureから `finish_inline_rename` へ渡す。
7. `.detach()` と通知を現在の位置のまま保持する。

closureが捕捉するfrom/target/kind/tickets、weak view経由のupdate、background executor、taskの所有・寿命を変えない。新しい取消token、generation検査、runtime、async traitを追加しない。基準のこの完了経路に存在しないgeneration照合を「すでに保証している」と説明してはいけない。現行のnavigation/cancel guardとticket契約を維持する。

### 6.5 結果受理の順序

`finish_inline_rename` は次の順を維持する。

1. ticketを解放し、各sessionのpending saveをローカルのqueueへ取り出す。
2. I/O成功時は、file/folder別にsessions・work-folder・recentを更新する。fileの `FileEventOutcome::Renamed` に対する自動命名解除も維持する。
3. 成功時はUI内pathを追従させ、recentを保存し、statusを更新してinline renameを解除する。recent保存失敗とrenameのI/O失敗を混同しない。
4. I/O失敗時はpendingを解除し、入力文字列を残してエラーを表示する。
5. 成功・失敗のいずれも、その後でqueueに取り出した保存要求を `save_session` へ渡す。
6. `retry_deferred_title_sync` を呼び、通知する。

「先にUIを消してからticketを返す」「失敗時は保存再開を飛ばす」「recent保存失敗ならファイルrenameも失敗したことにする」といった整理を入れない。

`follow_inline_rename_paths` はfolderの場合のselected/expanded/pending folderとdraft保存先、両kindの場合のloading pathとlatest open targetを現在どおり更新する。無関係なpathを変更しない。

### 6.6 取消

rename不在なら `true`。pendingならstatusと通知を更新して `false` を返し、呼出元にnavigationを続行させない。編集中なら状態を解除し、deferred title-sync再試行、通知、`true` の順を維持する。OSのrename処理を新たに取消可能にする設計ではない。

## 7. 検証設計

### 7.1 機械的移動の証拠

各メソッドを同名で対応付け、36件が新moduleに一度だけ存在し、親には旧定義や移行wrapperが残らないことを確認する。bodyは移動前と比較し、許容差分はmodule宣言、implの囲み、必要なimport接続、§3の2件の可視性に限定する。

`git diff --color-moved=blocks` はレビュー補助として使うが、それだけを同等性証明にしない。シンボルごとのbody比較を行い、文字列literal・条件式・return値・clone・spawn・notifyの差がないことを記録する。比較用の一時scriptを使う場合は、文字列内部も含めた無条件な空白除去で意味差を隠さない。恒久的な検証基盤を新設する必要はない。

既存テストの一覧・名前・属性・assertを比較する。親の状態定義、初期化、共有dispatchと§2の非変更ファイルも差分ゼロで確認する。意図しないrustfmtの全域整形を混ぜない。

### 7.2 保護契約と既存テスト

次は既存テスト名として基準コードで確認したもの。今回実行済みという意味ではない。

| 契約 | 既存テスト |
| --- | --- |
| Markdown拡張子を編集範囲から分ける | `inline_rename_keeps_markdown_extensions_out_of_the_editable_text` |
| 非UTF-8名のlossy renameを防ぐ | `inline_rename_refuses_non_utf8_names_instead_of_replacing_bytes`（`cfg(unix)`） |
| 結合文字・ZWJ絵文字の移動境界 | `inline_rename_movement_uses_grapheme_boundaries` |
| 選択方向を保ったHome/End | `inline_rename_shift_home_and_end_extend_from_the_active_caret` |
| preedit内の相対選択 | `inline_rename_ime_selection_is_relative_to_the_marked_replacement` |
| Escapeのcomposition優先 | `inline_rename_escape_cancels_ime_composition_before_the_rename` |
| Enterのcomposition優先 | `inline_rename_enter_commits_ime_composition_before_the_rename` |
| 親directoryからの逸脱を防ぐ | `inline_rename_rejects_names_that_escape_the_parent_directory` |
| replacement-local UTF-16からbyteへの対応 | `inline_rename_ime_selection_is_relative_to_the_inserted_text` |

この9件だけで終えず、既存のrename・folder rename・autosave/title-sync競合・filter・settings・本文入力の関連テストとworkspace全体を維持する。特に下表を受入時の確認軸にする。

| 確認軸 | 変えてはいけないこと / 必要な証拠 |
| --- | --- |
| 入力の隔離 | rename/filterでの置換・削除・clipboard・IMEが本文source/selection/undoへ漏れない。既存入力経路のguardと関連回帰を確認。 |
| pending | 文字置換の消費、二重確定抑止、取消時false、navigation停止。各メソッドの実際のguardを比較し、未実装の一律禁止を仮定しない。 |
| file/folder成功 | セッションpath、tree、recent、folder配下のUI/draft保存先が追従し、本文bytes/dirty内容を失わない。 |
| 失敗と競合 | 衝突・I/O失敗時の編集状態とエラー、ticket解放、pending saveとH1再試行の順序。 |
| 非同期 | 既存の遅延・競合テストとclosureの捕捉値/寿命を維持。実OSの安全性や電源断耐性をテスト以上に主張しない。 |
| APIと配置 | 呼出元のコード変更なし、既存型path維持、36件の移動と2件だけの可視性変更。 |

不足する契約テストを発見した場合は、基準コードに対する契約追加として配置変更とは別の差分/PRで扱う。望ましくない既存挙動を新しい正解として固定しない。本変更の正しさを判断できない不足を単なるfollow-upへ送ってmergeしない。

### 7.3 実行するコマンド

実装環境の同じOS/toolchain/featuresでbaseとheadを比較し、コマンド・終了コード・SHA・ログを保存する。crate名は基準の `crates/ui/Cargo.toml` で `hane-ui` と確認している。

```sh
# 対象テストの一覧とfocused実行。これだけをworkspace成功の代わりにしない。
cargo test -p hane-ui --all-features --locked -- --list
cargo test -p hane-ui --all-features --locked rename
cargo test -p hane-ui --all-features --locked sidebar_file_filter

# 通常featureとall-featuresは別々に確認する。
cargo test --workspace --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

git diff --check
```

実装PRのmacOS/Windows CIをcurrent exact headで確認する。既存CIが実行するvendor/GPUIのcheckを無効化しない。起動配線、feature、依存、build条件を本設計どおり変えなければ、release機構や版変更を本PRの追加作業にしない。もしそれらに差分が必要になったらまず別責務として切り分ける。

#388には、通常featureの並列実行でAI側の `runtime_lifecycle` が不安定になり、直列実行では成功した記録がある。その結果を新しいPRのpassとして流用しない。今回も失敗した場合は同じbase/toolchainで比較し、直列成功と通常並列成功を区別して報告する。テスト削除、ignore追加、timeout延長、無関係なAI修正を混ぜない。

### 7.4 GUI・性能

本設計の条件を満たす純粋な移動で、呼出元、focus、native入力、描画、task、状態、テストに意味差がないことを示せれば、その根拠をもって新たなGUI検証を不要と判断できる。文書の「機械的移動」というラベルだけで免除しない。

focus/IME/描画/配線に意味変更が混ざった場合は、その変更を分離する。必要な変更として別PRで扱う場合には、macOS/Windowsの対象経路で、F2/ダブルクリック、IMEの確定/取消と候補位置、選択とclipboard、file/folder rename、失敗時復帰をfocused GUIで確認する。実IME未確認をGPUI unit testの成功で代替しない。

ファイル移動を性能改善として報告しない。アルゴリズム・処理回数・実行位置・task寿命を変えない本PRでは、起動/RSSの新規測定を無条件の前提にはしない。これらに実際の差分が必要になった場合は配置変更から分離し、#23の比較可能な基準で測定する。

## 8. 実装手順とPRの完成条件

### Step 0: current factsと基準を確保

current main、重なるPR、既存 #301 の引き渡しを確認する。未受入の他PRをこの作業のためだけにmergeしない。実装ブランチはその時点の受入済みmainから作る。候補名は `refactor/301-inline-rename`。本書のdocsブランチへ製品改修を混ぜない。

対象36件、残す共有2メソッド、型・helper・テスト、呼出元を再確認し、baseの検証結果を採取する。移動範囲の前提が変わっていれば、本書と実装PRに差分を記録する。

### Step 1: 一つの責務として移動

新moduleを宣言し、§3の36件を順番を保って移す。`text_input_render_state` と `set_text_input_bounds` は元のimplに残す。2件だけ `pub(super)` とし、元のbodyを残したwrapperや二重定義を作らない。型・field・共有helperは触らない。

### Step 2: 同等性と回帰を確認

シンボル対応とbody差分、テスト一覧、API/可視性、非変更ファイルを確認してからfocused/workspace testsとclippyを実行する。失敗を原因別に分類し、移動で生じたimport/可視性の問題だけをこのPRで修正する。挙動のバグ修正や共通化を便乗させない。

### Step 3: 記録して引き渡す

実行計画の次節に、実際のbase/head、36件の対応、2件の可視性理由、親に残したもの、コマンドと証拠、未実施と理由、rollback、後続境界を記録する。実装PRは `Refs #301` とし、`Closes #301` は使わない。

レビュー・GUI要否・mergeの運用判断は、実行時のdefault branchにある [Commander Policy](aadw-command-policy.md) と [AGENTS.md](../AGENTS.md) を参照する。本書へ別のAADWルールを作らない。古いPRのCI/reviewや、PRがmergedであるという状態だけを本PRの検証証拠にはしない。

### この実装PRの受入チェックリスト

- [ ] actual baseと対象headが明示され、重なる未受入変更が混在していない。
- [ ] 36メソッドの旧→新対応があり、body・signature・属性・処理順が維持されている。
- [ ] 可視性変更は、呼出元を示した2件の `pub(super)` に限定されている。
- [ ] 型・field・初期化・共有helper・共有dispatch・既存テスト・既存入力/描画呼出元が維持されている。
- [ ] 二重定義、委譲wrapper、汎用Manager、新しい状態所有者がない。
- [ ] focused tests、通常/all-features、clippy、current-head macOS/Windows CIの結果が記録され、必要な条件を満たしている。
- [ ] GUI/性能の適用範囲を実差分から判断し、必要とした検証を未実施のままpassにしていない。
- [ ] 実行計画に受入結果と後続への引き渡しがあり、#301全体を完了扱いにしていない。

## 9. 後続への引き渡し・戻し方

#304には、filter/renameの専用メソッド配置、残るactions/native/shared-dispatchの判定箇所、現在異なる優先順を引き渡す。#305には、親に残るUTF-16/grapheme/selection helperと、別々に所有するfilter/renameの文字状態を引き渡す。#308/#309には、同じownerに残したticket、H1同期、保存queue、結果受理の順序を引き渡す。

本PRが受け入れられても #301全体は未完である。session I/O、background parse、height/cache、viewportの残りのpointer/panel処理、settings、計測、renderなど、未分離領域を実行計画で追跡する。#390がscroll/zoomを移したことを、viewport全責務の完了に拡大解釈しない。後続Issueの着手可否は、関連境界の受入と契約確認で判定し、無関係な全フェーズ完了を待たせない。

rollbackはこの機械的移動の実装PRを一単位でrevertする。永続データ変換、設定変換、ファイル名の移行は発生しない。後続PRが新配置に依存する場合は依存を明記し、必要なら後続から逆順に戻す。設計文書の保存PRと、製品の移動PRは別単位で戻せる。

## 10. この設計作成時に確認したこと／していないこと

確認したもの: GitHub上のmain SHA、refactor Issue群と #301の本文/コメント、#300の状態、#387/#388/#390の分離範囲とmainの配置、open PR、基準の `view.rs`・sidebar/filter・actions/input/capture・UI Cargo.toml・実行計画・AGENTS。

実施していないもの: 本仕様に基づくRustコード変更、移動後のコンパイル・tests・clippy、実OS/GUI検証、性能測定、実装PRのレビュー・merge・Issue close。本書にある受入チェックは実装時の作業であり、過去の別PRの成功を転記したものではない。

## 調査したコードの固定参照

以下はすべて調査基準SHAへのリンクである。実装開始時のcurrent mainの再取得を省略するためのものではない。

[view]: https://github.com/hide212131/hane/blob/db7c5f4121be810addbd509a60415d2833284bf8/crates/ui/src/view.rs
[sidebar]: https://github.com/hide212131/hane/blob/db7c5f4121be810addbd509a60415d2833284bf8/crates/ui/src/view/sidebar.rs
[filter]: https://github.com/hide212131/hane/blob/db7c5f4121be810addbd509a60415d2833284bf8/crates/ui/src/view/sidebar_filter.rs
[actions]: https://github.com/hide212131/hane/blob/db7c5f4121be810addbd509a60415d2833284bf8/crates/ui/src/actions.rs
[input]: https://github.com/hide212131/hane/blob/db7c5f4121be810addbd509a60415d2833284bf8/crates/ui/src/input.rs
[capture]: https://github.com/hide212131/hane/blob/db7c5f4121be810addbd509a60415d2833284bf8/crates/ui/src/capture.rs
[execution]: https://github.com/hide212131/hane/blob/db7c5f4121be810addbd509a60415d2833284bf8/docs/refactor-execution-plan.md

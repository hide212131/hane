# ADR-0029: source-first list editing

## ステータス

実装済み（第1段階）

source-first の planner / Editor / UI 配線と自動テストを実装済み。実アプリでの GUI validation と、
projection の再利用キャッシュ最適化は後続作業として残す。

実装時点の検証として、`cargo test --workspace --all-targets -- --test-threads=1`（全テスト成功）と、
対象 crate の `cargo clippy --all-targets -- -D warnings`（警告なし）を確認済み。GUI validation は未実施である。

## 日付

2026-09-22

## 関連

- [Issue #220](https://github.com/hide212131/hane/issues/220)
- [Pull Request #221](https://github.com/hide212131/hane/pull/221)
- ADR-0005: Revision とバックグラウンド処理
- ADR-0007: IME Composition モデル
- ADR-0013: Undo/Redo transaction
- ADR-0018: revision 付き Block Index
- ADR-0021: LayoutLine と visual 座標系
- ADR-0025: 複数行 inline presentation の shared parse

## 背景

### 現在の source の扱い

Hane の Markdown source は hane_document::RopeBuffer が唯一の Model である。BlockIndex は source の
ブロック境界と revision を持ち、hane-presentation は source を解析した結果を VisualBlock / layout に
変換する。この境界は維持する。

### Issue #220 / PR #221 の現在実装

Issue #220 は「通常リストの末尾で Enter した直後に sibling / child を入力できない」問題を扱い、PR #221
で以下の方式が導入された。

- Enter は source に改行だけを挿入する。
- 空の次行に ListEditingContext と ListCaretOrigin を保持する。
- apply_list_editing_context が presentation / layout に一時的な list owner と caret origin を付与する。
- 次の文字入力時に pending_list_indentation を IME prefix として source へ挿入する。
- BlockIndex の formal list_projections は編集時に clear される。

この方式は #220 の当時の受け入れ条件（Enter 直後は source の list structure を確定しない）には合っているが、
次の設計上の負債を残す。

1. source に存在しない「次の list item」を presentation と UI が一時的に作る。
2. Enter 後の文字入力と IME commit が、通常の source edit と異なる prefix 注入経路になる。
3. Enter 直後の Tab / Shift+Tab が formal projection の到着タイミングに依存する。
4. undo、cancel、selection、cache invalidation が transient context の寿命を考慮しなければならない。
5. View が Markdown の list owner、indentation、caret origin を保持するため、source-first の依存方向が崩れる。

### 今回の変更が置き換えるもの

本 ADR は #220 の次の判断を明示的に supersede する。

> Enter 直後は source に list structure を確定しない。

今後は、Markdown の文書構造を変えるキー操作は source を一つの recorded edit として即時に変更する。
View は変更後 source を通常の parser / presentation / layout 経路で表示する。

## 決定

### 1. source-first の編集パイプライン

キー入力は次の順序で処理する。

    GPUI key action
        ↓
    EditorView: selection / IME / 編集対象の選択
        ↓
    hane-markdown: current source に対する MarkdownEditPlan の生成
        ↓
    hane-editor: Markdown を知らない range replacement を recorded transaction として適用
        ↓
    RopeBuffer: revision と RevisionDelta を更新
        ↓
    BlockIndexState::apply_edits: 同期的な局所更新
        ↓
    presentation / layout: current source を再解析して投影
        ↓
    View

hane-editor に IndentList、NewListItem などの Markdown 固有 command は追加しない。Markdown 層が
返すのは「source の range を replacement bytes で置き換える」という計画だけである。

カーソル、selection、IME composition、scroll、cache は Model ではないため一時状態として保持してよい。
ただし「この空行は list item である」「この行は何階層である」という文書意味を View だけで補ってはならない。

### 2. 編集対象と第1実装の範囲

第1実装の list-aware 操作は、selection が caret のみである場合に限定する。

- Enter
- Shift+Enter
- Tab
- Shift+Tab
- 空 list item に対する後退削除（Windows の Backspace、macOS の Delete）

複数 selection に対する Tab / Shift+Tab は第1実装では扱わない。selection が空でない場合は、Enter / Backspace
など既存の通常 text command へ戻す。Tab / Shift+Tab は no-op とし、selection の一部だけを構造変更しない。

壊れた indentation や parser が ownership を確定できない source では list-aware plan を作らない。例外として、
入力直後の末尾空 item（`- `、`  - `）だけは ListEditProjection が opening line を局所的に lexically recover
する。これは表示用 AST を書き換える推測ではなく、次の source edit を成立させるための編集専用 fallback であり、
通常の source bytes を失わない。

### 3. 公開する Markdown editing API

crates/markdown/src/list_editing.rs を追加し、crates/markdown/src/lib.rs から編集用の型と planner を
re-export する。型は次の概念 API にする。実装時に既存の SourceOffset / SourceRange / Revision の
型名と命名規則へ合わせてよいが、責務は変えない。

    pub enum ListEditIntent {
        Enter,
        ShiftEnter,
        Indent,
        Outdent,
        Backspace,
    }

    pub struct SourceSelection {
        pub anchor: SourceOffset,
        pub active: SourceOffset,
    }

    pub enum MarkdownEditPlan {
        Replace {
            range: SourceRange,
            replacement: String,
            selection_after: SourceSelection,
        },
        NoOp {
            selection_after: SourceSelection,
        },
    }

    pub enum ListEditPlanResult {
        Handled(MarkdownEditPlan),
        NotApplicable,
    }

    pub fn plan_list_edit(
        document: &RopeBuffer,
        projection: &ListEditProjection,
        selection: SourceSelection,
        intent: ListEditIntent,
    ) -> Result<ListEditPlanResult, BufferError>;

Handled(NoOp) は Tab が first sibling だった場合のようにキーを消費する。NotApplicable は通常の
Enter / Backspace へ fallback できることを示す。

MarkdownEditPlan は source range、replacement bytes、編集後 selection だけを持つ。pixel、visual offset、
ListId、VisualBlock、GPUI の型、presentation の caret origin は含めない。

### 4. ListEditProjection は編集専用の同期 projection とする

既存の ListProjection は document-wide formal parse から作る表示用の render-neutral projection であり、
list numbering、marker disclosure、viewport projection に使う。これを編集 planner の唯一の入力に流用しない。

ListEditProjection は current source revision に対する編集専用の構造情報である。最低限、次を source byte
range で保持する。

    pub struct ListEditProjection {
        pub revision: Revision,
        pub block_id: BlockId,
        pub source_range: SourceRange,
        pub items: Vec<ListEditItem>,
    }

    pub struct ListEditItem {
        pub item_range: SourceRange,
        pub opening_line: SourceRange,
        pub marker_range: SourceRange,
        pub prefix_range: SourceRange,
        pub body_range: SourceRange,
        pub subtree_range: SourceRange,
        pub list_range: SourceRange,
        pub parent_item: Option<SourceRange>,
        pub previous_sibling: Option<SourceRange>,
        pub depth: usize,
        pub ordinal: usize,
        pub list_start: Option<u64>,
        pub marker: ListMarkerSource,
        pub task: Option<bool>,
        pub marker_indent_columns: usize,
        pub body_indent_columns: usize,
        pub empty_body: bool,
    }

    pub enum ListMarkerSource {
        Unordered { bullet: u8, separator: String },
        Ordered { delimiter: u8, source_number: String, separator: String },
    }

実際の型は既存 MarkdownParse::list_item_markers、list_structural_prefixes、MarkdownTree と重複する
情報を必要最小限に投影する。NodeId は parse snapshot の寿命に限定されるため、planner の公開結果で
構造 identity として返さず、source range / ordinal / 親子関係を使う。

#### 同期 projection の保持場所と更新

BlockIndex に document-wide の巨大な編集 projection 配列を追加しない。第1段階では
`BlockIndex::list_edit_projection_at` が current revision の caret 所属 block だけを同期 parse し、
`ListEditProjection` を返す方式を採用した。編集頻度と block サイズを計測した後、必要なら直近 dirty
block/window の `Option<ListEditProjection>` cache へ拡張する。

どちらを選んでも次を満たすこと。

- projection の revision と block id / source range が current document と完全一致する。
- 編集された block/window は BlockIndex の current revision 更新後、次の key action で直ちに再構築できる。
- 編集に交差しない cached projection は RevisionDelta で安全に rebase できる場合だけ保持する。
- rebase できない、parse window が再同期できない、または source range が一致しない場合は破棄する。
- 全 document を毎打鍵で走査・再構築しない。第1段階の同期 parse は caret 所属 block に限定する。

現在の BlockIndex::update にある self.list_projections.clear() は、stale な formal presentation projection を
無効化するために従来どおり必要である。その直後に、同じ dirty window の parse から編集 projection を再構築する
設計へ変更する。formal projection が一時的に無くても、list_edit_projection は exact current revision で
Tab / Shift+Tab を判断できる。

次の連続入力を必須 contract とする。

    Enter
      → source が "- A\n- " になる
      → caret 所属 block の ListEditProjection が current revision で再構築される
    Tab
      → background formal parse を待たず、"- A\n  - " になる

projection が同期予算を超えて安全に作れない場合は NotApplicable とし、source を推測編集しない。background
formal parse の完了後に次の入力で再試行できるようにする。

### 5. Enter

#### 非空の list item

caret が list item の本文中にある場合、現在 item の同じ list / 同じ階層の新しい item を caret の位置で作る。
計画は通常、range = caret..caret の一回の replacement になる。

    - ABC|DEF

    - ABC
    - |DEF

replacement は次の形で作る。

    "\n" + current item の marker indentation / container prefix
          + new marker prefix

新しい caret は生成した marker prefix の本文開始位置、つまり -  や 10.  の直後に置く。既存の suffix は
新しい item の本文になる。caret が item の opening paragraph の途中にある場合も同じ規則を使う。

現在 item の source marker の種類を維持する。

- - / * / + は同じ bullet byte を使う。
- marker 後の separator は現在 item の source style を維持する。生成分の indentation は columns を基準に
  spaces で作ってよい。
- ordered list は current list の意味上の ordinal から list_start + ordinal + 1 を計算する。後続 source
  marker の数字を読み替えたり、既存 item を正規化したりしない。
- delimiter . / ) は現在 item のものを維持する。
- 001. のような既存 source marker の leading zero は既存 bytes として維持するが、新しく生成する意味上の
  番号は canonical decimal とする。必要になれば style policy を別 ADR で追加する。
- task item から作る新 item は常に unchecked ([ ]) とする。

例:

    3. A
    41. B|

    3. A
    41. B
    5. |

上の表示例の末尾空白は説明上の caret 表記であり、実際の source は 5.  である。41 を 42 に直すの
ではなく、list の start=3 と current ordinal から意味上の次番号 5 を生成する。

    - [x] 完了|

    - [x] 完了
    - [ ] |

#### 空 item の Enter

空 item は、opening line の marker / task prefix の後ろに非空本文がない item とする。caret はその本文開始
位置または空本文末尾に限る。

- nested item は subtree 全体を一階層 outdent し、marker は維持する。
- top-level item は list marker / task prefix を除去し、同じ physical line を通常 paragraph にする。
- Enter 自体は新しい transient state を作らない。結果 source が parser にとって有効な通常 paragraph になる。

    - A
      - B
      - |

    - A
      - B
    - |

top-level の場合は、line ending を残して marker prefix だけを削除する。文書末なら source は直前の改行で
終わるか、既存の line ending を持つ。空行を追加するかどうかを View の都合で決めない。

### 6. Shift+Enter

Shift+Enter は新しい item を作らず、同じ list item の本文 continuation を source に作る。

    - ABC|

    - ABC
      |

replacement は "\n" + current item の body continuation prefix である。body continuation prefix は
current item の marker width、separator、入れ子の container prefix を CommonMark の構造から算出する。
unordered list の -  は通常 2 columns、10.  は通常 4 columns になるが、2 spaces 固定の規則にはしない。

### 7. Tab / Shift+Tab

Tab 系は paragraph の先頭でのみ list-aware とする。具体的には、caret が collapsed で、current item の
opening line の marker start から body start までの prefix 内または body start にある場合だけ planner を呼ぶ。
本文中の Tab を list hierarchy edit と解釈しない。

#### Tab（indent）

同じ list の直前 sibling が存在しない場合は Handled(NoOp) とする。最大3 spaces の marker 規則で、単に
先頭へ spaces を足しただけでは親子と保証できないためである。

直前 sibling がある場合、current item とその subtree 全体を直前 sibling の child に移す。

    - A
    - B
      - C

    - A
      - B
        - C

移動幅は固定値ではなく、直前 sibling の marker indentation からその child の marker を合法に開始できる
body_indent_columns までとする。

- -  / +  / *  の通常形は 2 columns。
- 10.  は 4 columns。
- task marker は marker width に含める。
- ordered marker の桁数と separator の幅を source metadata から算出する。

current item 自身とすべての子孫 item、continuation line の構造 prefix を同じ相対関係のまま移動する。subtree
の外側にある次の同階層 item、後続 paragraph、別 list は変更しない。

#### Shift+Tab（outdent）

current item に親 item がない場合は Handled(NoOp) とする。親 item がある場合、current item と subtree
全体を親 item と同じ marker indentation へ移す。

    - A
      - B
        - C

    - A
    - B
      - C

Tab と Shift+Tab の source operation は、対象 subtree の連続 source range を一回の replacement にまとめる。
各 physical line の prefix を parser が所有情報に基づいて変換し、本文 bytes、line ending、descendant 間の
相対 indentation は維持する。tab byte を途中で切るなど CommonMark 上の意味を推測する変換はせず、必要なら
対象 prefix 全体を canonical spaces へ置き換える。

### 8. 空 item の後退削除

selection が caret のみで、caret が空 item の body start にあるときだけ list-aware Backspace / macOS Delete
を適用する。

- nested item は Enter と同じく subtree 全体を一階層 outdent する。
- top-level item は marker / task prefix を削除して通常 paragraph にする。
- 非空 item、本文中、selection 付きの Backspace は既存の通常文字削除に戻す。
- Enter 直後の特別な「直前の newline を undo する」context は作らない。必要なら通常の Undo が Enter の
  一つの transaction を戻す。

    - A
      - B
      - |

    Backspace:

    - A
      - B
    - |

### 9. Markdown と Editor の crate 境界

| crate | 追加・変更する責務 | 持ってはいけない責務 |
|---|---|---|
| hane-document | 既存の byte range、revision、EditSummary、RevisionDelta | Markdown の list 判定 |
| hane-editor | replace_range_recorded 相当の汎用 recorded replacement、selection 検証、履歴記録 | list marker、indentation、ordered numbering |
| hane-markdown | ListEditProjection、MarkdownEditPlan、source-only planner、prefix/subtree変換 | GPUI、pixel、visual caret、IME platform API |
| hane-presentation | current source を表示する list metadata、SourceMap、layout | Enter 後の仮想 item、編集用 caret origin |
| hane-ui | key routing、projection lookup、plan 適用、fallback、frame invalidation | Markdown の再解析・markerの手作業推測 |
| hane-session | 既存の editor/session、save/reopen | list-specific editing state |

hane-editor には次のような Markdown 非依存 API を追加する。

    impl Editor {
        pub fn replace_range_recorded(
            &mut self,
            range: SourceRange,
            replacement: &str,
            selection_after: Selection,
        ) -> Result<EditSummary, BufferError>;
    }

この API は、開始時に active composition を commit し、range と replacement を RopeBuffer::edit へ一度だけ
渡し、編集後 selection を検証して設定する。履歴へは独立した一つの replacement transaction として記録し、
通常の 750 ms typing merge に混ぜない。MarkdownEditPlan を editor crate に渡すための依存は作らない。

UI 側は plan の SourceSelection を editor の Selection へ明示変換してからこの API を呼ぶ。

### 10. キー入力の処理順序

crates/ui/src/actions.rs は次の方針へ変更する。

1. sidebar / inline rename の専用入力を先に処理する。
2. editor の selection と IME 状態を読む。composition 中なら既存 ADR-0007 の規則に従って commit してから
   structural key を再評価するか、platform が composition command として消費した場合は何もしない。
3. collapsed caret の場合、current revision の ListEditProjection を取得する。
4. plan_list_edit を呼び、Handled(Replace) なら replace_range_recorded を一度呼ぶ。
5. Handled(NoOp) なら source を変更せずキーを消費する。
6. NotApplicable なら Enter / Shift+Enter / Backspace は既存の通常 EditorCommand に fallback する。
7. source edit 後は既存 after_input を通し、BlockIndex、height、scroll、autosave、background parse を更新する。

Enter、Shift+Enter、Tab、Shift+Tab、Backspace は presentation の ListCaretOrigin を引数にしない。

### 11. IME

Enter 時に marker indentation を遅延して挿入しないため、IME は通常 source range をそのまま扱える。

- ImeState から list-specific prefix を削除する。
- current_range と marked_range は実際に source に存在する marked text range と一致させる。
- replace_and_mark_text_with_prefix は削除し、通常の replace_and_mark_text / commit_text を使う。
- Enter が先に実行された場合、IME は既に source にある - 、    1. 、body prefix の後ろから開始する。
- IME composition の更新・commit は prefix を二重挿入しない。
- IME cancel は composition 開始時の文字だけを戻し、Enter / Tab 自体の transaction は戻さない。
- structural key の前に composition があれば、ADR-0007 の「command 前に commit」規則を使う。キャンセル中の
  marked range と list plan を同時に推測しない。

これにより crates/ui/src/input.rs の pending_list_indentation 読み取り、prefix concatenation、
composition cancel 時の list context 復元を削除できる。

### 12. selection、caret、presentation

source に marker が実在するため、通常の parser / SourceMap / layout が次を一貫して処理する。

- Enter 後の -  は empty list item の source marker になる。
- caret は実際の marker prefix の body start に置かれる。
- marker を開示するかどうかは existing selection / caret disclosure contract が決める。
- body continuation は実際の indentation bytes の後ろに置かれる。
- Tab / Shift+Tab は prefix replacement 後の source offset map で caret を同じ本文位置へ移す。
- selection 付きの structural key は第1実装では list-aware plan を作らず、通常の selection contract を壊さない。

presentation には「空 source line に list owner を付与する」特別経路を追加しない。

### 13. Undo / Redo

一回のキー操作が一回の source transaction になる。

| 操作 | 一つの recorded replacement |
|---|---|
| 非空 item の Enter | caret に \n + marker prefix を挿入 |
| Shift+Enter | caret に \n + body continuation prefix を挿入 |
| Tab / Shift+Tab | subtree range の prefix を一括置換 |
| 空 nested item の Enter / Backspace | subtree を一階層 outdent |
| 空 top-level item の Enter / Backspace | marker / task prefix を削除 |

Undo / Redo は RopeBuffer の通常 edit として revision を進める。BlockIndexState::apply_edits は Undo / Redo
でも current source を同期し、編集 projection を作り直す。Undo 時に presentation cache や transient list
context を復元する特別処理はない。

Enter 後に本文を入力した場合、Enter の structural transaction と通常の文字入力 transaction は別である。
Undo 一回で Enter を戻し、もう一回で後続文字を戻せることをテストで固定する。IME commit 全体は ADR-0007 / ADR-0013
どおり一つの IME transaction とする。

### 14. 性能と stale result

- planner は current caret が属する list block と、Tab 系で実際に移動する subtree だけを読む。
- subtree の prefix rewrite は出力 bytes の長さに比例する。大きな subtree を一回の transaction で動かすコストは
  避けられないが、文書全体を毎回走査しない。
- BlockIndex の dirty-window 再解析予算（現行の byte / block budget）を尊重する。
- ListEditProjection は revision-bound とし、revision、block id、source range のどれかが不一致なら planner
  へ渡さない。
- background formal parse は編集 projection の代用にしない。古い formal result が current source を上書きしない
  規則は ADR-0005 / ADR-0018 のまま維持する。
- projection を同期構築できない場合は fail closed（NotApplicable）とし、raw source bytes を変更しない。
- Tab / Shift+Tab の連打では各操作後に新しい source と current projection を使う。古い item range を再利用しない。

metrics には必要に応じて list_edit_plan の計画時間、対象 bytes、subtree bytes、projection source（sync cache /
bounded rebuild / unavailable）を追加する。ただし既存入力の hot path に文書全体の計測や allocation を追加しない。

## 現行実装からの移行と削除対象

### Phase 0: contract と API の追加

1. hane-markdown に ListEditProjection、marker metadata、MarkdownEditPlan、planner の純粋関数を追加する。
2. hane-editor に generic replace_range_recorded と selection-after contract を追加する。
3. planner / editor / BlockIndex の unit test を先に追加し、UI の既存挙動はまだ変えない。

### Phase 1: 同期 projection と source edit の配線

1. BlockIndex::update が dirty parse window から編集 projection を current revision で構築する。
2. BlockIndexState::apply_edits と formal publish / rebase が編集 projection の exactness を壊さないようにする。
3. EditorView が current caret の projection を取得する。UI は Markdown source を直接 scan しない。
4. Enter を list item 上で MarkdownEditPlan に切り替え、Enter → Tab が background parse なしに動くことを固定する。

### Phase 2: 全 key action と IME の切り替え

1. Shift+Enter、Tab、Shift+Tab、空 item Backspace を planner 経由にする。
2. crates/ui/src/input.rs の deferred indentation / prefix injection を削除する。
3. IME tests を actual source prefix 前提へ書き換える。
4. selection、undo / redo、mouse hit test、caret / SourceMap contract を回帰させる。

### Phase 3: transient list editing の撤去

source-first 経路が全て通った後、次を削除する。

#### crates/presentation

- ListCaretOrigin
- ListEditingContext
- apply_list_editing_context
- ListRowMetadata::empty_caret_origin
- ListRowMetadata::source_prefix（編集だけに使われる場合）
- layout.rs の transient caret origin 分岐
- これらを検証する presentation tests

通常の list owner、marker metadata、structural prefix metadata、body geometry は残す。表示用 list metadata と
編集用 projection を混ぜない。

#### crates/ui

- EditorView::pending_list_editing
- clear_pending_list_editing
- pending_list_indentation
- list_editing_context
- 旧 insert_newline(ListCaretOrigin) の transient context 経路
- cached_block / presentation rebuild の apply_list_editing_context 分岐
- mouse movement / selection / unmark からの pending context cleanup
- pending_list_editing を前提にした UI tests

通常の after_input、block cache、layout cache、scroll、IME candidate bounds は残し、source revision と
selection の変化から無効化する。

#### crates/editor

- ImeState::prefix
- replace_and_mark_text_with_prefix
- deferred list indentation 専用の IME tests

generic IME composition、UTF-16 ↔ byte conversion、cancel conflict、Undo transaction は残す。

#### crates/ui/src/actions.rs / input.rs

- ListCaretOrigin import
- Enter / Shift+Enter の context 引数
- text input / IME callback の pending indentation concatenation
- list-aware key を EditorCommand の Markdown command として追加する実装

Tab / Shift+Tab の action は UI に置くが、実際の source rewrite は planner に委譲する。

#### scripts / tests / docs

- hosted_list_enter_gui.py の「Enter 後に marker を入力する」期待値を、Enter 直後に -  が source に存在する
  期待値へ変更する。
- #220 の旧 transient contract tests は削除または source-first contract tests へ置換する。
- ADR-0029 が承認されたら ADR index の Proposed から Active へ移し、Issue #220 / PR #221 の旧判断を本 ADR へ
  参照付けする。

## テスト計画

### Markdown planner の table-driven tests

各 test は caret を | で表し、plan 適用後の exact source と selection を検証する。最低限、次を hane-markdown
で固定する。

| ケース | 入力 | 期待 source |
|---|---|---|
| bullet Enter | - A\| | - A\n- \| |
| bullet split | - AB\|CD | - AB\n- \|CD |
| nested Enter | - A\n  - B\| | - A\n  - B\n  - \| |
| ordered next | 10. A\| | 10. A\n11. \| |
| non-1 ordered | 3. A\n41. B\| | 3. A\n41. B\n5. \| |
| task | - [x] A\| | - [x] A\n- [ ] \| |
| Shift+Enter | 10. A\| | 10. A\n    \| |
| empty nested Enter | - A\n  - \| | - A\n- \| |
| empty top Enter | - A\n- \| | - A\n\| |
| empty nested Backspace | - A\n  - \| | - A\n- \| |
| first sibling Tab | - A\|\n- B | unchanged / NoOp |
| Tab subtree | - A\n- B\n  - C with B caret | - A\n  - B\n    - C |
| Shift+Tab subtree | - A\n  - B\n    - C with B caret | - A\n- B\n  - C |
| top-level Shift+Tab | - A\| | unchanged / NoOp |

| は source byte ではなく test helper の caret notationであり、保存 bytes には含めない。CRLF、bare CR、UTF-8、
leading zero、. / )、- / * / +、task marker、quote 内 list、malformed marker も追加する。

### Editor contract tests

hane-editor では Markdown 型を依存させず、次を固定する。

- arbitrary SourceRange replacement が一回の revision になる。
- selection_after が正確に復元される。
- range replacement が一つの undo entry / redo entry になる。
- structural replacement が通常の 750 ms typing merge に入らない。
- Unicode byte boundary、空 replacement、CRLF、selection の anchor / active を検証する。

### BlockIndex / projection tests

crates/markdown/src/block_index.rs に次を追加する。

1. formal projection が clear される編集でも current dirty block の ListEditProjection が作られる。
2. Enter で - A\n-  を作った直後に list_edit_projection が empty item を返す。
3. Enter の直後に Tab を plannerへ渡して、formal background result なしに nested source が生成される。
4. source revision、block id、source range が mismatch の projection は reject される。
5. stale background formal parse が current edit projection を置き換えない。
6. projection unavailable 時は NotApplicable になり、source が変わらない。

### UI / presentation / session tests

- Enter / Shift+Enter / Tab / Shift+Tab / empty Backspace の key routing。
- actual source marker が存在する Enter 後の caret geometry と SourceMap round-trip。
- selection 付き Tab は no-op、selection 付き Enter / Backspace は既存の通常 command。
- marker disclosure、mouse hit test、vertical movement、soft wrap が source edit 後も正しい。
- IME composition update / commit / cancel を Enter 後、Tab 後、empty item 上で実行する。
- 各 structural key の Undo / Redo、保存、reopen が exact source bytes と同じ list structure を復元する。
- 既存 list presentation（nested、複数段落、ordered start、hanging indent、task marker）を回帰させない。

### GUI validation

既存の scripts/hosted_list_enter_gui.py を source-first contract 用に更新し、macOS の実アプリで独立に
保存 bytes を検証する。最低限の focused scenario は次のとおり。

1. - abc → Enter → source が - abc\n-  → Tab → - abc\n  - 。
2. nested item → Enter → 同じ depth の空 sibling → Tab / Shift+Tab。
3. ordered 10. abc → Enter → 11. 、10. abc\n    1. xyz の ordered width。
4. task checked item → Enter → unchecked item。
5. Shift+Enter → body continuation、通常 Enter と source bytes が異なること。
6. empty nested item → Enter / Backspace → 一階層 outdent。
7. empty top-level item → Enter / Backspace → list marker 解除。
8. Enter → IME 日本語 commit / cancel → prefix 二重挿入なし。
9. Tab / Enter / Undo / Redo → save / reopen。

GUI は unit contract test の代替ではない。fixture byte polling、screenshot、アプリ起動・window discovery、
working tree clean の証拠を従来の trusted validation 経路で残す。

## 受け入れ条件

実装完了は、次を全て満たした場合とする。

- Markdown source が文書構造の唯一の Model であり、Enter / Shift+Enter / Tab / Shift+Tab / empty Backspace が
  source edit として即時に観測できる。
- Enter は同階層・同種の新 item を source に作り、ordered は意味上の次番号、task は unchecked item を作る。
- 空 nested item の Enter / Backspace は一階層 outdent、top-level は list marker を解除する。
- Tab は直前の同階層 sibling がある場合だけ subtree 全体を一階層下げ、first sibling は no-op。
- Shift+Tab は subtree 全体を一階層上げ、top-level は no-op。
- unordered の標準形は通常 2 columns、ordered は marker width + separator の CommonMark 構造幅であり、2 spaces
  固定ではない。
- 複数 selection の Tab / Shift+Tab は第1実装対象外で、source を破壊しない。
- Enter 直後の Tab が formal background parse の完了を待たずに動く。
- pending_list_editing、ListEditingContext、ListCaretOrigin、apply_list_editing_context、
  pending_list_indentation、IME prefix dependency が削除または list structure に依存しない形へ縮小される。
- source↔visual mapping、caret、selection、IME、Undo/Redo、save/reopen、existing list presentation の contract が
  回帰しない。
- workspace tests、clippy、format check、focused GUI validation が current implementation head で成功する。

## 却下した代替案

### Enter は改行だけにし、View で空 item を仮想化する

却下する。これは #220 / PR #221 の現行方式であり、今回の source-first 要件と、Enter 直後の構造編集の
決定性に反する。

### Editor に IndentList / NewListItem command を追加する

却下する。editor crate が Markdown syntax と presentation の境界を知ることになり、現行の editor → document、
markdown → document という依存方向を壊す。

### Tab を常に 2 spaces として実装する

却下する。ordered marker の幅、task prefix、quote / nested container を表現できず、CommonMark 上の親子構造を
保証できない。

### Enter / Tab の後に全文 formal parse を同期実行する

却下する。入力 latency が document size に比例し、ADR-0018 の dirty-window / background parse 契約に反する。
必要な範囲だけ同期 projection を作り、formal parse は background で補完する。

## 結果

この設計では、PowerPoint 風の list editing intent を Markdown source の合法な bytes へ直ちに変換できる。
その結果、保存 source、parser、presentation、layout、caret、Undo/Redo が同じ事実を共有する。

表示用 formal projection が一時的に更新途中でも、直前の編集で作った exact revision の ListEditProjection が
次のキー操作を受け持つため、Enter → Tab のような連続入力が background timing に依存しない。

一方、複数 selection の structural editing、複雑な malformed Markdown の自動修復、既存 source の全体整形はこの
ADR の範囲外とし、別設計で扱う。

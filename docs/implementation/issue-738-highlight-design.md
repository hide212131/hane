# Issue #738 実装設計書: ハイライト（見出しなどへの色付け）

> 位置づけ: 採用済み設計（2026-11-17、Claude Fable 5.2 による設計を Commander が検分し採用）。本書が本 Issue 実装の正本であり、判断に迷う場合は本書の決定表（9章の推奨）と仕様表（3章・4章）に従うこと。未実装・未検証の準備文書であり、製品コードの現状説明ではない。
>
> Commander 追記（2026-11-17）: 本書の調査対象は #737 実装前の main（6d45d29a）である。#737 は PR #739 で実装済み（main 32e9d187）であり、本書が「#737 の実装後に存在するもの」として参照する名前（present_block_plain、DisclosurePolicy::Plain、matches_plain、plain_text_mode.rs 等）は実在する。行番号・前提が実コードとずれる箇所は、実コードを正として読み替えること。本 PR の範囲は P1（色付け本体）＋ P2（設定画面）であり、P3（巨大ブロックの正確化）は設計書 8 章のとおり実使用で必要性を確認してから別途行う（本 PR の対象外）。


- 対象 Issue: #738「feat(editor): ハイライト（見出しなどへの色付け）」
- 関連 Issue: #737（プレーンテキストモード。設計書 `issue-737-plain-text-mode.md`）
- 調査対象: origin/main = `6d45d29a`（v0.47.0）
- この文書の行番号はすべて上記コミット時点のもの。実装時にずれていたら関数名で探すこと。
- 「未確認」と書いた箇所は、コードを読み切れていない。実装前に確認が要る。
- #737 は未実装である。#737 の設計書が定義する名前（`present_block_plain`、`DisclosurePolicy::Plain`、`matches_plain`、`plain_text_mode.rs` など）は「#737 の実装後に存在するもの」として参照する。

---

## 0. 結論（先に読む）

| 論点 | 決定 |
| --- | --- |
| 適用先 | **プレーンテキストモード専用**。通常（投影）モードの表示は 1 バイトも変えない |
| 色を付ける層 | 分類は `presentation`（`present_block_plain` の中で `style_runs` を作る）。色の解決は `ui`（`Theme`）。`presentation` は「役割」だけを出し、色を知らない |
| 分類の入力 | 既存の `parse_document`（pulldown-cmark）をブロック単位で 1 回走らせた `MarkdownParse`。新しい字句解析器は書かない。トップレベルのコードブロックだけは解析せず行の位置で決める |
| 表現 | `StyleKind::Syntax(SyntaxTone)` を追加し、`InlineDisplay.tone: Option<SyntaxTone>` に落とす。太字・斜体・打消しは既存の `StyleKind::Bold` / `Italic` / `Strikethrough` をそのまま使う |
| 変えないもの | 文字サイズ、行高、フォントファミリ（等幅にしない）、`BlockKind`（`Unsupported` のまま）、`visual_text`、SourceMap |
| 色 | 新規トークン 4 個（見出し 2 段、コード、構造マーカー）＋既存 2 個（`quote_foreground`、`link_foreground`）。値は 4 章 |
| 設定 | `settings.conf` に `syntax_highlight=true\|false`。欠損は `true`。プレーンモードの設定とは独立。強さの段階は持たない |
| 再計算の単位 | ブロック。既存の `block_cache`（`VisualBlock` 単位）に乗る。編集したブロックと、表示範囲が変わった巨大ブロックだけ再計算 |
| コードブロックの言語別ハイライト | やらない（現行にも無い。依存追加が要る別件） |
| 先行条件 | #737 のフェーズ P1（`present_block_plain` と `DisclosurePolicy::Plain`）が main に入っていること |

最重の不変条件は次の 4 つ。すべてテストで固定する（7 章）。

1. ハイライト ON / OFF で、各行の `visual_text` と `source_map` が完全に同じである（色付けは `style_runs` だけを変える）。
2. ハイライト ON / OFF の切替の前後で `(Revision, 本文長, dirty, can_undo)`、本文バイト列、キャレットと選択のソースオフセットが変わらない。
3. ハイライトの有無で `VisualLine.estimated_height`、`VisualBlock.kind`、`plain_block_height` が変わらない。
4. 字幅に影響する属性（太字・斜体）は必ず `style_runs` を通す。`shape.rs`（幾何）と `line.rs`（描画）が同じ `style_runs` を読むので、キャレットの x 位置と描画がずれない。

---

## 1. 目的と範囲

### 1.1 目的

プレーンテキストモードでは Markdown の記号がすべて見え、文字サイズも揃うため、見出しやコードの位置が一目で分からない。記法の要素を色と太さで区別し、文書の構造を読み取りやすくする。Issue の要求は「見出しなどを色つける。いい感じに」（2026-11-16、オーナー）。

「いい感じ」をこの設計では次のように定義する。

- **記号は薄く、内容を立てる**。`#` や `**` などの区切り記号は本文より薄い色にし、見出しの本文・コード・リンクの文字を色で立てる。
- **色数を絞る**。既存テーマは暖色の無彩色＋青 1 色。追加する色相は 2 つ（錆色、緑）だけ。
- **色だけに頼らない**。見出しと強調は太字、強調（`*`）は斜体、打消しは取り消し線を併用する。プレーンモードでは記号自体が見えているので、色が識別できなくても構造は読める。
- **レイアウトを動かさない**。文字サイズと行高は全要素で同じ。

### 1.2 やること

- プレーンテキストモードでの Markdown 構文の色付け（対象要素は 3.1 節）
- ライト / ダーク両テーマの色トークン追加
- ON / OFF 設定（設定画面のチェックボックス、永続化）
- 巨大ブロックでの縮退動作の定義

### 1.3 やらないこと

- 通常（投影）モードへの色付け（3.6 節に根拠。follow-up 候補）
- コードブロック内の言語別ハイライト（2.4 節。`syntect` / `tree-sitter` などの依存はリポジトリに無い）
- 等幅フォントへの切替（#737 U-3。本 Issue でも「本文フォントのまま」で確定する。9 章 U-5）
- 行末空白・タブの可視化（#737 U-10。別 Issue）
- 色のカスタマイズ、強さの段階、テーマの追加
- 参照リンク（`[text][ref]`）の定義解決（3.5 節。ブロック単独の解析では定義が見えない）
- 脚注（`parser_options()` が `ENABLE_FOOTNOTES` を有効にしていないので、そもそも解析されない）
- 既存の投影経路・スタイル経路のリファクタリング

### 1.4 着手メモ（docs/refactor-execution-plan.md §2.1 用）

- 変更する責務: `presentation` の表示ポリシー（`StyleKind` / `InlineDisplay`）への役割 1 種の追加、プレーン提示での `style_runs` 生成、`ui` の `Theme` トークンと描画時の色解決、`session` の `Settings`。
- 関連 Issue: #737。
- 先行条件: **#737 P1 に依存する**。待つ理由は、注入点である `present_block_plain` と `DisclosurePolicy::Plain` が #737 P1 で新設されるため。#737 P2（設定画面）と P3（Tab）には依存しない。ただし本 Issue の設定チェックボックスは #737 P2 のチェックボックスの隣に置くので、P2 が先なら配置が楽になる。
- 使う経路と検証: #737 が受け入れた `presented_block(Plain)` → `present_block_plain` → `layout_block` 経路。旧経路や投影経路には足さない。検証は 7 章。
- 変更前測定: 7.4 節。

---

## 2. 現行処理の調査

### 2.1 parser の要素分類

解析は `crates/markdown/src/parser.rs` の `parse_document(revision, range, source) -> MarkdownParse`（L508、`hane_markdown` から公開）。pulldown-cmark を `parser_options()`（L26-28: `ENABLE_STRIKETHROUGH | ENABLE_TASKLISTS | ENABLE_TABLES`）で走らせ、イベント列から木を作る。

**ノード種別**（`crates/markdown/src/syntax.rs` の `NodeKind`）:

| 分類 | 種別 |
| --- | --- |
| ブロック | `Document`、`Paragraph`、`Heading(u8)`、`CodeBlock`、`Quote`、`List{start}`、`ListItem{task}`、`Table`、`TableHead`、`TableRow`、`TableCell`、`Rule`、`HtmlBlock`、`FootnoteDefinition` |
| インライン | `Text`、`Strong`、`Emphasis`、`Strikethrough`、`InlineCode`、`Link`、`Image`、`Html`、`InlineHtml`、`FootnoteReference`、`TaskMarker(bool)`、`SoftBreak`、`HardBreak` |
| その他 | `Unsupported`（モデル化していないタグ・イベント） |

各ノードは `MarkdownNode { kind, source_range, parent, children, depth }`。`source_range` は元ソースの絶対オフセット。

**マーカーのサイドテーブル**（`MarkdownParse` のフィールド。導出は `crates/markdown/src/markers.rs` の `derive_markers` L496-729）:

| フィールド | 内容 | 導出元 |
| --- | --- | --- |
| `markers` | 構文記号の範囲。ソート済み・**隣接や重なりはマージ済みで種別を持たない** | 下記すべて |
| （見出し） | ATX の `#…` と後続の空白、閉じ `#` と末尾空白。Setext の下線は含まない | L510-542 |
| `quote_markers: Vec<(SourceRange, NodeId)>` | 引用の行頭プレフィックス（`> `）。行ごと・所有 Quote ごと | L543-548 |
| `list_item_markers: Vec<(SourceRange, NodeId)>` | リスト記号（`-`、`1.` など）＋区切り 1 バイト | L549-572 |
| `fence_markers` / `fence_marker_edges: Vec<(SourceRange, FenceMarkerEdge)>` | フェンスの開き（インデント＋記号列。**info string は含まない**）と閉じ行 | L573-647 |
| （インライン） | `Strong` / `Emphasis` / `Strikethrough` / `InlineCode` の開閉記号、`Link` の `[` と `](` 以降末尾まで | L651-687 |
| （改行） | `HardBreak` の構文部分（行末空白 or `\`） | L688-706 |
| `task_markers: Vec<TaskMarkerParse>` | `marker_range` = `[ ]` / `[x]` の 3 バイト | L739-799 |
| `links: Vec<LinkParse>` | **http(s) の外部リンクと裸 URL だけ**。`text_range` はリンク文字の範囲 | `parser.rs` L217-237 |

注意点:

- `Link` のマーカー（L674-680）は `](` を持つインラインリンクにしか付かない。参照リンクとオートリンク（`<https://…>`）にはマーカーが無い。
- `Image` は `has_delimiter_markers`（L84-94）に含まれず、マーカーが導出されない。
- 表の `|` と区切り行はマーカーに含まれない。
- `links` は http(s) 限定なので、相対パスや `mailto:` のリンクは入らない。色付けは `links` ではなく `NodeKind::Link` ノードから行う必要がある。

**ブロック索引**: `crates/markdown/src/block_index.rs` の `IndexedBlock { ordinal, id, kind: NodeKind, source_range, revision, confidence, line_count, leading_content_lines }`（L72-95）。`kind` はトップレベルブロックの種別。`leading_content_lines` はブロックの span 先頭にある空行の数で、フェンスの開き行の位置を再走査なしで得るためにある。

### 2.2 表示スタイルの適用経路

```text
NodeKind ──syntax_display()──▶ SyntaxDisplay { indexed_block, node_block, inline_style, line_context }
                                   │（crates/presentation/src/display.rs。parser → 表示の唯一の継ぎ目）
                                   ▼
        StyleKind ──▶ StyleRun { visual_range, kind } ──▶ VisualLine.style_runs
                                   │
                    StyleKind::display() ──▶ InlineDisplay（フラグの集合。重なりは union）
                                   │
              ┌────────────────────┴─────────────────────┐
              ▼                                          ▼
  crates/ui/src/shape.rs                        crates/ui/src/line.rs
  WindowShaper::runs（L51-91）                   row_element（L741-1070）
  幾何: 太さ・斜体・等幅 → TextRun               描画: セグメントごとの div に属性を付ける
```

- `StyleKind`（`display.rs` L7-18）: `Bold`、`Italic`、`Strikethrough`、`InlineCode`、`CodeBlock`、`Link`、`Image`、`Table`、`MarkedText`。`Clone, Copy, Debug, Eq, PartialEq`。
- `InlineDisplay`（L51-62）: `bold`、`italic`、`strikethrough`、`monospace`、`code_background`、`underline`、`link_color`。すべて `bool`。`union`（L66、`const fn`）は各フラグの OR。**色の役割を表すフィールドは `link_color` しか無い**。
- `StyleKind::display()`（L87〜、`const fn`）: `Bold`→bold、`Italic`→italic、`Strikethrough`→strikethrough、`InlineCode`→monospace+code_background、`CodeBlock`→monospace、`Link`→underline+link_color、`MarkedText`→underline、`Image` / `Table`→なし。
- ブロック単位は `BlockKind::display() -> BlockDisplay { font_scale, weight, surface, tint, monospace }`。見出しは `font_scale`（24/14、21/14、18/14、16/14）＋ `Semibold`、コードブロックは `BlockSurface::Code` ＋等幅、引用は `BlockTint::Muted`、表は `BlockSurface::Table`。`BlockSurface` / `BlockTint` は「役割であって色ではない」という設計で、色は UI が `Theme` から引く。
- UI は `StyleKind` を match しない。`InlineDisplay` のフラグだけを見る。

**幾何と描画の分離**（本設計の前提になる）:

- `shape.rs` の `WindowShaper::runs`（L51-91）は `line.style_runs` の端点で区切り、区間ごとに `inline_display_for(&range, &line.style_runs)` を引いて `FontWeight` / `FontStyle` / family を決める。`TextRun.color` は常に `self.style.color`（L84）で、**色は幾何に入らない**。
- `font_revision()`（L41-46）はフォントとズームだけをハッシュする。doc コメント（L35-40）に「色などの描画専用フィールドは意図的に除外。パレット変更で有効な幾何を捨てない」とある。
- 描画は `line.rs` の `row_element`。`line_segments_with_hover`（L1554-1611）が選択・IME・検索・`style_runs` の全端点で行を分割し、各 `LineSegment` を 1 つの `div` として描く。適用順（L1000 付近）: 背景は 選択（`selection_background`）> 検索の現在位置（`find_current_match_background`）> 検索一致（`find_match_background`）。続いて下線、太字、斜体、取り消し線、等幅、`text_bg(code_background)`（`segment_shows_code_background` L1489: 選択中・検索一致中は出さない）、最後に `link_color` のとき `text_color(rgb(link_text_color(theme, hover)))`。
- 行全体の色は `styled_block_with_surface`（L600-622）が `BlockTint::Muted` のとき `quote_foreground` を当てる。それ以外は継承（`theme.foreground`）。

**`present_markdown_from_parse` の style_runs 生成**（`crates/presentation/src/projection/mod.rs` L1226-1265）: `has_delimiter_markers(kind)` のノードを `syntax_display(kind).inline_style` で `StyleKind` に写し、行で切って `source_map.source_to_visual` で visual 範囲にする。**ラン範囲は区切り記号を含むノード全体**（開示中は `**` も太字になる）。

### 2.3 テーマの色トークン体系

`crates/ui/src/theme.rs`（220 行、全読）。`pub(crate) struct Theme`（`Copy`、フィールドは `u32` の RGB）。`DEFAULT_THEME`（ライト）と `DARK_THEME` の 2 定数。`resolve_theme(preference, appearance)` が選ぶ。

| トークン | ライト | ダーク | 用途 |
| --- | --- | --- | --- |
| `editor_background` | `0xfaf9f7` | `0x1f2022` | 本文背景 |
| `foreground` | `0x262626` | `0xe8e5df` | 本文文字、キャレット |
| `selection_background` | `0xe8eefc` | `0x34435f` | 選択 |
| `find_match_background` | `0xfff1a6` | `0x5c4b1e` | 検索一致 |
| `find_current_match_background` | `0xffa94d` | `0xcf7f2e` | 検索の現在位置 |
| `code_background` | `0xeeeae4` | `0x333438` | インラインコード背景 |
| `code_block_background` | `0xf1eee9` | `0x292a2e` | コードブロック面 |
| `link_foreground` | `0x2867a9` | `0x79b8ff` | リンク文字 |
| `link_hover_foreground` | `0x0066ff` | `0x40c8ff` | ホバー中のリンク |
| `quote_foreground` | `0x6b6259` | `0xaaa39a` | 引用文字、引用バー、水平線、画像 alt など。**唯一の「薄い文字色」** |
| `table_background` / `table_header_background` / `table_border` | `0xf5f2ed` / `0xe9e4dc` / `0xd2cbc0` | `0x27282b` / `0x34363a` / `0x55585f` | 表 |
| `tab_active_background` | `0x356d94` | `0x44779e` | アクティブタブ（青） |

特徴: 暖色寄りの無彩色＋アクセントは青 1 色。**構文用のトークン（緑・赤・紫など）は存在しない**。見出し専用の色も無い（見出しは `foreground` のまま、大きさと太さで区別している）。

### 2.4 コードブロック表示の現状

- 通常モード: `present_fenced_code_content_line`（`projection/mod.rs`）が `kind = BlockKind::CodeBlock` の行を作り、内容全体に `StyleKind::CodeBlock` を 1 本張る。結果は等幅＋ `code_block_background` の面。開きフェンスの記号と info string は別セグメントとして隠され、閉じフェンス行は畳まれる。
- **言語別ハイライトは無い**。`syntect` / `tree-sitter` はどの `Cargo.toml` にも無く、「highlight」の語は検索ハイライトと AI 選択（`line.rs`、`view.rs`、`view/document_find.rs`、`view/ai_selection.rs`）にしか出ない。info string は Mermaid 判定（`mermaid_fence`）以外に使われていない。
- プレーンモード（#737）: `kind = Unsupported` なので面も等幅も付かない。フェンス行も本文と同じ見た目になる。

### 2.5 データフロー

```text
RopeBuffer（唯一の正）
  └ Editor（ソースオフセットしか知らない）
      └ BlockIndex ─ IndexedBlock（kind, source_range, line_count, …）
          └ [ui] cached_block（view.rs L5195-5274）
              ├ 再利用: cached.rebase(deltas) && matches && covers(visible) && …
              └ 再提示: presented_block（line.rs L172-223）
                    └ block_context（L360-486）で行テキストを読む
                    └ [presentation] present_block_with_table_projection → VisualBlock { lines: Vec<VisualLine> }
              └ block_layout（L5278-5311）: 再利用時だけ layout_cache を使う。再提示なら必ず layout_block
                    └ WindowShaper（shape.rs）が style_runs から TextRun を作って折り返し・x 位置を出す
              └ row_element（line.rs）: セグメントごとに描画
```

キャッシュに関して本設計が依拠する事実:

- `cached_block` は、ブロック範囲に触れる編集があれば再利用しない（`edit_affects_range`、L5209-5211）。触れない編集では `rebase` して使い回す。
- `VisualLine::rebase` はソース範囲だけを付け替える。`style_runs` は行内の visual 範囲なので触らない。**ブロックのテキストが変わっていなければ `style_runs` はそのまま正しい**。
- `LayoutCacheEntry::is_valid`（L1567-1571）のキーは `(幅, font_revision, revision)`。`block_layout` は `reused == true` のときだけキャッシュを見る。再提示されたブロックは必ずレイアウトし直す。
- 巨大な joinable ブロック（`block_is_joinable`: Paragraph / Quote / List）には同期読みの予算がある: `JOIN_SYNC_LINE_BUDGET = 4_096` 行、`JOIN_SYNC_BYTE_BUDGET = 256 * 1024` バイト、判定は `block_fits_sync_join_budget(block, span)`（`line.rs` L111-118）。超えると提示窓の行だけを解析し、全体の解析は `schedule_joined_parse`（`view/background_parse.rs` L260〜）が背景で行って `joined_parse_cache` に置く。
- フェンスの開き行・閉じ行は、提示窓の外にあっても `block_context` が 1 行ずつ読む（L411-432）。開き行は `span.start + block.leading_content_lines`、閉じ行は「内容の最終行」。

---

## 3. 期待挙動の仕様

前提: プレーンテキストモード ON、`syntax_highlight = true`。「薄」= `Punctuation`、「構造」= `Structure`。色の値は 4 章。

### 3.1 対象要素（初回範囲）

| 要素 | 初回 | 備考 |
| --- | --- | --- |
| 見出し（ATX、Setext） | ○ | 本文と記号を別扱い |
| 強調 `**` `__` `*` `_`、打消し `~~` | ○ | |
| インラインコード | ○ | |
| リンク（インライン、オートリンク、裸 URL） | ○ | |
| 画像 `![alt](src)` | ○ | リンクと同じ規則 |
| フェンスコードブロック（記号、言語名、内容） | ○ | 言語別の色分けはしない |
| インデントコードブロック | ○ | 内容だけ |
| 引用（マーカーと本文） | ○ | |
| リストマーカー、タスクチェックボックス | ○ | |
| 表（`\|` と区切り行、ヘッダ） | ○ | |
| 水平線 | ○ | |
| HTML（ブロック、インライン） | ○ | 全体を薄くするだけ。タグ・属性の色分けはしない |
| 改行記号（行末 `\`） | ○ | `markers` に含まれるので自動的に薄くなる |
| 参照リンク `[text][ref]`、定義行 `[ref]: url` | × | ブロック単独では解決できない。本文色のまま |
| 脚注 | × | 解析されない |
| エスケープ `\*`、文字参照 `&amp;` | × | 本文色のまま |
| YAML フロントマター | × | 現行 parser が扱わない（未確認: `Rule` ＋段落として解析されるはず。その結果のまま色が付く） |

### 3.2 見出し

| 入力 | 期待結果 |
| --- | --- |
| `# 見出し` | `# `（記号＋後続空白）= 薄・通常の太さ。`見出し` = `Heading` 色・太字 |
| `## 見出し` | 同上（H2 も `Heading` 色） |
| `### 見出し` 〜 `###### 見出し` | 記号 = 薄。本文 = `HeadingMinor` 色・太字 |
| `# 見出し #`（閉じ記号つき） | 末尾の ` #` も薄 |
| `#`（本文なし）、`# `（空白だけ） | 全体が薄。太字なし |
| `#見出し`（空白なし） | 見出しではない（parser が段落とする）。本文色 |
| `####### x`（7 個） | 見出しではない。本文色 |
| `# **太字** と `code`` | `**` = 薄・太字、`太字` = `Heading` 色・太字、`code` = `Code` 色・太字、バッククォート = 薄・太字 |
| `# [リンク](url)` | `リンク` = `Link` 色・太字、`[` と `](url)` = 薄 |
| Setext: `見出し` / `===` | 1 行目 = `Heading` 色・太字。2 行目の下線 = 薄 |
| Setext: `見出し` / `---` | 1 行目 = `Heading` 色・太字（H2）。2 行目 = 薄 |
| 文字サイズ・行高 | **全レベルで本文と同じ**。段階差は色（2 段）と `#` の個数で示す |

### 3.3 インライン

| 入力 | 期待結果 |
| --- | --- |
| `**太字**`、`__太字__` | 全体が太字。開閉の記号 = 薄。内容は本文色 |
| `*斜体*`、`_斜体_` | 全体が斜体。記号 = 薄 |
| `***両方***` | 太字＋斜体。記号 = 薄 |
| `~~打消し~~` | 全体に取り消し線。記号 = 薄。内容は本文色 |
| `` `code` `` | 内容 = `Code` 色。バッククォート = 薄。**背景色・等幅は付けない** |
| ``` ``a`b`` ```（複数バッククォート） | 開閉の記号列全体 = 薄 |
| `**閉じていない` | 強調ではない（parser が Text とする）。本文色。入力途中で色が付いたり消えたりするのは仕様 |
| 行をまたぐ `**a` / `b**`（同じ段落） | 両方の行で太字。記号 = 薄 |
| `[文字](https://example.com)` | `文字` = `Link` 色。`[` と `](https://example.com)` = 薄。**下線は付けない**（クリックで開けないため。#737 でプレーン行は `links` が空） |
| `[文字](./relative.md)`、`[文字](mailto:a@b)` | 上と同じ（スキームによらない） |
| `<https://example.com>` | URL = `Link` 色。`<` `>` = 薄 |
| 裸 URL `https://example.com` | URL = `Link` 色 |
| `![alt](img.png)` | `alt` = `Link` 色。`![` と `](img.png)` = 薄 |
| `[文字][ref]`、`[ref]` | 定義が同じブロック内に無ければ本文色（3.5 節） |
| `<span>x</span>`（インライン HTML） | タグ部分 = 薄。`x` は本文色 |
| 行末 `\`（ハードブレーク） | `\` = 薄 |
| コードスパンの中の `**x**` | 強調にならない。全体が `Code` 色 |

### 3.4 ブロック

| 入力 | 期待結果 |
| --- | --- |
| ```` ```rust ```` （開き行） | インデント＋記号列 = 薄。`rust`（info string。行末まで）= 構造色 |
| フェンス内の行 | `Code` 色。内容中の `# x` や `**x**` は色分けしない |
| ```` ``` ````（閉じ行） | 薄 |
| 閉じていないフェンス | 開き行は上と同じ。以降ブロック末尾まで `Code` 色 |
| 開きと記号・長さが合わない閉じ風の行（例: 開き ```` ```` ```` に対する ```` ``` ````） | 閉じではない。`Code` 色（`fence_closes` の規則どおり） |
| `~~~` フェンス | バッククォートと同じ |
| インデントコードブロック（4 スペース） | 全行 `Code` 色 |
| `> 引用` | `> ` = 構造色。`引用` = `Quote` 色（= 既存の `quote_foreground`） |
| `> > 入れ子` | 各 `> ` = 構造色 |
| `> **太字**` | `**` = 薄、`太字` = `Quote` 色・太字 |
| 引用の遅延継続行（`>` なしで続く行） | `Quote` 色（parser が Quote の範囲に含める） |
| `- 項目`、`* 項目`、`+ 項目` | 記号＋区切り 1 文字 = 構造色。本文は本文色 |
| `1. 項目`、`1) 項目` | `1. ` = 構造色 |
| `- [ ] 未了`、`- [x] 済` | `- ` と `[ ]` / `[x]` = 構造色。本文は本文色（済でも打消しにしない） |
| リスト項目の継続行・入れ子 | 行頭のインデントは空白なので見た目に影響なし |
| `---`、`***`、`___`（水平線） | 行全体 = 薄 |
| 表のヘッダ行 `\| a \| b \|` | `\|` = 薄。セルの内容 = 太字・本文色 |
| 表の区切り行 `\| --- \| :-: \|` | 行全体 = 薄 |
| 表の本体行 | `\|` = 薄。内容は本文色。セル内のインライン要素は 3.3 節どおり |
| セル内の `\|`（エスケープ） | 本文色（セルの内容） |
| HTML ブロック（`<div>` … `</div>`、`<!-- … -->`） | ブロック全体 = 薄 |
| 空行 | ランなし |

### 3.5 境界ケース

| 入力・状態 | 期待結果 |
| --- | --- |
| 空文書、空行だけの文書 | ランなし。panic しない |
| CRLF 文書 | 行末の `\r\n` は `visual_text` から除かれている。ランが行末を越えない（行の内容範囲で切る） |
| 行頭・行末に全角文字、絵文字、結合文字 | ランの端点は必ず UTF-8 の文字境界（ノード範囲と行範囲はどちらも parser / RopeBuffer 由来の境界）。`debug_assert!(visual_text.is_char_boundary(..))` |
| 複数行にまたがるノード | 行ごとに切って出す。提示窓の外の行には出さない |
| ブロック末尾の空行（ブロックが所有する trailing blank） | ランなし |
| 参照リンクの定義が別ブロックにある | リンクとして色付けしない。**ブロックの外の編集で色が変わることは無い** |
| 入力途中（`**a` → `**a*` → `**a**`） | 1 打鍵ごとにそのブロックを再解析し、その時点の解析結果で色が決まる。キャレット位置は動かない |
| IME 合成中 | 合成中の文字列は文書に入っているので解析対象になる。合成範囲の下線（`marked`）は既存どおり重ねて描く。ハイライトは合成範囲を特別扱いしない |
| 巨大ブロック（予算超過。4,096 行超または 256 KiB 超）の joinable ブロック | P1: 提示窓の行だけを単独で解析する（縮退）。窓の外から始まる構造（複数行の強調、深い入れ子）は色が不正確になりうる。本文・キャレットへの影響は無い。P3: 背景解析の結果が届けば正確になる |
| 巨大なコードブロック | 予算に関係なく正確（解析しない。5.4 節） |
| 巨大な表・HTML ブロック（予算超過） | 提示窓の行だけを単独で解析（縮退）。表は行単独では表にならないので `\|` が薄くならない。許容する |
| `IndexedBlock.confidence` が暫定のブロック | その時点の `kind` と解析結果で色付けする。正式な索引が届いてブロックが変われば `matches_plain` が外れて再提示される |
| ハイライト OFF | `style_runs` は空。#737 の出力と完全に同じ |
| 通常（投影）モード | 設定値によらず現行どおり。`StyleKind::Syntax` は 1 つも出ない |
| プレーン + Reading（#737 U-8） | 編集可のときと同じ色付け |
| 選択範囲と重なる | 文字色はそのまま。背景が `selection_background` になる |
| 検索一致と重なる（選択中でない） | **そのセグメントは色付けをやめて本文色に戻す**（太字・斜体・取り消し線は残す）。検索背景上のコントラストを現行と同じにするため（4.3 節） |
| ズーム変更 | 色・太さは同じ。幾何は既存どおり `font_revision` で作り直される |
| テーマ切替 | 次の描画で新テーマの色になる。提示のやり直しは不要（色は描画時に解決）。既存の `set_theme` はキャッシュを消すので結果は同じ |

### 3.6 通常（投影）モードには適用しない

理由:

1. **同じ要素をすでに別の手段で表現している**。見出し = 大きさ＋ Semibold、強調 = 太字 / 斜体、リンク = 青＋下線、インラインコード = 背景＋等幅、コードブロック = 面＋等幅、引用 = `BlockTint::Muted` ＋バー、表 = グリッド、リスト記号 = グリフ置換。色を足すと二重表現になる。
2. **記号は隠れている**。「記号を薄く」の対象が、開示中の行にしか存在しない。開示中だけ色が付くと、キャレットを動かすたびに色が出入りする。
3. **`BlockTint` と競合する**。引用行は行全体に `quote_foreground` を当てており、セグメント単位の色と優先順位の整理が要る。
4. **規則の二重実装になる**。AGENTS.md の方針（新旧両方へ同じルールを実装しない）に照らし、投影経路の `present_markdown_from_parse`（L1226-1265）に同じ分類を足すのは避ける。

「開示中の記号だけ薄くする」は小さく切り出せる follow-up 候補（9 章 U-6）。その場合も本設計の `SyntaxTone::Punctuation` と描画側の解決をそのまま使える。

---

## 4. 色の割り当て

### 4.1 役割と色

`SyntaxTone` は色ではなく役割。色は `ui` が `Theme` から引く。

| 役割（`SyntaxTone`） | 対象 | トークン | ライト | ダーク | 併用する属性 |
| --- | --- | --- | --- | --- | --- |
| `Heading` | H1・H2 の本文 | **新** `syntax_heading` | `0x9a3f1c` | `0xf0a36b` | 太字 |
| `HeadingMinor` | H3〜H6 の本文 | **新** `syntax_heading_minor` | `0x7a4a2e` | `0xd9b08c` | 太字 |
| `Code` | インラインコード、コードブロックの内容 | **新** `syntax_code` | `0x2a6b57` | `0x8fcbb4` | なし（バッククォート・フェンスが見えている） |
| `Link` | リンク文字、URL、画像の alt | 既存 `link_foreground` | `0x2867a9` | `0x79b8ff` | なし（`[` `]` が見えている） |
| `Structure` | リスト記号、タスクの `[ ]`、引用の `>`、フェンスの言語名 | **新** `syntax_structure` | `0x9a3f1c` | `0xf0a36b` | なし |
| `Punctuation` | `#`、強調・コードの開閉記号、フェンス記号、リンクの括弧と宛先、表の `\|` と区切り行、水平線、HTML、Setext 下線 | 既存 `quote_foreground` | `0x6b6259` | `0xaaa39a` | なし |
| `Quote` | 引用の本文 | 既存 `quote_foreground` | `0x6b6259` | `0xaaa39a` | なし（`>` が構造色で見えている） |
| （役割なし） | 強調・打消しの内容、表ヘッダ、段落 | `foreground`（継承） | `0x262626` | `0xe8e5df` | 太字 / 斜体 / 取り消し線 |

- `syntax_structure` は `syntax_heading` と同じ値で始める。役割が別なのでトークンは分ける（後で片方だけ調整できる）。
- `Punctuation` と `Quote` は既存の `quote_foreground` を引く。このトークンはすでに「薄い文字色」として引用・水平線・画像 alt などに共用されており、同じ意味での再利用になる。新しい灰色を増やさない。
- `Link` は既存の `link_foreground` を引く。通常モードのリンクと同じ色なので、モード間で「青 = リンク」が一致する。`link_hover_foreground` は使わない（プレーンではホバーで開けない）。

### 4.2 設計の根拠

- **見出しは錆色＋太字**。既存パレットは暖色の無彩色で、アクセントは青（リンク、アクティブタブ）。見出しを青にするとリンクと区別できない。暖色側のアクセントとして錆色を選んだ。赤（エラーを連想）や黄（検索背景と近い）は避けた。
- **見出しの段階差は 2 段**。文字サイズを変えられない条件で、色だけで 6 段を作ると隣同士が判別できず、薄い側はコントラストが足りなくなる。H1・H2 を強い錆色、H3 以下を彩度を落とした茶にし、正確なレベルは `#` の個数で読む。
- **`#` は本文より薄くする**。記号はすべて同じ規則（薄・通常の太さ）にそろえる。見出し本文だけが色＋太字で立つ。
- **リスト記号と引用記号は薄くせず構造色にする**。これらは行の種類を決める記号で、薄くすると箇条書きの始まりが見つけにくくなる。見出しと同じ色相にして色数を増やさない（見出しは太字なので区別できる）。
- **コードは緑、背景なし**。青（リンク）・錆（見出し）と色相が離れている。インラインコードの背景（`code_background`）はプレーンでは付けない。背景は選択・検索の背景と重なったときの扱い（`segment_shows_code_background`）が増えるうえ、バッククォートが見えているので不要。
- **引用本文は通常モードと同じ薄色**。モード間で「引用 = 薄い」が一致する。

### 4.3 コントラスト（WCAG 2.x の相対輝度比。本設計のために計算した値）

| 色 | ライト: 本文背景 | ライト: 選択 | ダーク: 本文背景 | ダーク: 選択 |
| --- | --- | --- | --- | --- |
| `foreground`（参考） | 14.38 | 13.02 | 12.97 | 7.90 |
| `quote_foreground`（既存） | 5.68 | 5.14 | 6.53 | 3.98 |
| `link_foreground`（既存） | 5.54 | 5.01 | 7.85 | 4.78 |
| `syntax_heading` / `syntax_structure` | 6.43 | 5.82 | 7.89 | 4.80 |
| `syntax_heading_minor` | 7.01 | 6.35 | 8.18 | 4.99 |
| `syntax_code` | 5.98 | 5.41 | 8.83 | 5.38 |

- 新規トークンはすべて本文背景で 4.5 以上、選択背景でも 4.5 以上。
- 選択背景上の最小値は既存の `quote_foreground`（ダーク 3.98）。これは通常モードの引用を選択したときと同じ値で、本 Issue で悪化させるものではない。
- 検索背景の上では足りない。例: ダークの `find_current_match_background`（`0xcf7f2e`）に対し `foreground` でも 2.48、新規色は 1.5〜1.7。ライトの現在位置（`0xffa94d`）でも新規色は 3.1〜3.9。このため **検索背景が描かれるセグメントでは色付けをやめて `foreground` に戻す**（3.5 節）。これで検索一致の見た目は現行と完全に同じになる。

### 4.4 役割の優先順位

同じ文字に複数の役割が重なったとき（例: 見出しの中のコード）は、内側・具体的な方を採る。

```text
Structure(6) > Punctuation(5) > Code(4) > Link(3) > Heading = HeadingMinor(2) > Quote(1)
```

- `Structure > Punctuation`: 引用の `> ` は `markers`（薄）と `quote_markers`（構造色）の両方に入るので、構造色を勝たせる。
- `Punctuation > Code > Link`: コードやリンクの中の開閉記号は薄くする。リンク文字の中のコードはコード色。
- `Heading` と `HeadingMinor` は同じノードに同時には付かない。
- 太字・斜体・取り消し線は役割ではなくフラグなので、重なれば全部付く（既存の `union` どおり）。

---

## 5. 実装方針

### 5.1 全体像

```text
[ui] cached_block
  └ presented_block(policy = Plain, syntax_highlight = settings.syntax_highlight)
      ├ （#737）提示窓の行を読む → BlockWindow
      ├ （本 Issue）ハイライト入力を作る → PlainHighlightInput        … 5.4
      └ [presentation] present_block_plain(block, window, revision, line_height, highlight)
            ├ （#737）行ごとに present_raw_source → VisualLine（恒等写像）
            └ （本 Issue）plain_highlight(block, input) → PlainHighlight（ソース範囲つきのラン列）
                          行ごとに style_runs_for_line → VisualLine.style_runs  … 5.3
  └ block_layout → layout_block → WindowShaper::runs（太字・斜体を幾何に反映。変更なし）
  └ row_element → セグメントの InlineDisplay.tone → Theme の色                  … 5.5
```

Editor、DocumentSession、保存、SourceMap、`visual_text` には触れない。追加するのは `style_runs` の中身と、それを色に変える描画側の 1 か所だけ。

### 5.2 変更ファイル一覧

#### (1) `crates/presentation/src/display.rs`

```rust
/// プレーン表示での構文の役割。色ではない（`BlockTint` と同じ考え方で、色は UI がテーマから引く）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxTone {
    Quote,
    Heading,
    HeadingMinor,
    Link,
    Code,
    Punctuation,
    Structure,
}

impl SyntaxTone {
    /// 重なったときに勝つ順。大きい方を採る（4.4 節）。
    pub const fn rank(self) -> u8 {
        match self {
            Self::Quote => 1,
            Self::Heading | Self::HeadingMinor => 2,
            Self::Link => 3,
            Self::Code => 4,
            Self::Punctuation => 5,
            Self::Structure => 6,
        }
    }
}
```

- `StyleKind` に `Syntax(SyntaxTone)` を追加する。`StyleKind` は `Copy + Eq` のままでよい。
- `InlineDisplay` に `pub tone: Option<SyntaxTone>` を追加する。`Default` は `None`。
- `InlineDisplay::union`（`const fn`）: `tone` は `rank` の大きい方。`const fn` なので `match (self.tone, other.tone)` で書く（`Option::max` や `PartialOrd` は使わない）。同 rank のときは `self` 側を採る。
- `StyleKind::display()`: `base` に `tone: None` を足し、`Self::Syntax(tone) => InlineDisplay { tone: Some(tone), ..base }` を追加する。**`Syntax` は `tone` 以外のフラグを一切立てない**（等幅・背景・下線なし）。
- `StyleKind` を網羅 match している箇所はコンパイルエラーで分かる。`_ =>` で受けている箇所の有無は `grep -rn "StyleKind::" crates/` で確認する（未確認）。
- `syntax_display(NodeKind)` は変更しない。これは投影モード用の継ぎ目で、プレーンの分類は (2) に閉じる。

#### (2) `crates/presentation/src/projection/highlight.rs`（新規）

プレーン表示用の分類。入力は解析結果、出力はソース範囲つきのラン列。

```rust
/// 1 ブロック分の色付け。ソース範囲で持ち、行ごとに切り出す。
pub struct PlainHighlight {
    /// 開始位置でソート済み。重なりあり（重なりは InlineDisplay::union が解決する）。
    runs: Vec<(SourceRange, StyleKind)>,
}

/// present_block_plain に渡す色付けの入力。
pub enum PlainHighlightInput<'a> {
    /// `lines` を連結して解析する。ブロックの内容行すべて（予算内）か、
    /// 提示窓の行だけ（予算超過時の縮退）。
    Parse { lines: &'a [BlockLine<'a>] },
    /// 背景で済ませたブロック全体の解析（フェーズ P3）。
    Joined(&'a JoinedParse),
    /// トップレベルのコードブロック。解析しない。
    Code(PlainCodeBlock),
}

/// トップレベルのコードブロックの形。行番号は文書の行番号（`BlockLine::line` と同じ座標）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlainCodeBlock {
    /// フェンスの開き行。インデントコードブロックなら None。
    pub opening: Option<(usize, FenceDelimiter)>,
    /// 開きを閉じる行（`fence_closing_delimiter` かつ `fence_closes`）。無ければ None。
    pub closing_line: Option<usize>,
    /// 内容の終端（排他的）。これ以降はブロックが所有する空行。
    pub content_end: usize,
}

impl PlainHighlight {
    pub fn from_parse(parsed: &MarkdownParse) -> Self;
    /// `line` の内容範囲（行末の改行を除く）に重なるランを、行内の visual 範囲にして返す。
    pub fn style_runs_for_line(&self, content: SourceRange) -> Vec<StyleRun>;
}

/// コードブロック 1 行分。解析結果を使わない。
pub fn plain_code_line_runs(code: &PlainCodeBlock, line: usize, text: &str) -> Vec<StyleRun>;
```

**`from_parse` の規則**（`parsed.tree.iter()` を 1 回なめ、サイドテーブルを足す）:

| 対象 | 出すラン |
| --- | --- |
| `Heading(n)` | 子の範囲（先頭の子の `start` 〜 最後の子の `end`）に `Syntax(Heading)`（n ≤ 2）または `Syntax(HeadingMinor)`（n ≥ 3）と `Bold`。ノード範囲のうち子の範囲の外側（前と後ろ。行末の改行は除く）に `Syntax(Punctuation)`。子が無ければノード全体が `Punctuation` |
| `Strong` | ノード範囲に `Bold` |
| `Emphasis` | ノード範囲に `Italic` |
| `Strikethrough` | ノード範囲に `Strikethrough` |
| `InlineCode` | ノード範囲に `Syntax(Code)` |
| `CodeBlock`（解析したブロックの中に現れるもの = リストや引用に入れ子のコード） | ノード範囲に `Syntax(Code)`。開き行のうち `fence_marker_edges` の `Opening` の終端から行末までの非空白部分（info string）に `Syntax(Structure)` |
| `Link`、`Image` | 子があれば、子の範囲に `Syntax(Link)`、ノード範囲のうち子の範囲の外側（前と後ろ）に `Syntax(Punctuation)`。子が無ければノード全体が `Punctuation` |
| `Quote` | ノード範囲に `Syntax(Quote)` |
| `Rule`、`HtmlBlock`、`Html`、`InlineHtml` | ノード範囲に `Syntax(Punctuation)` |
| `Table` | ノード範囲のうち、どの `TableCell` にも入らない部分に `Syntax(Punctuation)`（`\|`、区切り行、行頭末の空白）。`TableHead` の子孫の `TableCell` 範囲に `Bold` |
| `parsed.markers`（全件） | `Syntax(Punctuation)` |
| `parsed.quote_markers`、`parsed.list_item_markers` | `Syntax(Structure)` |
| `parsed.task_markers` の `marker_range` | `Syntax(Structure)` |
| `parsed.links` のうち、所有ノードが `Text` のもの（裸 URL） | `text_range` に `Syntax(Link)` |

- ラン範囲が空のものは出さない。最後に開始位置でソートする。
- `Text`、`Paragraph`、`List`、`ListItem`、`TableRow`、`SoftBreak`、`HardBreak`（記号は `markers` 側）、`TaskMarker`（`task_markers` 側）、`Unsupported`、`FootnoteDefinition`、`FootnoteReference` は何も出さない。
- 規則は「ノード種別 → ラン」の対応だけで、文字列の再走査をしない（表の「セル外」と info string の空白除去を除く）。区切り記号の位置は parser 側（`derive_markers`）が正で、ここで数え直さない。
- `Strong` などのランは区切り記号を含むノード全体に掛ける。投影モードの開示時（`**` も太字）と同じ。
- 複数行にまたがる `Strong` / `Quote` などのランは、継続行の行頭プレフィックス（`> ` やインデント）にも掛かる。プレフィックスは `Structure` が rank で勝つので色は正しい。太字がプレフィックスに掛かる点は許容する。

**`style_runs_for_line`**: 開始位置でソート済みの `runs` から、`content` と重なるものを取り、`max(run.start, content.start) - content.start .. min(run.end, content.end) - content.start` を `VisualRange` にする。プレーン行は恒等写像（#737 の不変条件 3）なので引き算で足りる。重なりの探索は、終端が単調でないため「`start < content.end` の範囲を二分探索で切り、その中で `end > content.start` を線形に見る」でよい。ブロック全体では O(ラン数 × 最大ネスト深さ + 行数 log ラン数) 程度。巨大ブロックで全ランを毎行なめないこと。

**`plain_code_line_runs`**:

| 行 | 出すラン |
| --- | --- |
| `line >= content_end`（所有する空行） | なし |
| `opening` の行 | 先頭の空白＋記号列（`text` の先頭空白数 + `FenceDelimiter::len`）に `Syntax(Punctuation)`。残りの前後の空白を除いた部分（info string）があれば `Syntax(Structure)` |
| `closing_line` の行 | 行全体に `Syntax(Punctuation)` |
| それ以外 | 行全体に `Syntax(Code)` |

`text` は行末の改行を除いた内容で受ける。

**テーブルのセル範囲について（未確認）**: pulldown-cmark が `TableCell` のイベント範囲に前後の `|` や空白を含めるかを確認していない。含めない前提で上の規則を書いた。7.1 節の `table_cell_ranges_exclude_pipes` で先に固定し、もし `|` を含むなら「`TableHead` / `TableRow` の範囲内で、`InlineCode` ノードの外にあり、直前が `\` でない `|` を `Punctuation` にする」字句規則に切り替える。

#### (3) `crates/presentation/src/projection/block.rs`

#737 の `present_block_plain` に引数を 1 つ足す。

```rust
pub fn present_block_plain(
    block: &IndexedBlock,
    window: &BlockWindow<'_>,
    revision: Revision,
    line_height: f32,
    highlight: Option<&PlainHighlightInput<'_>>,   // 追加。None なら #737 と同じ出力
) -> VisualBlock
```

- 先頭 3 引数と `line_height` の正確な型・順序は #737 の実装に合わせる。
- 処理:
  1. `highlight` から 1 回だけ `PlainHighlight` を作る。`Parse { lines }` → `parse_joined_block(lines, revision).parsed` を `from_parse` に渡す（`parse_joined_block` は `mod.rs` L2205、公開済み。`lines` が空なら解析しない）。`Joined(j)` → `from_parse(&j.parsed)`。`Code(_)` → 作らない。
  2. #737 どおり各行を `present_raw_source` で提示し、行末の `\r` `\n` を pop する。
  3. **pop の後で**、行の内容範囲 `SourceRange::new(line.range.start, line.range.start + visual_text.len())` を使って `style_runs` を入れる。`Code` のときは `plain_code_line_runs(code, line.line, &visual_text)`。
- `kind`、`estimated_height`、`source_map`、`links`、`list`、`quote` などには触れない。
- `parse_joined_block` は `ProjectionIndex` も作る（使わない）。その分の無駄を避けたい場合は、同じモジュール内に `parse_document` を直接呼ぶ小さな関数を置く。どちらでも結果は同じ。まず `parse_joined_block` を使い、7.4 節の測定で問題が出たら置き換える。

#### (4) `crates/presentation/src/lib.rs`

`SyntaxTone`、`PlainHighlight`、`PlainHighlightInput`、`PlainCodeBlock`、`plain_code_line_runs` を `pub use` に追加する。

#### (5) `crates/ui/src/line.rs`

- `BlockPresentationRequest`（L132-143）に `pub(crate) syntax_highlight: bool` を追加する。既存の構築箇所（`view.rs` の `cached_block`、テスト、`shape.rs` のテストなど）はコンパイルエラーで分かる。投影モードでは値を読まない。
- `presented_block` の `Plain` 分岐（#737 で新設）で、`syntax_highlight` が `true` のときだけ入力を作る（5.4 節）。作った入力を `present_block_plain` に渡す。
- 色の解決を 1 関数にまとめる（`link_text_color` L1419 の隣）:

```rust
/// 構文の役割をテーマの色にする。役割 → 色の対応はここだけに置く。
pub(crate) const fn syntax_tone_color(theme: &Theme, tone: SyntaxTone) -> u32 {
    match tone {
        SyntaxTone::Heading => theme.syntax_heading,
        SyntaxTone::HeadingMinor => theme.syntax_heading_minor,
        SyntaxTone::Code => theme.syntax_code,
        SyntaxTone::Structure => theme.syntax_structure,
        SyntaxTone::Link => theme.link_foreground,
        SyntaxTone::Punctuation | SyntaxTone::Quote => theme.quote_foreground,
    }
}

/// 検索の背景が実際に描かれるセグメントでは色付けしない（4.3 節）。
/// `segment_shows_code_background` と同じ形の述語。
fn segment_syntax_tone(segment: &LineSegment) -> Option<SyntaxTone> {
    if !segment.selected && segment.find_match { None } else { segment.display.tone }
}
```

- `row_element` のセグメント描画（L1000 付近）で、`link_color` の `.when(...)` の**前**に `.when_some(segment_syntax_tone(segment), |element, tone| element.text_color(rgb(syntax_tone_color(theme, tone))))` を足す。プレーン行では `link_color` は立たないので順序は結果に影響しないが、将来両方が立ったときにリンク色（ホバーを含む）が勝つ順にしておく。
- `table_row_element`（L1081〜）と `table_fragment_needs_segments`（L1466）は変更しない。プレーン行は `kind = Unsupported` で `table_row` を持たないので通らない。
- `line_segments_with_hover`、`inline_display_for` は変更不要（`style_runs` の端点で分割し、`InlineDisplay` を union する既存処理に乗る）。

#### (6) `crates/ui/src/shape.rs`

**変更しない**。`WindowShaper::runs` は `style_runs` から `bold` / `italic` を読んで `TextRun` を作る（L63-90）。`tone` は読まない（色は幾何に入れない）。`font_revision` も変更しない。

`Syntax` ランの端点でも `TextRun` が分割される（L53-59 は全ランの端点を境界にする）。同じフォントの `TextRun` を分割したときに字送りが変わらないかは**未確認**（GPUI の `shape_line` の実装を読んでいない）。変わるとしても、幾何（`shape.rs`）と描画（`row_element`。セグメントごとの `div`）は同じ端点で分割するので両者はずれない。7.2 節の `plain_highlight_does_not_move_color_only_glyphs` で、色だけのランが x 位置を動かさないことを確かめる。動く場合の対処は 6 章 R4。

#### (7) `crates/ui/src/theme.rs`

`Theme` にフィールドを 4 つ追加する（`quote_foreground` の後ろ）。

```rust
/// プレーン表示の構文色（Issue #738）。H1・H2 の本文。
pub syntax_heading: u32,
/// H3〜H6 の本文。
pub syntax_heading_minor: u32,
/// インラインコードとコードブロックの内容。
pub syntax_code: u32,
/// リスト記号、引用記号、タスクのチェックボックス、フェンスの言語名。
pub syntax_structure: u32,
```

| | `DEFAULT_THEME` | `DARK_THEME` |
| --- | --- | --- |
| `syntax_heading` | `0x9a3f1c` | `0xf0a36b` |
| `syntax_heading_minor` | `0x7a4a2e` | `0xd9b08c` |
| `syntax_code` | `0x2a6b57` | `0x8fcbb4` |
| `syntax_structure` | `0x9a3f1c` | `0xf0a36b` |

`Theme` を構造体リテラルで作っている箇所が 2 定数以外にあるかは `grep -rn "Theme {" crates/ui/src` で確認する（未確認）。

#### (8) `crates/session/src/store.rs`

- `Settings`（L52-78）に `pub syntax_highlight: bool` を追加。`Default` は `true`。
- `FileStateStore::store` の `format!`（L470 付近）に `syntax_highlight={}\n` を追加。
- `settings_from_read`（L495-527）に `strip_prefix("syntax_highlight=")` を追加。`settings.syntax_highlight = value != "false";`（`autosave` と同じ形。欠損・不明値は `true`）。
- 読めない設定ファイルのとき（`Err` 分岐）は `Default` の `true` のままでよい。色付けは外部と通信せず本文も変えないので、`auto_update_check` のように倒す必要が無い。

#### (9) `crates/ui/src/view/plain_text_mode.rs`（#737 で新設されるファイル）

```rust
impl EditorView {
    pub(super) fn syntax_highlight(&self) -> bool { self.settings.syntax_highlight }

    /// `set_plain_text` と同じ形。戻り値は「要求した状態になったか」。
    pub(super) fn set_syntax_highlight(&mut self, on: bool, cx: &mut Context<Self>) -> bool {
        if self.syntax_highlight() == on { return true; }
        if self.editor_mode_switch_blocked() {
            // 既存と同じ status 文言
            return false;
        }
        self.settings.syntax_highlight = on;
        if self.plain_text() {
            self.cancel_text_selection_autoscroll();
            self.rebuild_presentation_for_mode_change();
        }
        self.store_settings();
        cx.notify();
        true
    }
}
```

- 状態の置き場所は `self.settings.syntax_highlight` の 1 か所。
- プレーン表示中の切替は `rebuild_presentation_for_mode_change()`（`reading_mode.rs` L154-187）を通す。太字の有無で折り返し位置が変わりうるので、`block_cache` / `layout_cache` の破棄、実測高さの破棄、`ModeAnchor` によるスクロール位置の保持が要る。これは表示モード切替とまったく同じ要件で、同じ関数で足りる。`mode_generation` も加算されるので、切替前に始まった背景ジョブの結果は捨てられる。
- **`matches_plain` に世代番号は足さない**。#737 設計書 9 章は「キャッシュ一致条件にハイライト設定の世代が要る」としていたが、設定の変更点はこの関数 1 か所で、そこで `block_cache` を空にする。テーマ（`set_theme`）と同じ扱いで足りる。
- 投影モード中の切替は設定値を変えて保存するだけ。表示は変わらないのでキャッシュを触らない。
- IME 合成中は拒否する（`editor_mode_switch_blocked()`、`reading_mode.rs` L110）。プレーン表示中は折り返しが変わりうるため。投影モード中も同じ述語で拒否してよい（分岐を増やさない）。
- 代入と `rebuild…` の順序は `set_plain_text` と同じにする（#737 設計書 4.2 (5) の未確認事項の結論に従う）。

#### (10) `crates/ui/src/view.rs`

- `cached_block`（L5195-5274）: `BlockPresentationRequest` に `syntax_highlight: self.settings.syntax_highlight` を渡す。`joined` は既存どおり `self.current_joined_parse(block)`（P3 で使う。P1 では `Plain` 分岐が読まない）。
- 再利用条件は #737 のまま（`matches_plain` と `covers`）。追加の条件は無い。根拠は 5.6 節。

#### (11) `crates/ui/src/view/settings_screen.rs`

- `display_and_save_settings`（L586）の「表示」カードに Checkbox `"settings-syntax-highlight"` を追加する。ラベル案: 「プレーンテキスト表示で Markdown 記法を色分けする」。ハンドラは `set_syntax_highlight` を呼ぶだけ。
- チェック状態は `self.syntax_highlight()` から描く（拒否されたときに表示がずれない）。
- プレーンモード OFF のときも操作可能にする（独立した設定。グレーアウトしない）。

#### (12) `crates/ui/src/view/background_parse.rs`（フェーズ P3 のみ）

- `schedule_joined_parse`（L260〜）は #737 で「`Plain` では起動しない」。P3 で「`Plain` かつ `syntax_highlight()` のときは起動する」に変える。ジョブの中身（`parse_joined_span`）とキャッシュ（`joined_parse_cache`）、完了時の `block_cache.remove(&id)` は既存のまま使える。
- 対象は既存どおり `block_is_joinable` のブロックだけ。表・HTML の巨大ブロックは縮退のまま。

### 5.3 分類 → スタイルのデータフロー

```text
ブロックのソース行（&str）
  │ parse_document（既存。pulldown-cmark）
  ▼
MarkdownParse { tree, markers, quote_markers, list_item_markers, task_markers, fence_marker_edges, links }
  │ PlainHighlight::from_parse（新規。ノード種別 → ランの対応表）
  ▼
Vec<(SourceRange, StyleKind)>           … Bold / Italic / Strikethrough / Syntax(tone)
  │ style_runs_for_line（行の内容範囲で切る。恒等写像なので引き算）
  ▼
VisualLine.style_runs: Vec<StyleRun>
  ├─▶ shape.rs  WindowShaper::runs      … bold / italic だけを読む（幾何）
  └─▶ line.rs   line_segments_with_hover → inline_display_for → InlineDisplay { bold, italic, strikethrough, tone }
                  └ row_element          … tone → syntax_tone_color(theme, tone) → text_color
```

`presentation` は `Theme` を知らない。`ui` は `StyleKind` を match しない（既存の境界を守る）。

### 5.4 ハイライト入力の作り方（`ui` 側）

`presented_block` の `Plain` 分岐で、次の順に決める。

| 条件 | 入力 |
| --- | --- |
| `!syntax_highlight` | `None` |
| `block.kind == NodeKind::CodeBlock`（トップレベルのコードブロック） | `Code(PlainCodeBlock)`。**解析しない** |
| （P3）`joined` がある | `Joined(joined)` |
| `block_fits_sync_join_budget(block, &span)` | `Parse { lines: 内容行すべて }` |
| 上記以外（予算超過） | `Parse { lines: 提示窓の行 }`（縮退） |

- 「内容行すべて」= `span.start .. span.end - trailing_blank_lines(document, &span)`。行の範囲は `document.line_range(LineId(n))` をブロックの `source_range` で切ったもの、テキストは `document.text(range)`。`block_context`（L360-394）の `clip_to_block` と同じ読み方をする。提示窓がすでに内容行すべてを覆っているなら窓の行をそのまま使い、読み直さない。
- 予算判定は既存の `block_fits_sync_join_budget` をブロック種別によらず使う（投影モードは joinable だけに使っているが、関数自体は種別を見ない）。
- `PlainCodeBlock` の作り方は `block_context` の L411-432 と同じ手順:
  - 開き行の番号 = `span.start + block.leading_content_lines`（`span.end` 未満のとき）。その行のテキストを読み、`fence_delimiter(text)` が `Some` なら `opening = Some((行番号, delimiter))`、`None` ならインデントコードブロックで `opening = None`。
  - `content_end = span.end - trailing_blank_lines`。
  - 閉じ行の候補 = `content_end - 1`（開き行と異なり、`span` 内のとき）。そのテキストについて `fence_closing_delimiter(text)` が `Some(c)` かつ `fence_closes(opening_delimiter, c)` なら `closing_line = Some(候補)`。
  - 読むのは最大 2 行。ブロックの大きさによらない。
- `BlockLine` の `disclosure` フィールドは、#737 がプレーン行に入れる値と同じにする（解析では読まれない）。

### 5.5 編集・保存・SourceMap に影響しないことの担保

| 経路 | 担保 |
| --- | --- |
| 本文 | 追加コードは `&Editor` / `&RopeBuffer` の読み取りだけ。`presentation` の新規関数は `&str` と `&MarkdownParse` しか受けない。書き込み API を呼ぶ箇所が無い |
| 保存 | `session` の変更は `Settings` の 1 フィールドと `settings.conf` の 1 行だけ。文書の保存経路・autosave・ドラフトには触れない |
| `visual_text` / SourceMap | `present_raw_source` の出力を変えない。`style_runs` は提示後に代入するだけ。テスト `plain_highlight_changes_only_style_runs` で、ON / OFF の `VisualLine` が `style_runs` 以外すべて等しいことを固定する |
| キャレットの x 位置・クリック位置 | 太字・斜体は `style_runs` 経由でしか入らない。`shape.rs`（`x_for_offset` / `offset_for_x` / `wrap_boundaries`）と `row_element` は同じ `style_runs` を読む。行に直接 `font_weight` を当てる経路（`styled_block_with_surface` の `BlockDisplay`）は使わない（`kind = Unsupported` のまま） |
| キャレットのソースオフセット | 設定の切替は Editor に触れない。`rebuild_presentation_for_mode_change` は表示キャッシュと高さだけを作り直す（#737 と同じ） |
| 行高・ブロック高さ | `estimated_height` と `kind` を変えない。`plain_block_height` は行数 × `line_height` のまま。太字で折り返しが増えた分は既存の実測経路が反映する（#737 4.3 節の 3 つ目の不変条件） |
| 解析の失敗 | `parse_document` は任意の UTF-8 を受ける。ランの範囲は必ず行の内容範囲で切る。範囲外・逆転したランは捨てる（`debug_assert!` ＋リリースでは skip） |

**#737 設計書 9 章「色と太さ（字幅が変わらない範囲）に限る」の解釈**: 太字・斜体はプロポーショナルフォントで字幅を変える。本設計はこれを許す。#737 4.3 節の不変条件が要求しているのは「見積り高さ = 行数 × `line_height`」「提示した高さ = `plain_block_height`」「折り返しは実測に任せる」の 3 つで、いずれも太字・斜体で崩れない（同じフォントサイズなので 1 行の高さは変わらず、折り返しの増減は長い行と同じく実測される）。崩れるのは文字サイズ（`font_scale`）、行高、`BlockKind` を変えた場合で、本設計はこれらを変えない。等幅ファミリへの切替もしない。

### 5.6 再計算の範囲とキャッシュ

**単位はブロック。新しいキャッシュは作らない。**

| 出来事 | 再計算されるもの |
| --- | --- |
| スクロール（表示中のブロックが提示済みの行で覆われている） | なし。`block_cache` の `VisualBlock`（`style_runs` 込み）を再利用 |
| スクロール（新しいブロックが見えた） | そのブロックを 1 回解析 |
| スクロール（巨大ブロックの別の部分が見えた。`covers` が外れる） | そのブロックを再解析（予算内なら全体、超過なら窓だけ）。投影モードが同じ条件で行う解析と同じ量 |
| ブロック内の編集 | そのブロックだけ再解析・再レイアウト（`edit_affects_range` で再利用が外れる） |
| ブロック外の編集 | なし。`rebase` で再利用。`style_runs` は行内 visual 範囲なのでそのまま正しい |
| キャレット移動・選択 | なし（プレーンには開示が無い。`disclosures_are_current` は #737 で常に `true`） |
| テーマ切替 | 提示はやり直すが（既存の `set_theme` がキャッシュを消す）、色は描画時に引くのでどちらでも正しい |
| ハイライト設定の切替 | プレーン表示中は全キャッシュ破棄（`rebuild_presentation_for_mode_change`） |

**ブロック外の編集で再利用してよい根拠**: ブロックの色付けは「そのブロックのテキスト」と「ブロックの種別」だけの関数である。唯一の文書横断の依存になりうる参照リンクの定義解決を、初回範囲から外している（3.1 節）。投影モードが `uses_reference` や `has_reference_sensitive_bare_link` で再提示を強制している事情（L5212-5226）は、プレーンには持ち込まない。ブロックの境界や種別が変われば `IndexedBlock` の `source_range` / `id` が変わり、`matches_plain` が外れる。

**可視範囲への限定**: 提示は可視ブロックにしか起きない（既存の仮想化）。解析はブロック単位で、ブロックが予算内なら全体を読む。行単位への限定はしない。Markdown は行をまたぐ構造（複数行の強調、リスト内のコード、引用の継続）を持ち、行単独の解析では色が誤るため。

**コスト見積り**: 1 回の解析は最大 256 KiB（予算上限）。これは投影モードが joinable ブロックについて同期で行っている解析と同じ上限・同じ parser で、予算値はそのために決められている。プレーン＋ハイライトの提示は「行の読み出し＋解析 1 回＋ラン生成」で、投影モードの提示（解析＋マーカー投影＋開示計算＋ SourceMap 構築）より処理が少ない。ただし投影モードが解析しない種別（表、見出し、HTML）も解析する点が増える。実測は 7.4 節。

---

## 6. 既存機能との相互作用と回帰リスク

| # | リスク | 起きる条件 | 対策 |
| --- | --- | --- | --- |
| R1 | 本文・保存内容が変わる | 色付けが文書に触れる | 追加コードは読み取りだけ（5.5 節）。テスト `syntax_highlight_toggle_preserves_document_snapshot` |
| R2 | キャレット・クリック位置がずれる | 太字・斜体が描画にだけ、または幾何にだけ入る | `style_runs` の 1 経路だけを使う。`BlockDisplay` を使わない。テスト `plain_highlight_bold_reaches_shape_runs`、`plain_heading_caret_x_matches_painted_text` |
| R3 | `visual_text` / SourceMap が変わる | 提示関数の中でテキストを触る | 提示後に `style_runs` を代入するだけ。テスト `plain_highlight_changes_only_style_runs` |
| R4 | 色だけのランで字送りが変わる | `TextRun` の分割で字間調整が切れる（未確認） | テスト `plain_highlight_does_not_move_color_only_glyphs`。動く場合は、`WindowShaper::runs` の境界を「`bold` / `italic` / `monospace` が変わる端点」に絞る（色だけの端点で分割しない）。この変更は投影モードのリンク色などにも効くので、採る場合は別 PR の整理として先に入れる |
| R5 | 高さがずれてスクロールが飛ぶ | `kind` や `estimated_height` を変える | 変えない。テスト `plain_highlight_keeps_block_height_invariants` |
| R6 | 設定切替後に古い色・古い折り返しが残る | キャッシュが残る | `rebuild_presentation_for_mode_change` で破棄。テスト `syntax_highlight_toggle_rebuilds_plain_presentation` |
| R7 | 投影モードの表示が変わる | `InlineDisplay` へのフィールド追加、`row_element` の変更 | 投影経路は `StyleKind::Syntax` を出さないので `tone` は常に `None`。テスト `projection_mode_never_emits_syntax_tone`。既存テストが全部通ること |
| R8 | 検索ハイライトが読みにくくなる | 色付き文字が検索背景に乗る | 検索背景が描かれるセグメントでは色付けしない（`segment_syntax_tone`）。テスト `syntax_tone_is_suppressed_on_find_match` |
| R9 | 選択範囲が読みにくくなる | 色付き文字が選択背景に乗る | 4.3 節のコントラスト。テスト `syntax_colors_keep_contrast` |
| R10 | 大文書で入力・スクロールが重くなる | 巨大ブロックを毎回解析する | 予算（既存値）で打ち切り、超過時は窓だけ。コードブロックは解析しない。7.4 節の測定 |
| R11 | 巨大ブロックで色がスクロール位置によって変わる | 縮退（窓だけの解析） | 仕様として許容（3.5 節）。P3 で joinable ブロックは背景解析に切り替える |
| R12 | ランが文字境界でない位置を指して panic | 範囲計算の誤り | ラン範囲は parser と RopeBuffer の境界だけから作る。行で切る。`debug_assert!`。テスト `plain_highlight_runs_are_on_char_boundaries`（CJK・絵文字・CRLF） |
| R13 | 設定ファイルの互換性 | 旧版が新キーを読む / 新版が旧ファイルを読む | 未知キーは無視される（L495-527）。欠損は `true` |
| R14 | 既存設定が消える | `store` の `format!` 変更ミス | テスト `settings_survive_a_store_reload` を拡張 |
| R15 | IME 合成中の切替で合成範囲が壊れる | 切替を許す | `editor_mode_switch_blocked()` で拒否。テスト `syntax_highlight_toggle_is_refused_while_composing` |
| R16 | IME の下線が色付けで消える | セグメント属性の上書き | 色付けは `text_color` だけ。下線（`marked`）は別属性。G4 で確認 |
| R17 | #737 と同時に進めて衝突する | `present_block_plain` のシグネチャ変更 | #737 P1 の merge 後に着手する。引数追加は `Option` で、`None` は #737 と同じ出力 |
| R18 | リンク色がクリックできそうに見える | 青い文字 | 下線を付けない。ホバー色も使わない。通常モードのリンクは「青＋下線」なので区別できる |

**個別の相互作用**:

- **開示**: プレーンには開示が無い（#737）。キャレットの位置で色が変わることは無い。
- **折り返し**: 太字・斜体のランは折り返し位置に反映される（`wrap_boundaries` が `runs` を使う）。行をまたぐセグメントは `row_element` が行（row）ごとにセグメントを作るので、色は折り返し後の各行に正しく付く（既存の `style_runs` の扱いと同じ）。
- **検索ハイライト**: 背景は既存どおり。文字色だけ検索一致の上で本文色に戻る。検索の一致範囲・ジャンプ・置換には関与しない。
- **選択範囲**: 背景は既存どおり。文字色は残る。
- **リンクホバー・クリック**: プレーン行は `links` が空（#737）。`link_hover` は立たない。
- **AI 選択ハイライト**（`view/ai_selection.rs`）: 描画の詳細は未確認。背景で描いているなら選択と同じく共存する。G5 で確認する。
- **Mermaid・画像**: プレーンでは出ない（#737）。フェンスの `mermaid` は言語名として構造色になるだけ。
- **表編集・リスト編集・タスクチェックボックス**: プレーンでは無効（#737）。色付けは入力経路に関与しない。
- **アクセシビリティ**（`a11y_links.rs`）: 色付けは読み上げ内容に関与しない。変更なし。

---

## 7. テスト計画

### 7.1 単体テスト

**`crates/presentation/src/projection/highlight.rs`**（解析結果 → ラン。入力は文字列、期待は「部分文字列 → 役割 / フラグ」の対応で書く。ヘルパ `tones_of(source) -> Vec<(&str, InlineDisplay)>` を用意し、行ごとに `style_runs_for_line` → `InlineDisplay::for_styles` で畳んで比較する）

| テスト名 | 入力 | 期待 |
| --- | --- | --- |
| `atx_heading_splits_marker_and_text` | `# Title` | `# ` = Punctuation・非太字、`Title` = Heading・太字 |
| `heading_levels_use_two_tones` | `# a` 〜 `###### a` | 1・2 = Heading、3〜6 = HeadingMinor |
| `heading_closing_hashes_are_punctuation` | `## a ##` | 末尾 ` ##` = Punctuation |
| `empty_heading_is_all_punctuation` | `#`、`# ` | 全体 Punctuation、太字なし |
| `setext_heading_underline_is_punctuation` | `Title\n===`、`Title\n---` | 1 行目 Heading・太字、2 行目 Punctuation |
| `non_heading_hash_is_plain` | `#a`、`####### a` | ランなし |
| `strong_and_emphasis_set_flags_and_dim_delimiters` | `**a** *b* ***c***` | フラグと記号の Punctuation |
| `strikethrough_sets_flag` | `~~a~~` | strikethrough、`~~` = Punctuation |
| `unclosed_emphasis_is_plain` | `**a` | ランなし |
| `inline_code_is_code_tone_without_monospace_or_background` | `` `a` `` | `a` = Code、`monospace == false`、`code_background == false`、バッククォート = Punctuation |
| `inline_link_colors_text_and_dims_destination` | `[t](https://e.x)`、`[t](./a.md)` | `t` = Link、`[` と `](…)` = Punctuation、`underline == false` |
| `autolink_and_bare_url_are_link_tone` | `<https://e.x>`、`see https://e.x now` | URL = Link |
| `image_follows_link_rule` | `![alt](a.png)` | `alt` = Link、残り = Punctuation |
| `unresolved_reference_link_is_plain` | `[t][r]` | ランなし |
| `nested_inline_in_heading_prefers_inner_tone` | ``# a `b` [c](u)`` | `b` = Code・太字、`c` = Link・太字、`a` = Heading・太字 |
| `quote_marker_is_structure_and_body_is_quote` | `> a`、`> > a`、`> **b**` | `> ` = Structure、本文 = Quote |
| `list_markers_are_structure` | `- a`、`* a`、`+ a`、`1. a`、`1) a` | 記号＋区切り = Structure、本文ランなし |
| `task_checkbox_is_structure` | `- [ ] a`、`- [x] a` | `[ ]` / `[x]` = Structure |
| `nested_fence_in_list_colors_code_and_info` | `- a\n\n  ```rs\n  x\n  ``` ` | 記号 = Punctuation、`rs` = Structure、`x` = Code |
| `rule_is_punctuation` | `---`、`***` | 全体 Punctuation |
| `html_is_punctuation` | `<div>\nx\n</div>`、`a <b>c</b>` | HTML 部分 = Punctuation |
| `table_cell_ranges_exclude_pipes` | `\| a \| b \|\n\| - \| - \|\n\| c \| d \|` | **parser の事実を固定**: 各 `TableCell` の範囲が `\|` を含まない（含むなら 5.2 (2) の字句規則に切り替える） |
| `table_pipes_and_delimiter_row_are_punctuation` | 同上 | `\|` と 2 行目 = Punctuation、ヘッダのセル内容 = 太字 |
| `hard_break_backslash_is_punctuation` | `a\\\nb` | `\` = Punctuation |
| `multi_line_strong_is_clipped_per_line` | `**a\nb**` | 両行に Bold、各行のランが行内に収まる |
| `plain_highlight_runs_are_on_char_boundaries` | CJK・絵文字・結合文字を含む各要素、CRLF 版 | 全ランの端点が `is_char_boundary`、行の長さ以内 |
| `empty_and_blank_sources_yield_no_runs` | `""`、`"\n\n"` | ランなし、panic なし |

**`plain_code_line_runs`**

| テスト名 | 入力 | 期待 |
| --- | --- | --- |
| `fence_opening_line_splits_delimiter_and_info` | ```` ```rust ````、```` ~~~ py ````、`   ```x` | 記号（インデント込み）= Punctuation、info（前後空白除く）= Structure |
| `fence_opening_without_info_has_only_punctuation` | ```` ``` ```` | Punctuation のみ |
| `fence_content_and_closing_lines` | 内容行、閉じ行 | Code / Punctuation |
| `indented_code_is_all_code` | `opening = None` | 全行 Code |
| `owned_trailing_blank_lines_have_no_runs` | `line >= content_end` | なし |

**`crates/presentation/src/display.rs`**

| テスト名 | 期待 |
| --- | --- |
| `syntax_style_sets_only_tone` | `StyleKind::Syntax(t).display()` が `tone == Some(t)` 以外すべて既定値 |
| `union_keeps_higher_ranked_tone` | 全組み合わせで rank の高い方。順序を入れ替えても同じ |
| `union_keeps_flags_with_tone` | `Bold` ∪ `Syntax(Heading)` = bold かつ Heading |
| `existing_style_kinds_have_no_tone` | 既存の全 `StyleKind` で `tone == None` |

**`crates/presentation/src/projection/block.rs`**

| テスト名 | 入力 | 期待 |
| --- | --- | --- |
| `plain_highlight_changes_only_style_runs` | 各種ブロック（見出し・リスト・表・フェンス・引用・CRLF） | `highlight = None` と `Some` の `VisualBlock` が、各行の `style_runs` 以外すべて等しい（`visual_text`、`source_map`、`kind`、`estimated_height`、`source_range`） |
| `plain_highlight_keeps_block_height_invariants` | 同上 | `VisualBlock::height()` == `plain_block_height` |
| `plain_highlight_none_matches_issue_737_output` | 同上 | `style_runs` が空 |
| `plain_highlight_window_only_parse_does_not_panic` | 複数行の強調の途中から始まる窓 | panic なし、ランが行内 |

**`crates/ui/src/theme.rs`**

| テスト名 | 期待 |
| --- | --- |
| `syntax_colors_keep_contrast` | 両テーマで、`syntax_tone_color` が返す全色の相対輝度比が `editor_background` に対し 4.5 以上、`selection_background` に対し 3.0 以上（計算式は WCAG 2.x。テスト内に小さな関数を置く） |

**`crates/ui/src/line.rs`**

| テスト名 | 期待 |
| --- | --- |
| `syntax_tone_color_maps_every_tone_in_both_themes` | 4.1 節の表どおり |
| `syntax_tone_is_suppressed_on_find_match` | `find_match && !selected` で `None`。`selected` なら残る。一致なしなら残る |
| `line_segments_split_at_syntax_run_edges` | `# a` の行でセグメントが `# ` と `a` に分かれ、`display.tone` が期待どおり |

**`crates/session/src/store.rs`**

| テスト名 | 期待 |
| --- | --- |
| `settings_survive_a_store_reload`（既存 L868 を拡張） | `syntax_highlight = false` が往復する |
| `missing_syntax_highlight_key_defaults_to_true` | キー無しのファイル → `true` |
| `unknown_syntax_highlight_value_defaults_to_true` | `syntax_highlight=maybe` → `true` |

### 7.2 統合テスト（`gpui::test`。`crates/ui/src/view/plain_text_mode.rs` と `crates/ui/src/shape.rs`）

ヘルパは #737 が使う `reading_mode.rs` L356〜 のもの（`open_view`、`settle`、`deliver_requested_frames`、`snapshot` = `(Revision, len, dirty, can_undo)`）を流用する。

| テスト名 | 手順 | 期待 |
| --- | --- | --- |
| `syntax_highlight_toggle_preserves_document_snapshot` | プレーン表示で各種要素を含む文書を開き、キャレットと選択を置いて ON → OFF → ON | `snapshot`、`full_text()`、選択のソースオフセットが毎回同じ |
| `syntax_highlight_toggle_rebuilds_plain_presentation` | プレーン表示で OFF → ON | `block_cache` が作り直され、見出し行の `style_runs` が空 → 非空 |
| `syntax_highlight_toggle_in_projection_mode_changes_nothing_visible` | 投影モードで切替 | 提示された `VisualBlock` が切替前後で等しい。設定値は変わる |
| `projection_mode_never_emits_syntax_tone` | 投影モード（Editing / Reading）で各種要素を提示 | 全行の `style_runs` に `StyleKind::Syntax` が無い |
| `syntax_highlight_toggle_is_refused_while_composing` | IME 合成中に `set_syntax_highlight` | `false` を返し、設定値・表示が変わらない |
| `plain_highlight_bold_reaches_shape_runs` | `shape.rs` のテスト（既存 L205 の形）。プレーン＋ハイライトで `# 見出し **b**` を提示し `shaper.runs` を直接呼ぶ | 見出し本文の `TextRun` が `FontWeight::BOLD`、`# ` は通常の太さ、全 `TextRun` の family が既定（等幅でない） |
| `plain_highlight_does_not_move_color_only_glyphs` | `` a `b` c `` と `- a` を ON / OFF で提示し、各文字境界の `x_for_offset` を比べる | 全オフセットで一致（色だけのランは幾何を変えない）。**失敗したら R4 の対処に進む** |
| `plain_heading_caret_x_matches_painted_text` | `# 見出し` の各オフセットで `x_for_offset` → `offset_for_x` を往復 | 元のオフセットに戻る |
| `plain_highlight_edit_reparses_only_edited_block` | 2 ブロックの文書で 1 つ目に入力し、両方を `cached_block` | 1 つ目は再提示（`reused == false`）、2 つ目は再利用（`true`）で `style_runs` が同じ |
| `plain_highlight_block_cache_is_reused_across_frames` | 同じ状態で 2 回 `cached_block` | 2 回目は `reused == true` |
| `plain_highlight_typing_keeps_caret_offset` | `**a` の末尾で `*`、`*` と入力 | 各打鍵後のキャレットのソースオフセットが「挿入位置＋ 1」。最後に行が太字になる |
| `plain_highlight_over_budget_block_uses_window_parse` | 4,097 行のリストを開く | panic なし。提示された行の `visual_text` がソースと一致。可視行のリスト記号が Structure |
| `plain_highlight_large_code_block_is_exact` | 10,000 行のフェンスの中ほどを表示 | 可視行がすべて Code。開き行・閉じ行へスクロールすると Punctuation / Structure |
| `plain_highlight_survives_theme_change` | テーマをライト → ダーク | panic なし。`style_runs` は同じ（色は提示に入っていない） |
| `settings_checkbox_reflects_syntax_highlight` | 設定画面でチェックを切替 | `settings.syntax_highlight` と一致。保存される |

（P3）`plain_highlight_uses_joined_parse_when_available`: 予算超過のリストで背景解析の完了後、窓の外から始まる複数行の強調が正しく太字になる。

### 7.3 GUI シナリオ

ビルドは対象 head の Debug / development build。記録には build 種別・対象 SHA・実行バイナリの由来を含める（AGENTS.md）。macOS で実施し、G4 は Windows でも行う（Windows 側で直接実行）。

| # | シナリオ | 確認点 |
| --- | --- | --- |
| G1 | 見出し（H1〜H6）、強調、打消し、インラインコード、リンク、画像、フェンス（言語名つき）、引用、リスト、タスク、表、水平線、HTML を含む文書をプレーン表示（ライト） | 3 章の表どおりの色と太さ。文字サイズ・行高が全行同じ。スクリーンショットを残す |
| G2 | 同じ文書をダークテーマで | 同上。どの色も背景に埋もれない |
| G3 | 各要素の上でクリック・ドラッグ選択・↑↓・Home / End | キャレットが期待の文字位置に立つ。太字の見出し行でもずれない |
| G4 | 見出し行・リスト行で日本語 IME 入力（合成・変換・確定・キャンセル） | 合成下線と候補ウィンドウの位置が正しい。色付けで下線が消えない |
| G5 | 色付きの文字を含む範囲を選択、検索（一致・現在位置）、AI 選択 | 選択中は色が残り読める。検索一致の上は本文色で読める |
| G6 | `**a` に続けて `**` を入力、`# ` を行頭に入力、フェンスを開いて閉じる | 入力に応じて色が変わる。キャレットが飛ばない。画面がちらつかない |
| G7 | 設定画面でハイライトを OFF → ON（プレーン表示中） | 色が消える / 付く。スクロール位置とキャレット行が保たれる。本文が dirty にならない |
| G8 | ハイライト ON のまま通常モードへ切替 | 通常モードの表示が従来どおり（色が足されない） |
| G9 | 折り返す長い見出し・長いリンク行 | 折り返し後の各行に色が付く。キャレットの上下移動が正しい |
| G10 | 大きな文書（数万行、巨大なリストと巨大なコードブロックを含む）でスクロール・入力 | 引っかかりが、ハイライト OFF のプレーン表示と比べて体感で悪化しない |
| G11 | OFF のまま終了して再起動 | OFF で起動する |
| G12 | 編集して保存し、外部ツールでファイルを確認 | 入力どおり。余計な変更なし |

### 7.4 変更前測定

- 対象: プレーン表示（#737 merge 後、ハイライト無し）での、大きな文書の入力 1 打鍵あたりの処理時間とスクロール時のフレーム時間。ハイライト実装後に同じ文書・同じ操作で比べる。
- 文書: (a) 通常の長文（数万行、段落と見出しが中心）、(b) 4,000 行前後の 1 つのリスト（予算内の最大級）、(c) 1 万行のコードブロック、(d) 200 KiB の 1 段落。
- `view.rs` に `instrument` / `timing-probe` feature の計測（`layout_cache_hits` / `layout_cache_misses`、L5293-5300）がある。提示・解析の時間を測る既存手段があるかは**未確認**。`docs/refactor-execution-plan.md` の測定手順に従う。
- 受入の目安: (a) で打鍵・スクロールの悪化が測定誤差の範囲。(b)(d) で 1 回の再提示が 1 フレーム（16 ms）を大きく超えない。超える場合は 5.2 (3) の `parse_document` 直呼びへの置換、それでも足りなければハイライト用の同期予算を既存値より小さく取る（9 章 U-7）。

---

## 8. 段階分割

| フェーズ | 内容 | 完了条件 |
| --- | --- | --- |
| **P1 色付けの本体** | 5.2 の (1)〜(10)。設定は `settings.conf` のキーとセッタまで（UI なし。既定 ON） | 7.1 全部、7.2 のうち設定画面と P3 以外が通る。既存テストが全部通る。G1〜G6、G8〜G10、G12。7.4 の測定で悪化なし |
| **P2 設定画面** | 5.2 (11) | `settings_checkbox_reflects_syntax_highlight`。G7、G11 |
| **P3 巨大ブロックの正確化** | 5.2 (12)、`PlainHighlightInput::Joined` の利用 | `plain_highlight_uses_joined_parse_when_available`。予算超過のリスト・段落で、スクロール位置によらず色が一致する |

- P1 だけで Issue の中核（プレーン表示で見出しなどに色が付く）は成立する。
- P1 が大きすぎる場合は、`presentation` の追加（5.2 の (1)(2)(3)(4)。`highlight = None` で呼ばれるので表示は変わらない）を先行 PR に切り出せる。
- P2 は #737 P2（設定画面のプレーンモード用チェックボックス）の後に置くと、同じカードに並べるだけで済む。
- P3 は必要性を P1 の実使用で確かめてから着手してよい。`PlainHighlightInput::Joined` の variant は P1 で定義だけしておくか、P3 で追加するかは実装者の判断でよい（未使用 variant の警告が出るなら P3 で追加する）。
- 先行条件: #737 P1。

follow-up 候補（本 Issue の外）: 通常モードの開示中の記号を薄くする、コードブロックの言語別ハイライト、参照リンクの解決、等幅フォント、行末空白・タブの可視化、色のカスタマイズ。

---

## 9. 未確定事項

| # | 事項 | 選択肢 | 推奨 |
| --- | --- | --- | --- |
| U-1 | 見出しの色相 | (a) 錆色（`0x9a3f1c` / `0xf0a36b`） (b) 既存の青（リンクと同色） (c) 色を付けず太字だけ | **(a)**。(b) はリンクと区別できない。(c) は Issue の要求（色を付ける）に合わない。値は実機で見て微調整してよい（7.1 節のコントラストテストを通る範囲で） |
| U-2 | 見出しレベルの段階差 | (a) 2 段（H1・H2 / H3〜H6） (b) 全レベル同色 (c) 6 段 | **(a)**。サイズを変えられない条件で色だけの 6 段は判別できない |
| U-3 | `#` の扱い | (a) 本文より薄く・通常の太さ (b) 見出し本文と同じ色・太字 | **(a)**。全記号を同じ規則にそろえる。(b) は VS Code などの既定に近いが、記号が本文と同じ強さになる |
| U-4 | リスト記号・引用記号の色 | (a) 構造色（見出しと同じ錆色） (b) 薄色（他の記号と同じ） (c) 別の色相 | **(a)**。箇条書きの始まりを見つけやすくする。(c) は色数が増える |
| U-5 | フォント（#737 U-3 の決着） | (a) 本文フォントのまま (b) 等幅 | **(a)** で確定する。等幅は日本語混在時の字幅と行高の検証が要り、色付けの成立に不要 |
| U-6 | 通常モードへの適用 | (a) しない (b) 開示中の記号だけ薄くする (c) 全面適用 | **(a)**。根拠は 3.6 節。(b) は別 Issue の候補 |
| U-7 | 既定値 | (a) ON (b) OFF | **(a)**。プレーン表示を選んだ利用者に、オーナーが求めた見た目を既定で出す。色が不要な人は設定で切る |
| U-8 | 設定の単位 | (a) プレーンモードとは独立の ON / OFF 1 個 (b) プレーンモードに連動（設定なし） (c) 強さの段階（弱 / 標準） | **(a)**。#737 設計書 9 章の方針どおり。(c) は要望が出てから |
| U-9 | インラインコードの背景 | (a) 付けない (b) `code_background` を付ける | **(a)**。色だけで足りる。(b) は選択・検索背景との重なり処理が増える |
| U-10 | 検索一致の上の文字色 | (a) 本文色に戻す (b) 構文色のまま | **(a)**。4.3 節のとおり検索背景上のコントラストが足りない |
| U-11 | 済タスク（`- [x]`）の本文 | (a) 本文色のまま (b) 薄く / 打消し | **(a)**。本文に取り消し線を引くと「記法として打消しが書いてある」ように見える |
| U-12 | コードブロックの言語名の色 | (a) 構造色 (b) 薄色 | **(a)**。何の言語かを見つけやすくする |
| U-13 | 巨大ブロックの縮退 | (a) 窓だけ解析（P1）→ 背景解析（P3） (b) 予算超過は色付けしない | **(a)**。投影モードの既存の縮退と同じ考え方 |
| U-14 | 設定画面の文言 | 「プレーンテキスト表示で Markdown 記法を色分けする」ほか | 既存の設定項目の文体に合わせて確定する |

### 未確認の一覧（実装前に読むこと）

- #737 の実装結果: `present_block_plain` の正確なシグネチャ、`presented_block` の `Plain` 分岐が行をどう読むか、`BlockLine.disclosure` に入れる値、`set_plain_text` 内の代入と `rebuild…` の順序。
- pulldown-cmark の `TableCell` イベント範囲が `|` を含むか（7.1 節のテストで固定する）。
- `HtmlBlock` / `Html` / `InlineHtml` ノードの範囲の取り方（ブロック全体を覆うか、行ごとか）。どちらでも規則は同じだが、テストの期待値を書くときに確認する。
- `Heading` ノードの範囲が行末の改行を含むか、Setext の下線行を含むか（`derive_markers` L514-528 は改行を自前で除いており、含みうる前提で書かれている）。
- 同じフォントの `TextRun` を分割したときに GPUI の字送りが変わるか（R4）。
- `StyleKind` / `InlineDisplay` を網羅 match・構造体リテラルで扱う箇所の全数（`grep -rn "StyleKind::\|InlineDisplay {" crates/`）。
- `Theme { … }` リテラルの全数。
- 行粒度（`Granularity::Lines`）のときにプレーン行がどの経路で提示されるか（`presented_block` を通るなら変更不要）。
- `view/ai_selection.rs` のハイライト描画（背景か文字色か）。
- `MemoryStateStore` の実装形（`Settings` を構造体のまま持つなら変更不要）。
- 複数ウィンドウ時の `settings` の共有（#737 U-7 と同じ扱いにする）。
- 提示・解析時間を測る既存手段。
- YAML フロントマターの解析結果。
- `docs/adr` の本文（0004 source-visual-mapping、0008 markdown-parsing-strategy、0021 layout-lines、0022 layout-cache-invalidation、0025 shared-parse-for-multiline-inline-presentation）。`StyleKind` に役割を足すことについて新しい ADR が要るかは Commander の判断に委ねる。

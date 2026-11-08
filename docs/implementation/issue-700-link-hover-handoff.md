# Issue #700 実装引き継ぎ: 本文リンクをマウスオーバー時に鮮やかな色で強調する

更新: 2026-11-09

要求と受入条件の正本は [Issue #700](https://github.com/hide212131/hane/issues/700)。この文書は実装担当（Claude Code）が対象 branch から直接読めるよう、採用済み設計と今回フェーズの条件を集約したものである。

## 今回のフェーズの範囲

- 既存の開ける HTTP(S) リンク（Markdown 記法リンクと裸 URL）の表示文字列にマウスオーバーしたとき、前景色だけを鮮やかな hover 色へ変える。
- 編集・閲覧の両モードが対象。クリックの起動条件、リンク認識、ブラウザ起動経路は変更しない。
- hover 色は暫定値としてライト `#0066FF` / ダーク `#40C8FF` を採用する（theme に `link_hover_foreground` を追加）。最終判断は実装後の GUI 確認で行う。
- カーソル形状の変更は対象外（#614 / #615 の範囲）。

## このフェーズの受入条件

Issue #700 の受入条件のうち、この実装フェーズで満たすものは次のとおり。

- Markdown inline/reference link と裸 URL の表示文字上で色が変わり、離れると戻る。
- 同一 destination の別リンクは変色せず、折返し・複数物理行の同一リンクは表示中の各部分が変色する。
- URL 末尾の最後の文字の右半分でも反応し、末尾後方の余白や記法部分では反応しない（hover 判定には caret 境界ではなく `glyph_offset` を使う）。
- ホバー前後で文字位置、折返し、行高、caret 位置、scroll 位置が変わらない（色以外の描画属性を変更しない）。
- 選択背景を維持し、ドラッグ中・IME 変換中は強調を抑制する。
- ポインター静止中の編集・再解析・スクロール・タブ切替で、古いリンクが強調され続けない。
- ホバーだけでは本文 bytes、revision、caret、selection、Undo/Redo、保存状態、外部 URL 起動記録が変わらない。
- 編集モードの修飾キー＋クリック、閲覧モードの通常クリック・4 px drag 判定を維持する。
- 同じリンク内の mouse move だけでは追加の view 更新通知や parse/layout cache 無効化を発生させない。

検証は下記テスト計画の focused regression で行い、Commander が対象 head で実行する。GUI 確認（両テーマ・両モード、実 IME、両 OS）は実装後の証拠工程で行うため、このフェーズの実装完了条件には含めない。

## 採用設計（Codex GPT-6.1 Sol / medium、2026-11-09）

### 調査結果と設計の位置付け

以下の追加シンボル・仕様は**提案**であり、既存実装ではない。

調査は読み取り専用で実施した。ファイル変更、GitHub 投稿、build、テスト、GUI 操作は行っていない。作業ツリーは調査前後とも変更なし。

既定の Jev CLI は指定パスにも PATH にも存在せず、Jev 応答は取得できなかった。以下を Jev 確定済み設計とは扱わない。

### 既存部品

| 責務 | 確認したファイル・シンボル |
|---|---|
| Markdown link と裸 URL の source 範囲 | `crates/markdown/src/parser.rs` の `build_tree`、`bare_http_links`、`scan_bare_urls` |
| source 範囲から表示リンクへの投影 | `crates/presentation/src/projection/mod.rs` の `LinkTarget` 生成部分。list projection または parse 結果を利用 |
| 表セルのリンク補完 | `crates/presentation/src/projection/block.rs` の `present_block_with_table_projection` 内 |
| 通常本文のリンク色 | `crates/ui/src/line.rs` の `row_element`、`theme.link_foreground` の適用部分 |
| 表セルのリンク色 | 同ファイルの `table_row_element`。通常本文とは別の描画経路 |
| 本文 mouse move | `EditorView::on_row_mouse_move`、`on_panel_mouse_move`。現在は主に選択・resize・scrollbar drag 用 |
| 座標から source byte への変換 | `row_hit_at_window_point` → `row_hit_at_point` → `BlockLayout::glyph_source_at_xy` |
| リンク対象判定 | `link_destination_at`、`LinkTarget::contains`。source 範囲は半開区間 |
| モード別クリック・起動 | `apply_row_click`、`on_row_mouse_down`、`on_panel_mouse_up` → `cx.open_url` |

本文のイベント接続は `crates/ui/src/view/render.rs:457`、座標変換は `crates/ui/src/view.rs:3720`、glyph 判定は `crates/presentation/src/layout.rs:718` にある。

`RowHit.offset` は caret 用の最寄り境界、`glyph_offset` はポインター下の文字の開始位置である。**ホバーには `glyph_offset` を使う。** caret 境界を使うと、リンク末尾文字の右半分が終端外と判定される。

既存の hover 表現として、サイドバーには `.hover(...)`、`.group_hover(...)` がある。`crates/ui/src/view/sidebar.rs:1026`  
カーソル指定には `.cursor_pointer()`、resize 用 `CursorStyle::ResizeLeftRight`、タブの `OpenHand` があるが、本文リンクに対応するカーソル切替は確認できない。

依存は registry の `gpui-pre = 0.3.6`。その実ソースで `on_mouse_move`、`on_hover`、`HoverListenerMode::InputModalityIndependent` を確認した。`vendor/gpui` の存在だけを現在の依存実装の根拠にはしていない。

### 変更対象

| ファイル | 変更・追加案 |
|---|---|
| `crates/ui/src/theme.rs` | `Theme::link_hover_foreground` を追加し、両テーマに設定 |
| `crates/presentation/src/display.rs` | `LinkTarget::link_text_range` を追加。`VisualLine::rebase` でこの範囲も変換 |
| `crates/presentation/src/projection/mod.rs` | 全 `LinkTarget` 生成箇所で parser/projection の元の `text_range` を保持 |
| `crates/presentation/src/projection/block.rs` | 表の補完経路でも元のリンク全体範囲を保持 |
| `crates/ui/src/view/link_hover.rs`〈新規〉 | `HoveredLink`、座標からの hover 解決、状態更新、描画範囲への変換を集約 |
| `crates/ui/src/view.rs` | module 宣言、`EditorView` の一時状態、選択操作との接続 |
| `crates/ui/src/view/initialization.rs` | hover 状態の初期化 |
| `crates/ui/src/view/render.rs` | 本文領域の mouse move/hover 接続、現在 frame での再判定、描画引数追加 |
| `crates/ui/src/line.rs` | `row_element`、`table_row_element` に hover 範囲を渡し、該当文字の foreground を変更 |

parser の認識規則、`shape.rs` のフォント方針、クリック判定は変更しない。

#### リンク全体の識別

現在の `LinkTarget.source_range` は物理行ごとに切り詰められている。この情報だけでは、複数物理行にまたがるリンクを一意にまとめられない。

そこで次を区別する。

- `source_range`：従来どおり、その表示行に属する範囲。クリック判定に使用。
- `link_text_range`〈追加〉：切り詰め前のリンク表示文字列全体の範囲。ホバーの同一性に使用。

destination だけで同一リンクと判定しない。同じ URL を指す独立したリンクが同時に変色するためである。

`VisualLine::rebase` は両範囲を変換し、安全に変換できない場合は既存契約に従って再利用を拒否する。`crates/presentation/src/display.rs:545`

### ホバー状態・ヒットテスト・再描画

`EditorView` に次の一時状態を持たせる。

- 本文領域が現在 hover されているか。
- 本文内の最後の pointer position。
- `Option<HoveredLink>`。
- `HoveredLink` は session、document instance、revision、`link_text_range`、destination を保持する。

永続設定、Editor の文書状態、Undo 履歴には入れない。

処理手順は次のとおり。

1. 本文 viewport の mouse move で位置を更新する。既存の row/panel drag 処理は維持する。
2. IME 変換中、選択・scrollbar・sidebar resize 等の drag 中なら hover を `None` にする。
3. 現在表示中の layout から、その座標が属する row を選ぶ。viewport、row の上下端を確認し、余白を隣の行へ丸めない。
4. 既存の `row_hit_at_window_point` を使い、`glyph_offset` に対応する `LinkTarget` を取得する。
5. 前後の `HoveredLink` が異なる場合だけ `cx.notify()` する。同一リンク内の座標変化だけでは通知しない。
6. 本文から離れた場合は解除する。`on_hover` はキーボード入力だけで解除されない `InputModalityIndependent` を使う。ウインドウ外・非アクティブ時や本文を覆う UI も解除対象とする。

row の探索は現在表示している layout に限定し、全ドキュメント走査や UI 側の Markdown 再解析を行わない。表セルは既存の x/y による cell fragment 選択を使う。

**ポインター静止中の変化も扱う。** `render_frame` で現在の presentation/layout を確定した後、保持位置を再判定する。スクロール、ズーム、検索バー、モード切替、編集、背景 parse の publish、タブ切替後の古い範囲を流用しない。描画中の再判定結果はその frame に使い、描画内で追加の `notify()` を連鎖させない。

`cx.notify()` は `EditorView` 単位の更新である。リンクを含む一行だけの再描画になるとは保証しない。一方、hover だけで block/layout cache や height index を無効化せずに済む設計は可能である。`crates/ui/src/view/viewport.rs:572`

ホバー処理から `after_input` は呼ばない。これは解析、caret の scroll、保存予約などを伴うためである。`crates/ui/src/view.rs:2605`

### 色と描画

| テーマ | 現在のリンク色 | hover 候補 | 本文背景 |
|---|---|---|---|
| ライト | `#2867A9` | **`#0066FF`** | `#FAF9F7` |
| ダーク | `#79B8FF` | **`#40C8FF`** | `#1F2022` |

値は提案であり、実画面での見やすさは未検証。既存の `resolve_theme` に従い、System 設定でも選択されたパレットの hover 色を使う。

描画では、同じ `link_text_range` を持つ各 `LinkTarget.source_range` を source map で visual 範囲へ変換し、row・表の各 fragment と交差させる。その範囲内の既存 link style の foreground だけを変更する。

- フォント、サイズ、weight、下線、padding、行高、文字列は変更しない。
- 選択背景 ＞ 検索背景という既存優先順位を維持する。
- hover 開始時だけ文字要素の分割が増えないよう、対象範囲の境界は通常時にも同じように扱う。
- `WindowShaper::font_revision` は既に色を geometry のキーから除外している。`crates/ui/src/shape.rs:34`

したがって hover 前後の layout を同じに保てる。ただし文字要素の分割は shaping に影響し得るため、「色だけだから安全」として済ませず、隣接リンク・英字の kerning・日本語を含む geometry 回帰で確認する。

### カーソルとクリック意味論

**今回、カーソル形状は変更しない案を推奨する。**

編集モードで常時 pointing hand にすると、通常クリックで開けるように受け取られる可能性がある。正確な案内には静止中の修飾キー押下・解放も扱う必要があり、既存 #614/#615 と重なる。

色は両モードでリンクの存在を示す。起動条件は現在の `apply_row_click` に委ねる。

| 操作 | 維持する動作 |
|---|---|
| 編集・通常左クリック | caret 移動・選択 |
| 編集・OS 標準修飾キー＋左クリック | HTTP(S) リンクを開く |
| 閲覧・通常左クリック | mouse-up 時に開く。4 px 以上移動なら選択 |
| 閲覧・OS 標準修飾キー＋左クリック | 既存の起動処理 |
| Shift/Alt 等を含む組合せ | 既存判定を維持 |

hover 判定は destination の選択までを行い、`cx.open_url`、focus 変更、selection 更新を実行しない。

### テスト計画

| 対象テストファイル | 追加する検証 |
|---|---|
| `crates/presentation/src/projection/tests.rs` | inline/reference、複数物理行で `link_text_range` が共通になること。`source_range` の行別 clipping と rebase の両立 |
| `crates/presentation/tests/bare_url_links.rs` | 裸 URL の元範囲、同じ URL が複数ある場合の独立性 |
| `crates/ui/src/line.rs` の tests | hover 範囲と segment の交差、通常行・表セルの色選択、選択・検索・IME 表示の優先順位 |
| `crates/ui/src/view.rs` の GPUI tests | 実 `EditorView` に座標指定 `MouseMoveEvent` を送り、hover 状態、描画色、解除、文書状態の不変を検証 |

GPUI の focused regression には次を含める。

- 最終文字の右半分、終端直後、先頭前方、UTF-8 境界。
- 同じ URL の別リンク、太字・code 等を含むラベル、soft-wrap、複数物理行。
- 表セルの折返しと中央・右寄せ、引用・リスト、検索バー表示、ズーム。
- リンク内移動では追加通知なし、別リンク・非リンクへの移動では更新。
- pointer 静止中の scroll、編集、Undo/Redo、背景 parse publish、タブ切替。
- selection drag、IME 開始・終了、本文外・ウインドウ外・別 UI に覆われた場合。
- hover 前後の row bounds、wrap boundaries、caret geometry、scroll 値、cache 再利用。
- hover だけで URL 起動記録や編集状態が変わらないこと。

既存の `bare_url_click_follows_the_modifier_contract_and_leaves_editor_state_untouched`、`reading_bare_url_press_opens_on_mouse_up_and_drags_select_through_dispatch`、表の hit-test とリンク描画テストを回帰対象に含める。

実装時は focused tests に続き、workspace tests、Clippy、fmt check を行う。今回は実行していない。

**GUI 確認は必要。** 色の差、native pointer 配送、実 IME、ブラウザ到達はモデルテストだけでは確定できない。

macOS/Windows の対象 head の Debug/development build で、次を確認する。

1. 両テーマ・両モードで Markdown link、裸 URL、折返し、表セルを順に hover。
2. 同一 destination の別リンク、リンク末尾、余白へ移動して強調範囲を確認。
3. 選択を残して hover、リンクからドラッグ、日本語変換中に pointer を移動。
4. pointer を止めて scroll・zoom・編集・タブ切替し、強調の追従を確認。
5. 編集の修飾キークリック、閲覧の通常クリックと drag を確認。
6. 対象 SHA、build 種別、実行バイナリの由来、before/hover/after 画像を記録。

Windows は接続先 Windows 環境から直接操作する。

### リスク・代替案・未確定事項

- **全体範囲 metadata の追加**：複数行を一体として扱うために必要。生成・rebase の一箇所でも漏れると誤強調につながるため、全 `LinkTarget` 構築箇所を対象にする。
- **古い geometry の利用**：再解析や disclosure の前後で座標が変わる。対象 revision の一致だけで済ませず、現在 frame の presentation/layout から再判定する。
- **hover の頻繁な通知**：位置そのものではなくリンク identity の変化を通知条件にする。性能確認では変更前後で同じ文書・操作条件を使う。
- **対象範囲の誤拡張**：`link_color` が付いているだけの文字を対象にしない。既存の openable `LinkTarget` を正とする。
- **代替案**：hover した物理行のリンク部分だけを変色させれば、全体範囲 metadata の追加は省ける。ただし複数行リンクの強調が途中で切れるため、第一案にはしない。
- **色の最終値**：候補色の視認性・好みは未確認。実装前に利用者へ確認すると有益なのは色味であり、本案では上表を暫定値とする。
- **検証の限界**：#603 の実装存在はコードで確認したが、この調査では実ブラウザ起動・GUI・IME の成功を確認していない。Issue の open/closed と実装・受入証拠を分けて扱う。

## 引き継ぎ上の注意

- 閲覧モードのクリックで開く処理（#603）は Issue が open のままだが、基準 main のコードには実装済みである。未実装と扱わない。
- `HoveredLink` 等の状態は EditorView の一時状態とし、永続設定・文書状態・Undo 履歴へ入れない。
- hover 処理から `after_input` を呼ばない（解析・caret scroll・保存予約を伴うため）。

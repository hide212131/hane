# Issue #779 設計書: 乾くインク

> 位置づけ: **未採用の設計案**です。Commander の検分前で、製品コードの現状説明ではありません。採用時は Commander 追記で、対象フェーズと 10 章の未決事項への判断を明記してください。

- 調査対象: main = `c697e1a0`（v0.53.0）。行番号はこのコミット時点のものです。
- 表記: **［確認］** はコードで確かめた事実、**［推測］** は読んだ範囲からの推定、**［未確認］** は確かめていない点です。

## 0. 結論

| 論点 | 決定（推奨） |
| --- | --- |
| 何を持つか | 「どのソース範囲を、いつ確定したか」の一覧（`InkLedger`）を、**表示中の文書 1 つ分だけ** view が持つ。文書・履歴・保存・セッションには何も書かない |
| どこで色を変えるか | **描画時だけ**。`row_element`（`crates/ui/src/line.rs`）のセグメント `div` の文字色を最後に上書きする。`style_runs`・`block_cache`・`layout_cache`・`WindowShaper` には入れない |
| 投影との整合 | 検索一致と同じ経路を使う。ソース範囲のまま渡し、行ごとに `BlockLayout::visual_range_on_row` で変換する |
| 位置の追従 | `RopeBuffer::deltas_since` の差分で、すべての編集経路を一律に追従する。差分が取れなければ全部消す |
| インクを足す経路 | 直接入力・IME 確定・`unmark_text` の 3 つだけ。貼り付け・一括置換・undo/redo・AI では足さない |
| 乾き方 | 4 段の離散レベル。既定は 60 秒で乾く（15 秒ごとに 1 段）。変化の時刻は 5 秒刻みの共通グリッドに揃える |
| 色差 | OKLab の明度 L を、コントラストが上がる向きに最大 **0.04**（標準）動かす。色相と彩度は変えない |
| 区切りの帯 | 「。」「！」「？」の確定か本文での Enter で、直前の連続した確定範囲の背後に帯を計 **1.0 秒** 出す |
| 再描画 | タイマー 1 本だけ。インクも帯も無ければタイマーも無い。`request_animation_frame` は使わない |
| Reduce Motion | OS 設定を新たに読む（GPUI にも hane にも手段が無い）。ON なら帯を出さない |
| 設定 | `settings.conf` に 4 キーを足す（マスタースイッチ・帯・速さ・強さ） |

テストで固定する不変条件:

1. インクの有無・レベル・帯によって、`VisualLine`（`visual_text`・`source_map`・`style_runs`）、`BlockLayout`（行分割・高さ・x 位置）、`caret_geometry` が変わらない。
2. 記録・乾燥・タイマーの発火によって、`(Revision, 本文, dirty, can_undo, can_redo)`、選択、IME 状態が変わらない。
3. 濡れた範囲も帯も無いとき、タイマーは存在せず、インク由来の `cx.notify()` は 0 回。
4. マスタースイッチ OFF のとき、描画結果は現行と同一（セグメント分割も含む）。

## 1. 範囲

**やること**: 確定範囲の記録と位置の追従、描画時の文字色（乾き）と背景の帯、再描画の予約、設定、Reduce Motion 連動。

**やらないこと**:
- 文書・undo 履歴・セッション復元・下書きへの保存
- 非アクティブなタブでの保持
- 文字の形・太さ・位置・行高・折り返しの変更
- 描画方式（セグメントごとの `div`）の作り替え
- 閲覧モードでの表示

**着手メモ**: 変更するのは `ui`（`line.rs`、`input.rs`、`actions.rs`、`view.rs`）と `session` の `Settings` です。`editor`・`document`・`presentation` は変更しません。先行条件はありません。描画時の上書きなので、投影とレイアウトの契約に触れないためです。#738 の構文色とは、その結果を基準色として読むだけで接し、規則を二重に実装しません。

## 2. 現行処理の調査

### 2.1 描画経路と色の決定 ［確認］

| 層 | 場所 | 色の扱い |
| --- | --- | --- |
| 幾何 | `shape.rs` `WindowShaper::runs`（L51-91） | `style_runs` の端点で区切り、フォントの属性だけを `TextRun` に入れる。色は常に `self.style.color`（L84）。`font_revision()`（L41-46）は、色を意図的に除外するとコメントで明記している |
| 描画 | `line.rs` `row_element`（L1066-1405） | `line_segments_with_hover`（L1914-1971）が行をキャレット・選択・IME・検索一致・`style_runs`・リンク端で分割し、セグメントごとに `div` を 1 つ作る（L1314-1367） |

セグメントの文字色は次の順で決まります。
1. 親から継承する色。ルートは `foreground`（`render.rs` L465）、`BlockTint::Muted` の行は `quote_foreground`（`styled_block_with_surface` L925-947）。
2. `segment_syntax_tone`（L1779）があれば `syntax_tone_color`（L1764）。検索一致の上では無効。
3. `link_color` なら `link_text_color`（L1754）。最後に当たるのでリンクが勝つ。

背景は 選択 > 検索の現在位置 > 検索一致 の順です（L1329-1338）。

表のセル（`table_row_element` L1416〜）は別経路です。`table_fragment_needs_segments`（L1826）が偽なら単一のテキスト要素で描き、真ならセグメントを **shaper の x 座標で絶対配置**します（L1572-1645）。

**乾きは `style_runs` ではなく、描画時のセグメント境界として表現します。** `style_runs` に入れると `VisualLine` が時間で変わり、`block_cache` の再利用が壊れます。一方、検索一致にはすでに「ソース範囲を描画時に行へ投影する」型があります（`row_find_match_visuals` L1038-1057）。これに乗れます。

### 2.2 GPUI の shaping キャッシュ ［確認］

`line_layout.rs` の `CacheKey`（L607-613）は、文字列・フォントサイズ・`FontRun{len, font_id}`・折り返し幅です。**色はキーに入りません。**

そのため、レベルが変わって色だけが変わる場合はキャッシュに当たり、再 shaping は起きません［推測: フレーム間の世代管理の詳細は未確認］。再 shaping が起きるのは、インクの追加や消滅で境界が増減する瞬間だけです。

**位置への影響**: 通常行のセグメントは flex で並ぶ独立したテキスト要素なので、要素をまたぐカーニングや合字は効きません。インク境界で分割すると、境界の後ろがカーニング分（ラテン文字で 1px 程度以下［推測］）ずれる可能性があります。これはキャレットや選択端で**現行でも毎回起きている**のと同じ現象です。厳密に防ぐには描画を 1 要素多色に作り替える必要があり、本 Issue の範囲を超えます（R-1）。本設計では、同じレベルの隣接範囲をまとめ、境界を書記素クラスタ境界へ外側に寄せて、影響を最小にします。表セルは x の絶対配置なので、ずれは生じません。

### 2.3 編集モデル ［確認］

- `RopeBuffer` は編集 1 回で revision を 1 進め、`RevisionDelta` を最大 4,096 件保持します（`document/src/lib.rs` L352）。`deltas_since`（L421）は、履歴が欠けていればエラーを返します。
- `RevisionDelta::transform_range`（L133）は交差する範囲を `None` にするため、インクの追従には粗すぎます。独自の分割規則を使います。
- IME の合成中も合成文字列は**文書に入ります**（`ime.rs` L89 `document.edit`）。
- 確定には 3 経路があります。
  - `commit_text`（L106）: `EditSummary` を返す。
  - `commit_composition`（L241）: 文書を編集しない。`unmark_text` に加え、`set_selection`・`dispatch` などの冒頭で強制確定として呼ばれる（`lib.rs` L220/248/267/366）。
  - `cancel_composition`（L256）: 元の文字列に戻す。
- rewrite も `adopt_rewrite`（`lib.rs` L602）で `document.edit` を繰り返すので、差分が残ります。

### 2.4 入力の受け口 ［確認］

- `input.rs` の `replace_text_in_range`（L294-360）は、合成も範囲も無いときは `self.insert_text`、それ以外は `commit_text` を呼びます。
- `unmark_text`（L276）は `commit_composition` の後に `cx.notify()` だけを呼び、`after_input` は呼びません。
- `EditorView::insert_text`（`view.rs` L3806）はテストの補助からも使われます。貼り付けの本体は `clipboard.rs` L156 の `replace_range_recorded` です。**このため、記録は `input.rs` 側に置きます。**
- Enter は `actions.rs` の `Newline`（L359-393）から、`apply_list_edit_intent(Enter)` か `insert_newline`（`view.rs` L3795）へ進みます。
- 全編集経路は `after_input`（`view.rs` L2740）に合流します（`unmark_text` を除く）。

### 2.5 リセット点 ［確認］

`on_document_replaced`（`view.rs` L2640-2694）は、タブ切り替え（`activate_session` L2052）・作業フォルダ切り替え・その場の再読込で呼ばれ、文書固有のキャッシュを消します。インクの破棄もここに置きます。

### 2.6 アニメーションとタイマー ［確認］

- キャレットは点滅しません（`cursor_overlay` L2006）。乗れる点滅タイマーはありません。
- 遅延処理は `cx.spawn` + `background_executor().timer` + 世代カウンタの型で書かれています（`viewport.rs` L772-789）。
- `BackgroundExecutor::now()`（`executor.rs` L350）は、テストでは偽時計を返します。`advance_clock` は `ui` のテスト 16 ファイルで使われています。

### 2.7 Reduce Motion ［確認］

GPUI にも hane にも、この設定を読むコードはありません。OS を直接読む前例として `input_mode.rs`（macOS は `extern "C"`、Windows は `windows_sys`）があり、`objc`・`windows` は依存済みです。

### 2.8 設定 ［確認］

`store.rs` の `Settings`（L52-106）、保存（L489）、読み込み（L543）があります。手本にできるのは `set_syntax_highlight`（`plain_text_mode.rs` L99）と、設定画面のチェックボックス（`settings_screen.rs` L614-707）です。

## 3. 挙動の確定仕様

### 3.1 パラメータ

| 名前 | 既定 | 選択肢 | 意味 |
| --- | --- | --- | --- |
| `DRY_DURATION` | 60 s | 速い 20 / 標準 60 / ゆっくり 180 s | 確定から乾くまでの時間 |
| `WET_LEVELS` | 4 | 固定 | 4 = 最も濡れている、0 = 乾いた |
| `DRY_TICK` | 5 s | 固定 | 変化時刻を揃える共通グリッド |
| `INK_DELTA_L` | 0.04 | 弱 0.025 / 標準 0.04 / 強 0.06 | レベル 4 での OKLab 明度差 |
| `MERGE_WINDOW` | 1.5 s | 固定 | 接する範囲をまとめる時刻差 |
| `MAX_SPANS` | 512 | 固定 | 上限を超えたら古い順に捨てる |
| `BAND_HOLD` / `BAND_FADE` | 600 ms / 400 ms（4 段） | 固定 | 帯の保持時間と消える時間 |
| `BAND_DELTA_L` | 0.03 | 強さに連動（0.02 / 0.03 / 0.045） | 帯の背景の明度差 |
| `BAND_MAX_BYTES` | 4,096 | 固定 | 超えたら末尾側に切る |

### 3.2 乾きのレベル

```text
now_q = epoch + floor((now - epoch) / TICK) * TICK
age   = max(0, now_q - committed_at)
level = clamp(ceil(4 * (1 - age / D)), 0, 4)
```

既定では、15 秒ごとに 4 → 3 → 2 → 1 → 0 と下がります（グリッドの分だけ最大 5 秒遅れます）。レベル 0 になった範囲は一覧から消します。1 段あたりの明度差は約 1% です。

### 3.3 色

基準色（現行の優先順位どおり、リンク色 > 構文の役割色 > `Muted` の `quote_foreground` > `foreground`）を OKLab に変換し、L だけを動かします。

```text
L' = clamp(L + dir * INK_DELTA_L * level / 4, 0, 1)   // ライトは dir=-1（暗く）、ダークは +1（明るく）
level 0 のときは基準色をそのまま返す（変換の往復誤差を出さない）
```

16 進の具体値は本書では固定しません。本設計時に計算環境を実行できず、値を書くと検証されない数字になるためです。代わりに単体テストで次を固定します。
- L の差が `delta_l × level / 4` ± 0.005
- a・b の差が 0.005 以下
- level 0 で完全一致

色を変えないのは次の場合です。
- 検索一致の上（構文色と同じ規則）
- IME の合成範囲
- 画像・図・畳まれた水平線の行

選択範囲の上では色を変えます。

### 3.4 トリガー

| 操作 | インク | 帯 |
| --- | --- | --- |
| 直接入力（合成なし） | ○ | — |
| IME 確定（`commit_text` / `unmark_text`） | ○ 確定した範囲 | 確定文字列の末尾が「。」「！」「？」なら ○ |
| 合成中の打鍵・キャンセル | × | × |
| 強制確定（合成中のクリックなど） | ×（P4 候補） | × |
| Enter / Shift+Enter（文書が変わったとき） | × | ○ |
| Tab・削除系・undo/redo | × | × |
| 貼り付け・一括置換・AI | × | × |
| whisparrot（音声入力） | **判断待ち（U-3）** | × |
| タブ切り替え・再読込・作業フォルダ切り替え | 全消去 | 全消去 |

### 3.5 範囲の追従

範囲は重ならず、開始位置でソートされた状態を保ちます。差分 1 件（編集前の範囲 `B=[bs,be)`）ごとに次の規則で更新します。

| 位置関係 | 結果 |
| --- | --- |
| 範囲が編集より前（`end <= bs`） | そのまま。末尾にちょうど挿入された文字は範囲に入らない |
| 範囲が編集より後（`start >= be`。純粋な挿入で `start == bs` も含む） | `byte_delta` だけずらす |
| 交差している | `B` の外側だけ残す（左はそのまま、右はずらす）。確定時刻は引き継ぐ |
| 範囲の内側への挿入 | 左右に割る。挿入された文字は、確定経路で記録されない限り濡れない |

- 新しい範囲が既存の範囲に接していて、時刻差が `MERGE_WINDOW` 以内ならまとめます（時刻は新しい方を採ります）。
- 差分が欠けていた場合や、文書インスタンスが変わった場合は全部消します。
- 一括置換や AI 置換は範囲内のインクを消します。これは仕様とします。

### 3.6 状態ごとの表示

| 状態 | 乾き | 帯 |
| --- | --- | --- |
| 編集モード・プレーンテキストモード | 表示 | 表示 |
| 閲覧モード | 描かない（時間は進む） | 描かない |
| IME 合成中 | 既存のインクは表示し続ける | 即座に消し、新たには出さない |
| 隠れた記号・合成表示（`•`） | 幅 0 になり描かれない（`source_map.rs` L150-154） | 同左 |
| 折り返し | 行ごとに切って描く | 同左 |
| 表のセル | セグメント経路に切り替えて描く | 描く |
| マスタースイッチ OFF | 全消去し、タイマーを止める | 同左 |

### 3.7 区切りの帯

- **範囲**: 前の区切り以降の確定範囲を、連続している限りつないだもの（`pending_band`）です。離れた場所で確定したら、そこから数え直します。差分でも追従します。
- **トリガー**: 全角の「。」「！」「？」、または Enter です。ASCII の「.」では出しません（「3.14」「e.g.」で誤発火するため。U-5）。
- **見た目**:
  - 色: `editor_background` の L を本文側へ `BAND_DELTA_L` 動かした色。新しいトークン `ink_band_background` を足します。
  - 時間: 即座に出し、600 ms 保持してから、100 ms ごとに不透明度 75 / 50 / 25% と下げて消します。再描画は計 5 回です。
  - 背景の優先順位: 選択 > 検索の現在位置 > 検索一致 > 帯。帯の部分ではインラインコードの `text_bg` を抑止します（GPUI では `text_bg` が `bg` より上に描かれるため。L1838 のコメント）。
- Reduce Motion が ON なら出しません。

### 3.8 境界ケース

- 濡れた範囲の途中への入力: 範囲が割れ、時刻差が窓内なら再びまとまります。
- 選択した上への確定: 古い範囲は消え、新しい範囲になります。
- 確定直後の undo: 範囲も消えます。redo で戻った文字は濡れません。
- 結合文字や肌色修飾子を後から入力: 描画時に書記素クラスタ全体へ広げます。**必須**です。分断すると見た目が壊れるためです。
- 差分が 4,096 件を超える rewrite: 全部消します。
- スリープ中に `Instant` が止まる OS［推測: macOS］: 復帰後に残りの時間だけ乾きます。実害はありません。
- 合成中の設定変更: 許可します（投影に触れないため）。

## 4. 機構設計

### 4.1 全体像

```text
input.rs replace_text_in_range / unmark_text ─▶ note_ink_commit(range, text)
actions.rs Newline ───────────────────────────▶ note_ink_boundary(Newline)
                    ▼
InkLedger（crates/ui/src/ink.rs、GPUI 非依存。時刻は引数で受け取る）
  sync(document)      … deltas_since で追従（after_input と render で呼ぶ）
  frame(now) → InkFrame / next_wakeup(now) → Option<Instant>
                    ▼
view/drying_ink.rs  タイマー 1 本（次の変化時刻だけ起きて cx.notify）
render.rs           row_element(..., &ink_frame)
line.rs             visual_range_on_row → 書記素境界へ広げる → セグメント境界
                    → 最終文字色を ink_color で上書き、帯は bg
```

### 4.2 `crates/ui/src/ink.rs`（新規）

```rust
pub(crate) struct InkParams { dry_duration: Duration, delta_l: f32, band_delta_l: f32, band_enabled: bool }
struct InkSpan { range: SourceRange, committed_at: Instant }
pub(crate) struct InkOwner { session: SessionId, generation: u64, workspace: u64 }
pub(crate) struct InkLedger {
    owner: Option<InkOwner>, revision: Revision, epoch: Instant,
    spans: Vec<InkSpan>,               // 重ならない・ソート済み
    pending_band: Option<SourceRange>, band: Option<InkBand>,
}
pub(crate) struct InkFrame { wet: Vec<(SourceRange, u8)>, band: Option<(SourceRange, f32)>, params: InkParams }

impl InkLedger {
    fn sync(&mut self, owner: InkOwner, document: &RopeBuffer);  // revision が同じなら何もしない
    fn record_commit(&mut self, range: SourceRange, text: &str, now: Instant, p: &InkParams);
    fn record_boundary(&mut self, b: InkBoundary, now: Instant, p: &InkParams);
    fn dismiss_band(&mut self);
    fn frame(&mut self, now: Instant, p: &InkParams) -> InkFrame;   // 乾いた範囲を捨て、同じレベルの隣接をまとめる
    fn next_wakeup(&self, now: Instant, p: &InkParams) -> Option<Instant>;
}
fn rebase_range(range: SourceRange, d: &RevisionDelta) -> [Option<SourceRange>; 2];
pub(crate) fn ink_color(base: u32, level: u8, delta_l: f32, dark: bool) -> u32;
pub(crate) fn band_color(background: u32, delta_l: f32, dark: bool) -> u32;
```

OKLab の変換は 40 行程度の純関数で書き、依存は足しません。`Theme` には明暗を表すフィールドが無いので［確認］、`editor_background` の L が 0.5 未満ならダークと判定します。

### 4.3 既存ファイルの変更

| ファイル | 変更 |
| --- | --- |
| `view.rs` | `EditorView` に `drying_ink: DryingInkState { ledger, timer: Option<(Instant, Task<()>)>, reduce_motion: fn() -> bool }` を足す。`on_document_replaced` で `reset()`、`after_input` の先頭で `sync` を呼ぶ |
| `view/drying_ink.rs`（新規） | `note_ink_commit`・`note_ink_boundary`・`drying_ink_frame`・`schedule_drying_ink_wakeup`・setter。時刻は常に `cx.background_executor().now()`。予約が不要になったら Task を drop して止める |
| `input.rs` | `self.insert_text(..)`（L348）を、`EditSummary.range_after` を記録する `insert_typed_text` に替える。`commit_text`（L352）の summary を `after_input` の**後**で記録する。`unmark_text` は、確定の前に `ime().current_range` を読んでおいて記録する。合成開始時に `dismiss_band` を呼ぶ |
| `actions.rs` | `Newline` / `ShiftNewline` の本文分岐で、revision が変わっていれば `note_ink_boundary(Newline)` を呼ぶ |
| `render.rs` | L502 付近で `InkFrame` を取得し、`row_element`（L548）に渡す。閲覧モードと OFF のときは空にする |
| `line.rs` | `row_ink_visuals` を `row_find_match_visuals` と同じ形で足す（二分探索・書記素境界の拡張）。`line_segments_with_hover` に境界を追加する。`LineSegment` に `ink_level`・`band_alpha` を足す。セグメントの連鎖の**最後**に、帯の `bg` とインクの `text_color` を追加する（既存の 2 つの `text_color` は残す）。`segment_shows_code_background` に帯の条件を足す。表は `table_fragment_needs_segments` にインクの条件を足し、セグメント経路にも同じ処理を入れる |
| `theme.rs` | `ink_band_background` を足す |
| `reduce_motion.rs`（新規） | macOS は `NSWorkspace.accessibilityDisplayShouldReduceMotion`、Windows は `SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION)`、その他は `false`。帯のトリガー時だけ読む［未確認: 該当定数の feature とスレッドの制約］ |
| `store.rs` / `settings_screen.rs` | 5 章 |

インクは `block_cache` と `layout_cache` のキーにも中身にも入りません。

## 5. 設定

| キー | 値 | 欠損・不正値 | 推奨既定 |
| --- | --- | --- | --- |
| `drying_ink` | true / false | 既定値 | **true（U-1）** |
| `drying_ink_band` | true / false | 既定値 | true |
| `drying_ink_speed` | fast / standard / slow | standard | standard |
| `drying_ink_strength` | weak / standard / strong | standard | standard |

設定画面では、「表示」セクションの `settings-plain-text-card` の後にカード `settings-drying-ink-card` を足します。
- 中身: チェックボックス 2 つと、テーマと同じ形のボタン群 2 組。
- マスタースイッチ OFF のとき、他の項目は無効表示にします。
- 補足文: 「文書には保存されません。タブの切り替えや再起動で消えます。」

OFF にしたら一覧を消してタイマーを止めます。キャッシュの消去は不要です。

## 6. 再描画のまとめ

| 状況 | インク由来の再描画 |
| --- | --- |
| インクも帯も無い | 0 回。タイマーも無い |
| 入力中 | ほぼ追加なし（打鍵ごとに `after_input` が notify するため） |
| 入力を止めた後 | 5 秒グリッド上のレベル変化時だけ。範囲が 1 つなら 4 回 |
| 帯 | 1 回につき 5 回 |

乾きの変化でブロックの再提示や再レイアウトは起きません。

## 7. テスト計画

- **単体（`ink.rs`、GPUI なし、時刻は引数）**:
  - 追従規則の全ケース（前・後・端・内側・交差・包含・2 範囲にまたがる置換・複数差分）
  - 差分の欠落やインスタンス違いでの全消去
  - ランダムな編集列で、重ならず・ソート済みの不変条件が保たれること
  - 範囲のまとめと上限
  - D=20/60/180 でのレベル境界
  - 乾き切ったら `next_wakeup` が None になること
  - 帯の蓄積・トリガー・時間
  - `ink_color` の ΔL・a/b 不変・level 0 での一致（ライト・ダークの全基準色トークン）
- **描画関数（`line.rs`）**:
  - インクが空なら、既存の全ケースでセグメント列が同一
  - 同じレベルの隣接で境界が増えない
  - 書記素クラスタ（結合文字・ZWJ・修飾子・異体字セレクタ）を分断しない
  - 検索一致・合成範囲では色を変えない
  - 帯は `text_bg` を抑止し、選択には負ける
  - 隠れた記号と `•` で幅 0 になる
  - 折り返しで行ごとに切れる
- **GPUI（`view/drying_ink.rs`、`advance_clock` 使用）**:
  - 経路ごと: 直接入力・`commit_text`・`unmark_text` で濡れる。合成中・キャンセル・貼り付け・一括置換・undo・AI では濡れない
  - タブ切り替えと再読込で消える。閲覧モードでは描かない
  - 不変条件 1・2: `VisualLine`・レイアウト行・`caret_geometry`・`debug_bounds` が同一。文書状態も同一
  - 乾き切った後はタイマーが無く、通知が 0 回（`input_routing.rs` の `count_notifications` と同じ方法）
  - 帯の時間と通知 5 回以下。Reduce Motion を差し替えて帯が出ないこと
  - 設定の反映と保存（`store.rs` は L1190-1254 の型で書く）
- **既存テストへの影響**:
  - `row_element` と `line_segments_with_hover` の呼び出し箇所を修正する必要がある。
  - 既定 ON の場合、`EntityInputHandler` 経由で入力してから描画構造を検査するテストが落ちる可能性があります［推測、洗い出しは未実施］。そのテストの中で OFF にして対応し、期待値は書き換えません。`dispatch(Insert)` や `view.insert_text` で入力するテストは影響を受けません。
- **GUI の観点**（要否は Commander の判断）: 標準の濃さが「見えるが気にならない」か。日本語 IME（macOS・Windows）。ラテン文字の境界で字が跳ねないか。静止時の GPU 使用率。Reduce Motion。

## 8. フェーズ分け

| フェーズ | 範囲 | 受入条件 |
| --- | --- | --- |
| **P1** モデル | `ink.rs` だけ（どこからも呼ばない。見た目の変化なし） | 単体テストが全部通る。`gpui` を使わず `Instant::now()` を呼ばない。既存テストに変更がない。clippy が通る |
| **P2** 乾き・タイマー・マスタースイッチ | `view.rs`・`drying_ink.rs`・`input.rs`・`render.rs`・`line.rs`（帯以外）、`drying_ink` キーとチェックボックス | 不変条件 1〜4 を GPUI テストで固定している。OFF なら既存の期待値を変えずに全テストが通る。設定の往復・欠損・不正値のテストが通る。既定値は U-1 の判断に従う |
| **P3** 帯・Reduce Motion・速さと濃さ | 帯・`actions.rs`・`reduce_motion.rs`・テーマトークン・残りの設定 | 残りのテストが通る。帯 1 回の通知が 5 回以下。Reduce Motion は macOS・Windows で実装済み |
| **P4**（任意） | 強制確定の捕捉（`editor` に最小 API。別 Issue が望ましい）、音声入力、英文の文末 | — |

## 9. リスクと代替案

| ID | リスク | 対応 |
| --- | --- | --- |
| R-1 | 境界でのカーニング・合字の変化 | 現行のキャレットや選択端と同じ性質として受け入れ、境界を最小化する。根本対策（1 要素多色の描画）は別 Issue |
| R-2 | 書記素クラスタの分断 | 描画時に外側へ広げる（必須） |
| R-3 | 既定 ON による既存テストへの影響 | テストごとに OFF にする。既定値は U-1 |
| R-4 | 強制確定した文字が濡れない | 既知の制限として P4 へ |
| R-5 | 色差の過不足 | 強さを 3 段用意し、GUI で調整する。定数は 1 か所 |
| R-6 | 1 フレームのコスト | 回数を抑え、キャッシュを再利用する |

採らなかった案:
- `style_runs` に入れる案: キャッシュ設計と衝突する。
- `Editor` に記録を持たせる案: 編集コアに表示専用の状態が入る。
- `request_animation_frame` で連続的に乾かす案: 静止時に GPU を回さないという要求に反する。
- タブごとに保持する案: Issue の「リセット」に反する。

## 10. 未決事項（オーナー判断が要るもの）

| ID | 論点 | 推奨 |
| --- | --- | --- |
| **U-1** | マスタースイッチの既定値 | **ON**。ただし既存テストへの影響は ON の方が大きい |
| **U-2** | Reduce Motion で止める範囲 | **帯だけ**。乾きは動きではないため |
| **U-3** | 外部挿入の扱い | 貼り付け・一括置換・AI は ×。**音声入力は判断待ち**（○ にするなら乾きだけで、帯は出さない） |
| **U-4** | インクの色相 | **明度だけ**（ブルーブラックの青みは見送り） |
| **U-5** | 英文の文末で帯を出すか | **出さない**（P4 候補） |
| **U-6** | 乾燥時間の既定 | **60 秒** |

---

**補足（確認できなかった点）**
- 色の具体的な 16 進値は計算スクリプトの実行が許可されず、出していません。仕様は OKLab の ΔL で定め、テストで固定する形にしました。
- Issue #779 の本文は `gh` の実行が許可されず読めていません。依頼文に書かれた要件を正としています。
- Windows の `SPI_GETCLIENTAREAANIMATION` が既存の feature 設定で使えるかは未確認です。
- 既定 ON で影響を受ける既存テストの洗い出しは、まだしていません。


---

## Commander 追記（採用と進め方）

- 本設計は 2026-11-25 にオーナーが採用を承認した（Issue #779 のコメント参照）。10 章の未決事項はすべて推奨どおりに決定済み: U-1 既定 ON、U-2 Reduce Motion は帯だけ停止、U-3 外部挿入には付けない（音声入力も当面は付けない。付ける場合は乾きだけで帯なし、を将来の拡張とする）、U-4 明度のみ、U-5 英文の文末では帯を出さない、U-6 乾燥 60 秒。
- 対象フェーズは P1（モデル）→ P2（乾き・タイマー・マスタースイッチ）→ P3（帯・Reduce Motion・速さと強さ）。P4 は対象外。
- 本書はこの追記時点では未実装・未検証の設計であり、実装の進捗は PR のコメントと Actions の結果で確認すること。

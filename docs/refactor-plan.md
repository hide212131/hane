# Hane 全面リファクタリング計画

Phase 0〜4 の段階的開発で蓄積した重複・実験遺物・計測スキャフォールドを除去し、
RFP（`docs/rfp.md`）が本来要求する構造へ寄せるための計画。コードは全読了済み。

今後、ファイラーと Markdown 機能を継続追加する前提で、機能追加後に変更すると手戻りが
大きくなる境界を優先する。優先度は次の意味で使う。

- **P0（機能追加前に必須）**: 後回しにすると新機能が現行の行単位モデルや `EditorView` へ密結合する。
- **P1（初期機能追加と並行可）**: 性能と保守性に重要だが、限定的な機能追加を直ちに止めるものではない。
- **P2（後回し可）**: 整理価値はあるが、ファイラー/Markdown 機能の設計を直接左右しない。

---

## 0. 現状診断（実測に基づく無駄の棚卸し）

### A. アーキテクチャ上の負債（最重要）

1. **「1物理行 = 1 VisualBlock」になっており、RFP §9 のブロックモデルではない。**
   - `presentation::present_polished_line` / `paragraph_blocks`、`ui::line::presented_line` は
     すべて物理行単位。コードフェンス・表・リスト項目・引用など複数行にまたがる構造を
     ブロックとして持てない。
   - その穴埋めとして、行ごとに文脈フラグ（`fenced_code_context` / `table_context`）を
     毎フレーム再計算する仕組みが増殖している：
     `ui::view::render` 内のフェンス走査ループ、`fence_before_line`（最大2048行を都度走査）、
     `local_table_context`（最大256行を都度走査）、`cached_line` のブロック種別整合チェック。
   - RFP §9「変更されたブロックだけレイアウトを破棄・他は再利用」「§20 Document→Block→Line→Run
     で影響範囲を狭める」が、行単位モデルのため成立しにくい。**これが下記重複の根本原因。**

2. **Markdown 解析器が二重化している。**
   - `hane-markdown::parse_document` は pulldown-cmark の `into_offset_iter()` で
     ブロック/インライン範囲を正しく取得（RFP §7 準拠）。
   - 一方 `presentation::marker_ranges`（lib.rs:478-584）は `#` / `> ` / `- ` / フェンス /
     `**` / `` ` `` / リンクを**手書きで再走査**してマーカ範囲を再導出している。
     同じ情報を別ロジックで2回計算しており、乖離バグの温床。

3. **表・フェンス判定ヘルパの重複定義。**
   - `is_pipe_row` / `is_table_delimiter` が `markdown`（73-87行）と `presentation`（864-874行）に
     二重定義。
   - フェンス追跡ロジックが3か所に散在：`markdown::parse_block_context`、
     `ui::view::fence_before_line`、`ui::view::render` 内インラインループ。

### B. Phase 実験の遺物（製品パスに残存）

4. **`present_bold` + `parse_bold`** は Phase 0 の「太字だけ」実験。`present_markdown` に
   完全に置換済みだが、`app/main.rs` の `HANE_PHASE0_BACKGROUND_PRESENTATION`
   合成負荷生成でのみ生存（product 描画には未使用）。

5. **`line_spans` + `LineSpan`**（presentation lib.rs:240-300）は製品未使用（確認済み）。
   UI は `ui::line::line_segments` を使う。テストのみが参照する死蔵コード。

### C. 計測スキャフォールドが製品コードに混入

6. `benchmark::process_memory_bytes` は `metrics::process_memory_bytes` をそのまま呼ぶだけの
   無意味な間接層。`app` はこの1関数のためだけに `hane-benchmark` へ依存。
   `metrics` と `benchmark` は `Distribution`/`percentile` 系も重複気味。

7. `EditorView` に計測専用メソッドが多数寄生：
   `set_cursor_offset_for_measurement` / `move_cursor_down_for_development` /
   `enable_display_linked_scroll_measurement` / `apply_phase1_scroll_frame` /
   `apply_phase0_background_presentation` / `record_phase0_idle_memory` / `arm_startup_timing`。
   `phase0_metrics.rs` と併せ、製品型に計測関心が張り付いている。

8. `app/main.rs` が **20種以上の `HANE_*` 環境変数分岐**で埋まっている
   （PHASE0/1/2_AUTOSCROLL の別名エイリアス、MEASUREMENT_*, DEV_CURSOR_*, NO_FOCUS,
   BACKGROUND_PRESENTATION 等）。

9. **フェーズ別スクリプト/ドキュメントの複製**：
   `capture_phase2/3/4.sh`、`measure_phase1..4{,_memory}.sh`（ほぼ同一を phase 数だけ複製）、
   ADR 18本のうち 0010/0012/0015/0016/0017 は各フェーズの実装計画（歴史的）、
   `docs/phase0..4/report.md` 5本。

---

## リファクタリング方針

- **各フェーズ独立でコミット可能／リリース可能**にする。高リスクな構造変更は
  R3.5 と R4A〜R4C に分割し、各段階で動作する状態を保つ。
- **すべてのフェーズを R0 のテスト+ベンチ基準線で回帰ゲートする。** RFP §16-18 の
  `keystroke_to_paint` p95/p99 を最重要指標として、前後で悪化させない。
- 低リスク（削除・統合）→ 高リスク（構造変更）の順。R1〜R3 は BlockIndex と描画移行を
  安全にするための地ならし。
- Markdown の正式解析は pulldown-cmark を唯一の意味解析器とする。ただし、イベントの
  source range だけでは得られない開閉マーカ位置は、イベント範囲内に限定した字句解析で補う。
- 背景解析が現在 revision に追いついていない間も入力を止めない。正式 BlockIndex と、
  active block 周辺の暫定表示経路を分けて設計する。

---

## Phase R0 — 基準線の確立（P0・変更なし・安全網）

**目的**: 後続の全変更を測るための不変の物差しを作る。

- [x] `cargo test --workspace` / `cargo clippy --workspace -- -D warnings` が緑であることを確認・記録。
      結果と実行環境は `docs/baseline/README.md` に保存。
- [x] 現行の性能数値（1/10/100 MB, 10万段落）を `measure_phase*` で採取し
      `docs/refactor-plan.md` の付録か `docs/baseline/` に固定保存（回帰比較の原本）。
- [x] 10万段落 fixture を UI 性能シナリオへ追加し、先頭・中央・末尾での入力、
      スクロール、入力しながらのスクロールを採取する。
      AC接続条件の集計結果を `docs/baseline/ui-results.md` に保存。
- [x] `file_open_time` / `local_parse_time` / `full_parse_time` / `layout_time` /
      cache hit/miss / block-index update time を記録し、回帰原因を切り分けられるようにする。
      現行の計測値と未実装項目を `docs/baseline/README.md` に明記し、構造導入時に追加する。
- [x] 測定機、電源状態、refresh rate、build profile、サンプル数を固定し、
      「完全不変」ではなく許容回帰率とばらつきの判定方法を明記する。
      条件と判定方法は `docs/baseline/README.md` に定義。
- [x] 現行の公開 API 一覧（`cargo public-api` 等）をスナップショット。
      `docs/baseline/public-api.md` に保存。
- [x] 基準タグ `refactor-baseline` を打つ（`0111cdc`）。

**完了条件**: 以降のフェーズで参照する「緑のテスト・性能原本・APIスナップショット」が揃う。

---

## Phase R0.5 — 編集・表示契約テストの固定（P0・安全網）

**目的**: ブロック境界や描画実装を変更しても守るべき振る舞いを、構造変更より先に固定する。

- [x] 文書内の全編集可能 source offset について、`source → visual → source` の往復を検証する。
      ASCII、日本語、絵文字、結合文字、サロゲートペア相当の UTF-16 変換を含める。
- [x] 複数行の quote/list/code/table、Setext heading、`1)` 形式の番号付きリスト、
      reference link、escape 済みマーカを golden fixture として追加する。
- [x] 開きフェンスの追加・削除により遠方のブロック境界が変化するケースを追加する。
- [x] 背景 parse 中の連続編集、stale revision の破棄、正式解析待ちの暫定表示をテストする。
- [x] カーソル上下移動、クリック、ドラッグ選択、IME marked range を、複数行ブロックでも
      検証できる UI 非依存の契約テスト API を用意する。

契約テストは `crates/markdown/tests/document_contract.rs`、
`crates/presentation/tests/source_map_contract.rs`、`crates/editor/tests/ime_contract.rs` に置く。
現行モデルで未対応の期待値は `docs/baseline/unsupported-markdown.md` に固定する。

**完了条件**: R4 系フェーズで内部型を置換しても再利用できる契約テストが緑。既知の未対応構文は
期待値を曖昧にせず、明示的な pending/unsupported 一覧へ分離されている。

---

## Phase R1 — 死蔵・重複コードの削除（P0・低リスク）

**目的**: 誰も使っていない／二重定義のコードを消し、以降の見通しを上げる。

- [x] `presentation::present_bold` / `markdown::parse_bold` を削除。
      `app/main.rs` の Phase 0 合成負荷は、現在の背景解析を代表する `parse_document` /
      `present_markdown` workload として再定義する。旧 `present_bold` と処理量が異なるため、
      このシナリオだけは R1 で新しい基準線を取り、以後の比較原本とする。
      新基準は `docs/baseline/r1-background-workload.md` に保存。
- [x] `presentation::line_spans` / `LineSpan` と関連テストを削除（製品未使用を確認済み）。
- [x] `is_pipe_row` / `is_table_delimiter` を `markdown` に一本化し `pub` 化。
      `presentation` / `ui` はそれを import。`presentation` 側の重複定義と
      `is_alignment_cell` を削除。
- [x] `benchmark::process_memory_bytes` の間接層を削除。`app` は `hane-metrics` に直接依存。
      `gpui_baseline` example も直接参照へ変更し、`Cargo.toml` から不要になった
      `hane-benchmark` 依存を除去。

**完了条件**: R0 のテストが緑で、再定義した背景 workload 以外は性能が許容範囲内。
ワークスペースの LOC と重複関数が明確に減少。

---

## Phase R2 — 計測ハーネスを製品コードから分離（P1/P2・低〜中リスク）

**目的**: 「速さの証明」に必要な計測を残しつつ、製品型・製品バイナリから計測関心を剥がす。

- [x] 計測専用の入口を集約：`EditorView` の `*_for_measurement` / `*_for_development` /
      `phase0/1` 系メソッド、CSV 出力、合成入力、自動スクロールを feature フラグ
      `instrument` 配下へ隔離。UI 側は `crates/ui/src/instrument.rs`（計測状態・CSV・env 解釈）と
      `EditorView` の `#[cfg(feature="instrument")]` impl に集約し、製品ビルドは no-op stub のみ。
      app 側の合成入力ハーネスは `crates/app/src/instrument.rs` へ分離。既定 `cargo build -p hane`
      には CSV・合成入力・開発操作 API が含まれない（optional 依存 markdown/metrics/presentation/document
      も instrument 配下）。
- [ ] `keystroke_to_paint` 等の低オーバーヘッド timing hook は製品と同じ入力・paint 経路に残す。
      instrument on/off の差を R0 基準線で測り、計測ビルドだけ別の挙動にならないことを確認する。
      （コード上は `FrameMetrics` 記録を両ビルドの同一 paint 経路に維持済み。on/off の実測比較は未実施。）
- [x] `HANE_*` 環境変数を1か所（`ui::instrument::InstrumentationConfig::from_environment`）で解釈するよう集約。
      `PHASE0/1/2_AUTOSCROLL` → `HANE_AUTOSCROLL`、`PHASE0_BACKGROUND_PRESENTATION` → `HANE_BACKGROUND_PRESENTATION`、
      `PHASE0_NO_FOCUS` → `HANE_NO_FOCUS` に統一。`scripts/` も追従（計測ビルドは `--features instrument`）。
- [ ] `metrics` と `benchmark` の役割を再定義：ランタイム計測=`metrics`、
      オフライン集計/フィクスチャ=`benchmark` に線引きし、重複 `Distribution`/`percentile` を統合。
- [ ] **P1** スクリプトを引数化して統合：`scripts/measure.sh <scenario>` /
      `scripts/capture.sh <scenario>` の2本に集約し、`measure_phase*` / `capture_phase*` を廃止。
- [ ] **P2** 歴史的ドキュメントを整理：`docs/phase*/report.md` と実装計画 ADR を
      `docs/history/` へ移動し、`docs/adr/README.md` に「現行 vs 歴史」の索引を明記。

**完了条件**: 既定 `cargo build -p hane` に合成入力・CSV・開発操作用 API が含まれない。
同じ timing hook を通る製品ビルドと instrument ビルドの性能差が、R0 で定めた許容範囲内。

---

## Phase R3 — Markdown 解析の単一化（P0・中リスク）

**目的**: Markdown の意味解析を pulldown-cmark に統一し、マーカ導出を `hane-markdown` の
構文イベント連動 lexer に集約する（RFP §6.1/§7）。

- [x] `hane-markdown` を拡張し、pulldown-cmark のイベントと source range を正として、
      その範囲内だけを字句解析する。見出し、引用、リスト、フェンス、強調、コード、リンクの
      開閉マーカを `MarkdownParse.markers` として返す（`derive_markers`）。
- [x] `presentation::marker_ranges` と関連手書きロジックを削除し、
      `hane-markdown` の `markers` を消費するだけにする。`fence_delimiter` 依存も除去。
- [~] CommonMark/GFM fixture を追加。ATX heading、`1.`、inline link、強調/コード/取り消し線/
      引用/箇条書きの開閉マーカ、異なる delimiter run を検証済み。
      Setext heading、`1)`、reference link、autolink、escape は現行導出のまま未網羅
      （挙動は移設前と不変。R3.25 の構造化ノード導入時に fixture を拡充する）。
- [x] フェンス/表文脈を `hane-markdown` の有界同期フォールバック（`local_block_context`）へ一元化。
      `ui::view::fence_before_line` / `local_table_context` / `render` 内インラインフェンスループを
      廃止し、背景 `BlockContextIndex` ＋1か所だけのフォールバックに統一。

**完了条件**: Markdown の意味解析が pulldown-cmark の1経路、マーカ字句解析が
`hane-markdown` の1経路のみ。R0.5 の往復・構文 fixture が緑。解析は依然バックグラウンドで、
入力は待たない。

---

## Phase R3.25 — Markdown 機能拡張用の解析・表示契約（P0・中リスク）

**目的**: Markdown 機能を追加するたびに parser/presentation/ui の型と分岐を個別増築せずに済む
境界を、追加機能の実装前に確定する。

- [x] `MarkdownParse` の flat な `blocks` / `spans` を `MarkdownTree`（block/inline node ツリー）へ置換。
      全ノードが parent / children / document 順 / depth / source range を持ち、list→item→paragraph、
      quote→paragraph、table→head/row→cell、task list の checkbox 状態を表現できる。
      未モデル構文は `NodeKind::Unsupported` として range を保持する。
- [x] parser が返す構文種別（`markdown::NodeKind`）、presentation が返す表示種別
      （`presentation::BlockKind` / `StyleKind`）、UI の描画方針（`BlockDisplay` / `InlineDisplay`）を
      3層に分離。UI crate に `NodeKind` / `BlockKind` / `StyleKind` の出現はなく、文字列判定もない。
- [x] `Unsupported` / raw-source fallback を正式な表示契約に含める。未実装構文でも source を失わず、
      編集・保存・カーソル移動が継続できるようにする。
- [x] Markdown feature ごとの fixture を共通形式（`presentation/tests/support`）にし、parse tree、
      marker ranges、SourceMap、disclosure、保存後 source bytes を同じケースで検証する。
- [x] 初期の拡張対象（task list、nested list、複数行 quote/code、image、table、link）で API を試し、
      feature 固有の情報が `EditorView` へ漏れないことを確認した（UI crate の変更は不要）。

**完了条件**: 新しい Markdown 構文を追加するときの主な変更先が `markdown` と `presentation` の
feature 実装・fixture に限定され、既存構文の共通 SourceMap/編集経路を複製しない。

---

## Phase R3.5 — revision 付き BlockIndex の導入（P0・中〜高リスク）

**目的**: UI をブロック描画へ移す前に、Markdown ブロック境界と編集差分を管理する土台を作る。

- [x] `BlockIndex` に stable block ID、kind、source range、revision を保持し、
      byte offset ↔ block、block ordinal ↔ source range を対数時間または同等の計算量で引けるようにする。
      block は文書を tile し（block 間の空行は上の block に属する）、絶対 offset ではなく byte 長を
      chunk 単位の Fenwick tree で保持する。ADR-0018。
- [x] 編集時の再解析開始点と、旧ブロック列へ再同期したと判定する条件を定義する。
      フェンス等で再同期できない場合は、後続を保守的に invalidation する。
      窓 = dirty run の前後1 block、再同期 = 窓の最後の block が既存の終端 block と同じ
      開始位置・種別で終わること、打ち切り = 256 KiB / 512 block。
- [x] 非交差ブロックを revision delta で rebase し、影響ブロックだけを置換する API を実装する。
      （`BlockIndex::update`。byte 長保持のため非交差 block は書き込みなしで移動する）
- [x] 背景の正式解析結果、active block 周辺の暫定解析結果、現在 document revision の
      publish 優先順位を定義し、stale result が表示を上書きしないようにする。
      （`BlockIndexState::publish` / `apply_edits`）
- [x] BlockIndex 更新時間、再解析バイト数、invalidated block 数を計測する。
      （`BlockIndexUpdate` → metrics CSV の `block_index` record と `hane-bench` シナリオ）

**完了条件**: UI はまだ行描画のままでも、各行が所属する正式/暫定 block を BlockIndex から取得できる。
遠方へ影響する編集を含む R0.5 テストが緑で、通常の局所編集が文書サイズ比例の同期処理を発生させない。

**完了（2026-08-27）**: `EditorView::block_at_line` が行→block を返し、背景 job が Formal 索引を
publish、入力経路が増分更新する。100k block の文書で打鍵 median 2.5 µs / p95 4.2 µs、
block 分割 median 5.4 µs / p95 6.4 µs、1編集あたりの再解析は 1 KiB 未満。

---

## Phase R3.75 — DocumentSession と FileService の分離（P0・中リスク）

**目的**: ファイラー追加前に、ファイル状態・永続化・最近使ったファイルを描画主体の
`EditorView` から分離し、複数ファイル/選択切替へ拡張できるようにする。

- [ ] `DocumentSession`（document/editor、path、dirty/saved revision、save generation、表示状態）と
      `EditorView`（GPUI 入出力・描画）を分離する。
- [ ] open/save/save-as/autosave/atomic write を `FileService` または同等の I/O 境界へ集約し、
      UI は request/result を扱うだけにする。ファイル I/O は入力処理を待たせない。
- [ ] canonical path と表示名を分離し、同一ファイル判定、rename/move、削除、外部変更、
      読み込み失敗を表現できる `FileIdentity` を定義する。
- [ ] Recent Files と将来の filer tree state を分け、永続設定が `EditorView` のライフサイクルに
      依存しない repository/store API にする。
- [ ] 画像等の相対 resource 解決を `EditorView` の `document_directory` 計算から分離し、
      session/file identity を基準に解決する `ResourceResolver` を用意する。
- [ ] filer が発行する open/rename/move/delete と、未保存 session、autosave、外部変更の競合規則を
      UI 非依存テストで固定する。

**完了条件**: `EditorView` が `PathBuf`、atomic save、recent-files 永続化を直接所有しない。
単一 session の現行挙動を保ったまま、複数 `DocumentSession` を保持・切替できる API が成立する。

---

## Phase R4A — ブロック単位の仮想化と描画（P0・高リスク）

**目的**: R3.5 の BlockIndex を使い、「1物理行=1ブロック」を Markdown ブロック単位へ移す。

- [ ] `VisualBlock` を Markdown ブロック（Heading/Paragraph/List/Quote/Code/Image/Table）単位に。
      1ブロックが**複数行にまたがる source_range** を保持し、
      `style_runs` / `revision` を保持（RFP §9）。
- [ ] `EditorView::render` / `cached_line` を「行イテレート」から「ブロックイテレート」へ改修。
      `HeightIndex` はブロック高さで駆動。可変高さ仮想スクロールをブロック粒度で実装（RFP §10）。
- [ ] UI の `fenced_code_context` / `table_context` / `local_table_context` を撤廃し、
      正式/暫定 BlockIndex の block kind だけを消費する。
- [ ] block source range 内の物理行を描画する互換レイヤを設け、R4A ではカーソル・選択・IME の
      既存挙動を維持したまま仮想化の単位だけを変更する。

**完了条件**: 画面外の GPUI 要素が block 数に比例して生成されない。100 MB / 10万段落で
入力遅延とスクロールが R0 の許容範囲内。R0.5 の既存編集契約がすべて緑。

---

## Phase R4B — Block → LayoutLine → Run レイアウト（P0・高リスク）

**目的**: 複数行ブロックと折り返しを、カーソル・選択・IME と整合する表示座標系へ移す。

- [ ] `LayoutLine` に block-local visual range、source mapping、y/height、shape result を保持する。
- [ ] source offset ↔ block/LayoutLine/x/y の双方向変換を実装する。
- [ ] 上下移動を物理行の grapheme column 依存から、layout 上の preferred x へ移行する。
- [ ] クリック、ドラッグ選択、複数 LayoutLine をまたぐ選択、IME marked range、カーソル矩形を
      新しい座標変換へ統一する。
- [ ] soft wrap と明示改行を区別し、block 内の source↔visual 往復を R0.5 テストへ追加する。

**完了条件**: 複数行 quote/list/code/table と折り返し段落で、上下移動・クリック・選択・IME が成立。
全編集可能 source offset の往復テストが緑。

---

## Phase R4C — レイアウトキャッシュと差分更新（P1・高リスク）

**目的**: RFP §9/§20 の「変更されたブロックだけ再レイアウト」を実際の描画経路で成立させる。

- [ ] VisualBlock または別の cache entry に layout result、幅、theme/font revision、document revision を保持する。
- [ ] 編集、viewport 幅、theme/font、画像高さの変化ごとに invalidation 条件を明文化する。
- [ ] 非交差ブロックは BlockIndex とともに rebase し、shape/layout result を再利用する。
- [ ] 実測高さ更新時の HeightIndex 差分更新と scroll anchoring を実装する。
- [ ] cache hit/miss、再レイアウト block 数、`keystroke_to_paint` を CI/定期性能試験で比較する。

**完了条件**: 非交差ブロックの編集で画面内の無関係な shape/layout が再実行されない。
100 MB / 10万段落の p95/p99 とメモリが R0 の許容範囲内。

> リスク管理：R4A〜R4C はそれぞれ独立コミット可能にする。R0 の性能原本と R0.5 の契約テストを
> ゲートにし、悪化時は次段階へ進まない。変更対象には presentation/ui に加えて、visual 上下移動を
> 担う editor API も含む。

---

## Phase R5 — 型・API 表面の整理（P1/P2・低リスク・仕上げ）

**目的**: 重複語彙とボイラープレートを削り、公開面を最小化。

- [ ] `markdown::BlockKind`/`InlineKind` と `presentation::BlockKind`/`StyleKind` の関係を再整理。
      変換関数（`presentation_block_kind` / `presentation_style_kind` 等）の重複を削減。
- [ ] `line_segments` / `visual_offset_at_x` のスタイル境界分割ロジック（現状ほぼ同型が複数）を
      1つの共有ヘルパへ統合。
- [ ] clippy pedantic を通し、`pub` 過多・未使用エクスポートを整理（R0 の API スナップショットと diff）。
- [ ] README / ADR を現行構造に合わせて更新。

**完了条件**: 公開 API が意図的に絞られ、clippy pedantic 緑、ドキュメントが実装と一致。

---

## フェーズ依存関係とリスク

```
R0 基準線 ─ R0.5 契約テスト
  └─ R1 削除
       ├─ R3 解析単一化 ─ R3.25 Markdown契約 ─ R3.5 BlockIndex
       │                                             └─ R4A ブロック描画
       │                                                  └─ R4B 複数行レイアウト
       │                                                       └─ R4C 差分キャッシュ
       ├─ R3.75 DocumentSession/FileService
       └─ R2 計測分離

  P0/P1 の必要経路完了 ─ R5 仕上げ
```

- **Markdown 機能追加の開始条件**: R0〜R1、R3、R3.25、R3.5 が完了していること。
  複数行表示や折り返しを伴う機能は R4A〜R4B 完了後に追加する。
- **ファイラー実装の開始条件**: R0〜R1 と R3.75 が完了していること。filer UI は
  `FileService` / `DocumentSession` の利用者とし、ファイル I/O を直接実装しない。
- R0.5 は構造変更前の振る舞いを固定する。R1〜R3 は BlockIndex 導入前の重複と責務を整理する。
- R3.25 で Markdown 拡張契約、R3.5 で source 上のブロック境界、R3.75 でファイル境界を確定する。
- R4A で仮想化、R4B で visual 座標、R4C で再利用を順に成立させる。
- どのフェーズも R0 のテスト+性能原本で回帰ゲートする。特に `keystroke_to_paint` を死守。
- R4A〜R4C は高リスクだが、各段階で動作する状態を維持し、問題の所在を切り分けられるようにする。

## 機能追加前の実行優先順

1. **P0共通基盤**: R0 → R0.5 → R1
2. **Markdown先行基盤**: R3 → R3.25 → R3.5 → R4A → R4B
3. **ファイラー先行基盤**: R3.75（R1後、Markdown系と並行可能）
4. **性能仕上げ**: R4C
5. **整理作業**: R2 の残作業 → R5

各機能に対応する P0 開始条件の完了前は、その新規 feature/UI を本実装しない。必要な調査・spike は
許可するが、製品経路へ新しい文字列判定、`EditorView` のファイル状態、物理行前提の表示分岐を追加しない。

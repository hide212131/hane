# Issue #587: Mermaid図表示の実装仕様

## 目的と現在の状態

仕事の契約は [Issue #587](https://github.com/hide212131/hane/issues/587)。本書は実装担当が同じbranchから読める具体的仕様であり、状態台帳ではない。

本書を最初に作成した時点では、製品コード・テストは未変更で、表示・入力・ビルド・実画面・性能は未検証。基準mainは `3f1f7805ca46507199dae8f8268329e58291d763`。現在の実装・検証状態はPRとcurrent headの証拠で確認する。

調査時の関連open PR #586はフォルダ削除を扱い、本文の構文・表示・配置は別責務。EditorViewの文書切替・破棄が共有境界なので、統合時に最新差分を再確認する。#306/#307/#368/#567の全完了は待たず、受入済みの現行経路へ追加する。原文と位置対応の契約は以下で維持する。リファクタリング、workflow、vendor patch、保存方式の変更は混ぜない。

## 利用者から見た動き

通常のMarkdownコード囲みの言語名が `mermaid` の場合に対応する。最初の言語トークンで識別し、余分な空白や大文字小文字は正規化するが、原文は変更しない。バッククォートとチルダは既存Markdownパーサの判定に従う。独自の行頭正規表現でMarkdownを再解析しない。

最初の対象図種は `flowchart` / `graph`、`sequenceDiagram`、`classDiagram`、`stateDiagram-v2`、`erDiagram`。上位コンテナの解釈は既存パーサに従い、引用・リスト内のコードを別文書として誤認識しない。複合した配置を図として安全に示せない場合も、原文と理由を保ち、対応範囲を文書に明記する。

| 状況 | 表示と操作 |
| --- | --- |
| 編集モードで対象ブロックに入力・選択・検索位置がない | 正常な描画結果があるときだけ、コード全体の代わりに一つの図を表示する。生成中は原文を残す。 |
| 図をクリックする | その図の本文先頭を編集位置にし、囲みも含む全原文を表示する。同じクリックを別行への編集として再解釈しない。 |
| カーソル、選択、日本語変換範囲、現在の検索結果がブロックへ入る | 既存のdisclosure判定をブロック全体に適用する。途中の行だけを図と原文の混在状態にしない。 |
| 編集位置が外れる | 内容が変わっていなければ結果を再利用する。変わっていれば背景で再生成し、完了までは原文を残す。 |
| 閲覧モード | 図を表示し、既存の編集禁止と読み位置を保つ。モード切替で文書を変更しない。 |
| 記述ミス・未対応・上限超過・未閉鎖の囲み | 原文を表示したまま短い理由を添える。空の図・前の内容の図を成功として出さない。 |

図の表示を切り替えても、Markdown文字列、改行、文書のdirty状態、undo/redo、保存形式を変えない。画像/PDF書き出し、図形のドラッグ編集、専用設定画面は対象外。

## 使う既存経路

| 責務 | current mainの場所 | 追加内容 |
| --- | --- | --- |
| 正式な構文 | `crates/markdown/src/parser.rs`、`syntax.rs`、`block_index.rs` | `Tag::CodeBlock`から言語指定、囲みの範囲、本文、終端の成立を得る。既存のtable/fence projectionにならい、改訂番号と結び付く補助情報として渡す。 |
| 表示方針・原文対応 | `crates/presentation/src/display.rs`、`projection/block.rs`、`source_map.rs` | 図の所有行と元の各物理行を関連付ける。表示状態はこの層からUIへ渡す。 |
| 高さ・位置 | `crates/presentation/src/layout.rs`、`height_index.rs` | 図の高さと畳まれた行を一つの位置計算へ反映する。描画だけで余分な高さを作らない。 |
| 背景処理・保持 | `crates/ui/src/view.rs`、`view/background_parse.rs`、新しい小さなMermaid専用module | 既存executorと文書実体の確認を使い、描画要求・キャンセル・結果受入・保持上限をここに閉じる。 |
| 画面と入力 | `crates/ui/src/line.rs`、`view/render.rs`、`view/reading_mode.rs` | 既存のDisclosurePolicy、block_layout、row_element、読書位置補正、イベント配送へ接続する。 |

現在の `NodeKind::CodeBlock` は言語名を保持しない。`NodeKind`のCopy前提を広く変えるより、既存の補助projectionへ情報を追加する。解析が古い、部分更新で安全な範囲対応を示せない、囲みが未成立の場合は通常コードへ戻る。

現在の画像経路 `VisualLine.image` と `img(resolved)` は一物理行に対する既存画像用であり、その宛先へMermaid本文を流し込まない。生成SVGはメモリ上の画像として扱い、通常画像のResourceResolver、保存先、外部URL読込とは分離する。ビルド対象は `gpui-pre = 0.3.6`。`vendor/gpui`を読んだだけでAPIが利用可能と判断しない。

## 複数行を一つの図にする際の不変条件

`VisualBlock.lines`は物理行と対応し、`EditorView::disclosures_are_current`は `block.span + lines_before + lines.len()`から範囲を求めている。**原文の行を単に削って一行へ置き換えない。**

原文各行、source range、ブロックの同一性は維持する。図の所有行に図の高さを持たせ、それ以外の図内行は表示上だけ畳む方針を基本とする。既存のフェンス行を畳む処理と同様、layout/height/source→point/point→source/上下移動へ同じ表示方針を渡す。新しい図専用の座標計算をUIだけに持たせない。

クリックは図内の高さ割合から原文の行を推測せず、図の本文先頭へ決定的に移動する。キーボード移動や検索が原文内へ入る場合は、原文を開いてから位置を再計算する。選択と日本語変換が関係する間は畳まない。

可視範囲がコードブロック途中から始まる場合も、図の所有範囲と全体の高さを失わない。囲みの始点を可視範囲内で発見したときだけ図を認識する実装にしない。通常コード、table、画像、見出しの位置対応は変更しない。

## 描画ライブラリとAPI

ブラウザ/JavaScriptを必須にしない現行方針に従い、`merman` 0.9.2を固定して使う。今回のfeatureは以下。既定で含まれる全図種、ELK、Cytoscape、数式、CLI、PNG書き出しは有効にしない。

```toml
merman = { version = "=0.9.2", default-features = false, features = [
    "svg", "diagram-flowchart", "diagram-sequence",
    "diagram-class", "diagram-state", "diagram-er",
] }
```

本体はMIT OR Apache-2.0。選択した依存と第三者表示を実際の依存解決結果で確認する。

### 依存の事前検証と必要な更新

同じmainの隔離コピーを使い、公式Rust/Cargo 1.98.1で依存を解決した。Merman 0.9.2は `unicode-segmentation ^1.13.3`を要求し、Haneの `=1.12.0`とはそのまま併用できなかった。Mermanを導入する変更に限り、workspaceの固定値を `=1.13.3`へ更新する。

直接依存はworkspaceに上記Merman指定、`hane-ui`に `merman.workspace = true`、`roxmltree = "=0.21.1"`、`serde_json = "=1.0.154"`を追加する。XMLの寸法・画像要素確認と、描画前のテーマ設定に使う。機械生成したlockfileでは既存の更新は以下5件に限られ、新しいpackageは36件だった。lockfileに載るoptional packageがすべて実際のビルドへ入るとは扱わない。

| 既存package | 更新 |
| --- | --- |
| unicode-segmentation | 1.12.0 → 1.13.3 |
| serde_json | 1.0.151 → 1.0.154 |
| indexmap | 2.14.0 → 2.14.2 |
| thiserror | 2.0.20 → 2.0.21 |
| thiserror-impl | 2.0.20 → 2.0.21 |

変更前・上記依存だけを変えたコピーの両方で、`cargo test --locked -p hane-document -p hane-editor -p hane-markdown -p hane-presentation --all-features` の既存376テストが成功した。製品コードやテスト期待値は変更していない。これは既存4crateの事前確認で、Mermaid実装・UI・全workspace・実画面の合格ではない。

unicode-segmentation更新にはUnicode 16→17のデータと境界修正が含まれる。既存テストには同じライブラリから期待境界を求めるものもあるため、上記の成功を全入力で境界が以前と同一だった根拠にしない。本文と短い入力欄の日本語・結合文字・複合絵文字・CRLFの移動/選択/削除をcurrent-head回帰と実画面で確認する。文書を新しい形へ自動変換する処理は加えない。

依存差分は初回引き継ぎに公式Cargoの生成結果として添付する。元Cargo.lockのSHA-256は `a8ecc3a4194b4b27ba98b08d334cd04972c813156639aba3d3e7c2b8cf968208`、生成後は `10ae618999b7178be0547fabeaea81bde85bb128f6e0743107636c23abf3c8eb`、manifestとlockの結合差分は `c6891175ac7f0e8fb225b84f0a773c03192ce750c78ed56aa49ec76809850f66`。無関係な既存依存の一括更新は行わない。

Cargo.tomlだけを変えてCargo.lockを未更新にしない。lockfileを推測で手書きしたり、CIの`--locked`を外したりしない。通常workerはshell/Cargoを実行できないため、固定したmanifestに対して公式Cargoで生成した依存差分を受け渡し、製品変更と一緒に適用する。生成元、Cargo版、指定内容、差分を記録し、生成結果を受け取る前に整合済みと報告しない。

v0.9.2で確認したAPIの要点（本書の段階でコンパイルは未実施）:

```rust
use merman::svg::{RenderResourcePolicy, SvgPipeline};
use merman::{
    OperationControl, ParseOptions, RenderOutput, RenderRequest,
    Renderer, SvgEnvironment, SvgRequest,
};

let renderer = Renderer::new().with_parse_options(ParseOptions::strict());
let request = SvgRequest {
    environment: SvgEnvironment::deterministic()
        .with_resource_policy(RenderResourcePolicy::interactive()),
    pipeline: Some(SvgPipeline::resvg_safe()),
    ..SvgRequest::default()
};
// UIスレッド外で、操作ごとのcontrolを渡す。
let output = renderer.render(RenderRequest::svg(source, control, request));
// RenderOutput::Svg(Some(svg)) の svg.into_parts().0 がSVG文字列。
// Svg(None)、NoDiagram、エラーを表示成功としない。
```

`resvg_safe()`はHTMLを含むラベル等を既存のSVG描画で扱える形にし、外部参照等を検査する。検査後の文字列置換でSVGを書き換えない。色・背景・フォントは描画前の設定へ渡す。図の記述から外部リソース・スクリプト・クリックcallbackを実行しない。

未対応図種は `RenderError::Parse(diagnostic)` の `terminal_diagnostic_details().code == "merman.parse.unsupported_diagram"`で区別できる。`RenderError`はnon_exhaustive。`Cancelled`は古い処理として捨て、`ResourceLimitExceeded`は原文と短い理由へ戻す。

### 日本語と図の大きさ

日本語を含むSVG文字列ができたことだけでは合格にしない。図の文字幅計算と、GPUIが実際に描くフォントの両方を確認する。既定のMermanの文字幅計算は実フォントと一致するとは限らない。

まず既存GPUIの日本語フォント経路で確認する。実測が必要なら `DeterministicTextMeasurer::with_width_callback(...)` とGPUIのスレッド条件を確認する。`native-fonts`/`system-fonts`は新しいresvg/usvg系依存を増やすため、必要性を報告せず自動追加しない。ラベルのはみ出しや欠落を既知制限として通常の日本語受入から除外しない。

図は縦横比を保って本文幅へ収める。SVGの自然寸法、画面幅、倍率からlayoutの高さを決め、描画と同じ値を使う。寸法が非有限・ゼロ・上限外なら原文へ戻す。文字や線が空白になる画像APIを使わない。


### GPUI 0.3.6への接続と画像の破棄

使用中の公開crateのchecksumをCargo.lockと照合して確認したAPIは次のとおり。

- `App::svg_renderer(&self) -> SvgRenderer`。Clone + Send + Syncなので、背景処理で使える。
- `SvgRenderer::parse_svg(&self, bytes: &[u8]) -> Result<ParsedSvg, usvg::Error>`。
- `SvgRenderer::render_parsed(&self, svg: &ParsedSvg, size: impl Into<SvgSize>) -> Result<Arc<RenderImage>, usvg::Error>`。
- `gpui::img(Arc<RenderImage>)`で多色の図を描く。アイコン用の `gpui::svg()` は使わない。
- `App::drop_image(&mut self, image: Arc<RenderImage>, current_window: Option<&mut Window>)`でウィンドウ側の保持も解除する。

`SvgSize::Size`は幅だけを採用して縦横比を保ち、height引数は高さ上限にならない。`ExactSize`なら指定どおりになるので、Hane側で縦横比を保つ寸法を先に計算する。`ScaleFactor`の内部2倍化と、内部上限8192pxだけに依存しない。SVGのルート寸法/viewBoxはXMLとして確認し、文字列の部分一致で推測しない。

一枚の画像は最大辺2048px・最大4,194,304画素、保持合計は32 MiBを初期上限とする。画素メモリは幅×高さ×4で数え、表示待ち・表示中・再利用分を含める。同時処理の結果にもこの予算を適用し、入力サイズ上限とは別に扱う。予算に収まらない結果は原文へ戻す。上限超過の追い出し、差し替え、文書/ビューの破棄で `cx.drop_image`を適切なwindow文脈で呼ぶ。古い結果とキャンセルで処理数や画像参照を残さない。

GPUIのusvg 0.46.0は既定でSVGの画像hrefをローカルファイルとして読む。必ずMermanの`resvg_safe()`を通し、その後のSVGを変更せずGPUIへ渡す。このpipelineは外部の画像/paint/filter等の参照を除去するが、承認済みdata URL画像は許す。今回Haneでは埋込画像のデコードも対象外とし、XMLから`image`/`feImage`要素を検出したら原文へ戻す。これで小さいSVG内の巨大な埋込画像も回避する。図内リンクは表示情報にとどめ、既存の文書リンククリック処理へ渡さない。

API一次資料: https://docs.rs/gpui-pre/0.3.6/gpui/struct.SvgRenderer.html

## 非同期処理と保持上限

rendererの初期化と処理は必要時に遅らせ、起動と通常の入力を待たせない。可視範囲付近だけを要求し、毎フレーム同じ図を再生成しない。図の内容、文書実体、ブロック同一性、テーマ/フォント条件を結果のキーに含める。画面幅と倍率をSVG生成へ使った場合はキーへ含め、最終配置だけに使う場合はlayoutだけ更新する。

古くなった処理には保持した `OperationControl` の `cancel()`を呼ぶ。futureを破棄するだけで同期描画が止まるとは仮定しない。完了時も文書実体・内容・表示条件を再確認し、タブ切替、再読込、削除、連続編集後の結果を誤って適用しない。

初期上限は、図の入力64 KiB・512物理行、同時描画2件、保持するSVG合計16 MiB・64件とし、Mermanのinteractive資源制限を併用する。上限判定のために巨大ブロックを先に全量コピーしない。画像の画素数も既存GPUIの扱いを確認して有界にし、SVGが短くても巨大な画像を確保させない。失敗した同一内容を毎フレーム再試行しない。上限はユーザー設定へ増やさず、検証に基づく調整が必要なら根拠を残す。

図の成功、失敗、原文の開閉で高さが変わるときは、既存のblock cache/layout cache/HeightIndexと読み位置補正を一緒に更新する。古い高さが残ることを描画上だけ補正しない。

## 検証

| 境界 | 必要な確認 |
| --- | --- |
| 認識と原文 | 対象5図種、大小文字/空白、チルダ、複数図、未閉鎖、長い囲み、引用/リスト、非Mermaidコード。解析と部分更新で同じ範囲を得る。 |
| 位置と開閉 | クリック、上下移動、検索、選択、IME、原文へ戻る/離れる、文書先頭/末尾、図の前の追記。source→point→sourceと実際の編集が対応する。 |
| 可視範囲と大きさ | 長いブロック途中へのスクロール、複数図、幅/倍率変更、明暗テーマ、図の高さ変更、閲覧↔編集の読み位置。 |
| 非同期 | 古い結果の遅延投入、タブ切替、再読込、削除、同じoffsetに別図、テーマ切替、キャンセルと完了の競合、上限直前/超過。 |
| 永続性 | 未保存の図原文、undo/redo、保存して再読込。表示切替だけで原文・dirty・履歴を変えない。 |
| 表示と性能 | macOS/Windowsの日本語実画面、通常文書の起動/入力/スクロールの変更前後、複数図の初回/再表示と保持メモリ。 |

自動テストは上記の契約と回帰へ絞り、内部構造をなぞるだけのテストは増やさない。現在のworkspace testsとClippy、macOS/Windows required CIを維持する。生成SVG単体のテストをGUIの代わりにしない。

必須の実画面確認は、5図種の日本語、クリックから原文編集、日本語変換、検索、undo/redo、保存再読込、幅/倍率、閲覧切替を同じcurrent headで確認する。Windowsは添付AGENT.mdおよび現行手順に従い、接続されたWindows環境から直接操作する。取得できない証拠は未検証として残す。

通常実装担当はClaude Code。保護パスとworkflowを変更せず、workerでshell/git/gh/test/build/lint/commit/pushを実行しない。trusted finalizer後のcurrent-head検証、レビュー、実画面、mergeの判断はCommander Policyに従う。機能を自動releaseしない。

## 一次資料

- [Merman v0.9.2 Cargo features](https://github.com/Latias94/merman/blob/v0.9.2/crates/merman/Cargo.toml)
- [Rust組み込み](https://github.com/Latias94/merman/blob/v0.9.2/docs/rendering/RUST_EMBEDDING.md)
- [SVG出力の変換と検査](https://github.com/Latias94/merman/blob/v0.9.2/docs/rendering/SVG_OUTPUT_PIPELINE.md)
- [資源制限とキャンセル](https://github.com/Latias94/merman/blob/v0.9.2/docs/integration/RESOURCE_POLICY.md)
- [公開render API](https://github.com/Latias94/merman/blob/v0.9.2/crates/merman/src/render.rs)
- [フォント例](https://github.com/Latias94/merman/blob/v0.9.2/crates/merman/examples/render_svg_monospace.rs)

この環境では既定のJev CLIが存在しなかった。Commander Policy §2.9の返答取得不能時の扱いに従い、current factsから文書準備のactionを選んだ。Jevの判断を取得したとは記録しない。

## 実装の構成と判断（実装worker記録）

実装worker（shell・Cargo・テストを実行できない環境）が記録する。ここに書いた内容はコードを読んで確認した範囲で、ビルド・テスト・実画面の結果ではない。

### 構成

| 責務 | 場所 | 内容 |
| --- | --- | --- |
| 正式な認識 | `crates/markdown/src/mermaid.rs` | ブロック原文をpulldown-cmarkで解析し、トップレベルの単一のfenced code blockで最初の言語語が `mermaid`（ASCII大文字小文字を無視）のときだけ `MermaidFence` を返す。囲みの範囲・本文範囲・本文・閉鎖の有無を絶対offsetで持つ。引用・リスト内、インデント型、別言語は認識しない。開閉行の判定は既存の `fence_delimiter` 系を使う。 |
| 表示方針 | `crates/presentation/src/diagram.rs`、`BlockKind::Diagram` | `apply_diagram` が囲みの各物理行を残したまま、先頭行（開き囲み）に図の高さ、他の囲み行に高さ0を与える。前後の空行は変えない。範囲を保証できない提示（クリップ済み等）は原文のまま。`diagram_display_size` / `diagram_row_height` を配置と描画の両方が使う。 |
| 高さ・位置 | 既存の `layout_block` / `BlockLayout` / `HeightIndex` | 既存の「高さ0の折り畳み行」と同じ行構造なので、source↔point、クリック、上下移動、被覆は通常の行から得る。描画後の測定高さが既存経路で `HeightIndex` に入り、読み位置補正も既存のものを使う。 |
| 生成・保持 | `crates/ui/src/mermaid.rs` | Merman（`=0.9.2`）→ `resvg_safe` SVG → XML検査（ルート寸法、`image`/`feImage` 不在）→ GPUI `SvgRenderer` → `RenderImage`。保持・同時実行・画素の上限と古い結果の破棄。 |
| 接続 | `crates/ui/src/view/mermaid_diagrams.rs`、`line.rs`、`view.rs`、`view/render.rs` | 可視範囲付近の認識と背景ジョブ、図にしてよいかの判定、提示キャッシュとの照合、クリック、描画。 |

### 動作の決め方

- 図にするかは毎フレーム、`line.rs` の既存の行ごとのdisclosure判定（カーソル・選択・日本語変換範囲・現在の検索結果）を囲みの全行に適用して決める。どれか一行でも触れていれば全原文を表示し、途中の行だけが混在することはない。閲覧モードは既存どおりdisclosureが無いため常に図。
- 図は、ブロックの正式な索引（`Confidence::Formal`）があり、現在の本文・テーマ（明暗）に完全一致する描画済み結果があるときだけ出す。それ以外（生成中・失敗・未閉鎖・未対応・上限超過・古い結果）は原文を表示し、失敗は短い理由を右上に添える。
- 編集位置が囲みに入っている間は描画を始めない。外れたときに本文が変わっていれば背景で作り直し、完了まで原文を残す。変わっていなければ結果を再利用する。同じ失敗は本文とテーマが同じ限り再実行しない。
- 古い仕事は `OperationControl::cancel()` とフラグで止め、完了時にトークン・文書実体で照合して古い結果を捨てる。タブ切替・再読込（文書の置換）・可視範囲から外れたブロックは仕事と画像を破棄する。
- クリックは、図の行の原文範囲の末尾（＝本文の先頭行の先頭）に編集位置を置く。図内の高さ割合から行を推測しない。閲覧モードでは何もしない。
- SVGはMermanが返した文字列をそのままGPUIへ渡し、置換しない。色・背景・フォントは描画前に `%%{init: …}%%`（serde_jsonで生成）として原文の前に置く。先頭にfront matterがあるときはその直後に置く。
- 画像は最長辺2048px・最大4,194,304画素。保持画素メモリ（幅×高さ×4）の合計32 MiBを、表示待ち・表示中・再利用分・描画中の結果すべてに適用する。追い出しや文書/ビューの破棄では `cx.drop_image` を呼ぶ（描画フレーム先頭、またはビューのrelease時）。SVGは画像化した時点で捨てるため保持しない（1件最大8 MiB、同時2件なので同時に持つSVGは最大16 MiB）。入力は本文64 KiB・512行、同時描画2件、保持64件。
- 画像は自然寸法の最大2倍で作り、本文幅・表示倍率からの表示寸法（縦横比を保ち、拡大はしない）は配置のたびに計算する。幅や倍率の変更ではSVGを作り直さない。倍率が2倍を超えると画像は拡大表示されるため鮮明さが落ちる。

### 依存

| crate | 目的 | 使用箇所 |
| --- | --- | --- |
| `merman =0.9.2`（svgと5図種のfeatureのみ） | Mermaid→SVG | `hane-ui` |
| `roxmltree =0.21.1` | SVGのルート寸法と `image`/`feImage` の検査 | `hane-ui` |
| `serde_json =1.0.154` | 描画前のテーマ設定JSONの生成 | `hane-ui` |
| `unicode-segmentation =1.13.3` | Merman要件による更新（Unicode 17） | 既存利用箇所 |

### 未検証・既知の制限

実装workerは次を実行・確認していない。current headのCI、レビュー、実画面で別途確認する。

- ビルド、既存・追加テスト、Clippy、`--locked` の通過。特にMerman 0.9.2のAPIは本書の抜粋と事前検証の記述だけを根拠にしており、コンパイルしていない。推測した箇所: `OperationControl::new()` と `Clone`、`RenderRequest::svg` の第1引数を `&str` で渡すこと、`RenderOutput::Svg(Some(svg))` と `svg.into_parts().0` が文字列として扱えること、エラー型を型名で参照せず `Debug` 表記の先頭（`Cancelled` / `ResourceLimitExceeded`）で判別していること。GPUI 0.3.6側は `App::svg_renderer`、`SvgRenderer::{parse_svg, render_parsed}`、`SvgSize::ExactSize`、`App::drop_image`、`Context::on_release` を使う。
- 日本語の実描画（Merman既定の文字幅計算と実フォントの一致、ラベルのはみ出し・欠落）、明暗テーマの見た目、幅・倍率変更、macOS/Windows実画面、性能・メモリ。`native-fonts` / `system-fonts` は追加していない。必要性はこの確認で判断する。
- IME変換中の動作、検索移動、undo/redo、保存・再読込の実画面確認。自動テストは `view/mermaid_diagrams.rs` にあるが、実行していない。
- 依存の第三者ライセンス表示の確認（`cargo tree` / `cargo deny` 相当）。新しいpackageのライセンスは未確認。
- `unicode-segmentation` 更新後の本文・短い入力欄の文字境界は、手で書き下した境界を固定する回帰テスト（`crates/editor/tests/grapheme_contract.rs`、`short_text_input.rs`）を追加したが、実行していない。実画面での確認も残る。
- 対応範囲: 引用・リストの中のMermaidブロックは図にしない（通常コード）。旧記法の `stateDiagram` は対象外で、原文に理由を添える。可視範囲が囲みの途中から始まっても、図にするブロックは常に全行を提示する。キーボードで囲みの中へ入ると原文が開く（クリックと同じ）。図内リンクのクリックは扱わない。画像・PDF書き出し、図形のドラッグ編集、専用設定は対象外。
- 未実装・簡略化: 画面外の図ブロックの高さは、描画されるまでコード表示の見積りのまま（描画後に測定高さへ置き換わり、既存の読み位置補正が働く）。`HeightIndex` の初期見積りを図の高さで先に埋める処理は入れていない。

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

本体はMIT OR Apache-2.0。選択した依存と第三者表示を実際の依存解決結果で確認する。Cargo.tomlだけを変えてCargo.lockを未更新にしない。lockfileを推測で手書きしたり、CIの`--locked`を外したりしない。通常workerはshell/Cargoを実行できないため、固定したmanifestに対して公式Cargoで生成した依存差分を受け渡し、製品変更と一緒に適用する。生成元、Cargo版、指定内容、差分を記録し、生成結果を受け取る前に整合済みと報告しない。

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

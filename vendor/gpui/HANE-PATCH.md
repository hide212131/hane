# Hane の限定パッチ（Issue #101, #126, #228）

crates.io の GPUI 0.2.2 を同梱。Apache-2.0 ライセンスは `LICENSE-APACHE` に保持。
配布 crate の SHA-256: `979b45cfa6ec723b6f42330915a1b3769b930d02b2d505f9697f8ca602bee707`。
製品コードの変更は `src/platform/mac/text_system.rs`（Issue #101）、
`src/platform/mac/platform.rs`（Issue #126）、`src/platform/mac/window.rs` と
`src/platform/mac/events.rs`（Issue #228、macOS のトラックパッド pinch）のみ。
それぞれ独立した理由によるパッチで、互いに依存しない。

## Issue #101: 合成斜体フォールバック

macOS の CoreText は `.SystemUIFont` の英字を斜体にできても、日本語を直立の Hiragino にフォールバックする。GPUI 0.2.2 は解決後の PostScript 名で FontId を再利用するため、Hane が FontStyle::Italic を渡すだけでは字形が変わらない。

- CoreText の属性として斜体の意図を保持し、フォールバック後の各 run へ引き継ぐ。
- 選ばれた書体が斜体を持たない場合だけ、12度の合成斜体を適用する。ネイティブの斜体とカラー絵文字には重ねて適用しない。
- 合成斜体は独立した FontId を持ち、通常字形のキャッシュを汚染しない。書体の features / fallbacks と字送りは維持する。
- 描画と raster bounds に同じせん断変換を適用する。座標の上下方向の違いを反映し、アンチエイリアス用に1ピクセルの余白を取る。空白字形はゼロの描画範囲を維持し、不要なspriteを作らない。
- 解析、ソース範囲、表示ポリシー、カーソルの字送り計算は変更しない。Windows の描画処理は変更しない。OS のフォントを使用し、フォントファイルは同梱しない。

### 検証

macOS の実フォント・CoreText・Core Graphics を使う回帰テスト:

```sh
cargo test --manifest-path vendor/gpui/Cargo.toml --features runtime_shaders --lib hane_oblique
```

通常／太字の漢字・ひらがなについて、解決後 FontId、字形画像、字送り・配置、通常→斜体→通常のキャッシュを比較する。1倍・2倍の倍率とサブピクセル位置を検証し、拡張キャンバスとの比較で文字切れも検出する。英字のネイティブ斜体と絵文字への二重適用を検証する。通常の workspace テストのテスト用フォント実装では実字形を検証できないため、このテストを macOS CI でも実行する。

上流版で既定のシステムフォールバックを含めて同じ問題が解消されたら、実字形の回帰テストと Hane の GUI 確認を通したうえでパッチを削除する。明示的な fallback descriptor に traits を追加するだけの変更では、日本語の合成斜体は解消しない。

## Issue #126: 入力ソース切替後の IME 再同期

Hane 起動後（ウィンドウが既に key / first responder の状態）に日本語入力ソースへ切り替えても、AppKit は現在の first responder の `NSTextInputContext` を自動では再同期しない。Text Services Manager はそのコンテキストが最後に activate されたときの入力ソースのまま composing を続ける（または始めない）ため、Romaji 入力のような変換前提の IME に切り替えても preedit が始まらず、未変換のローマ字がそのまま `insertText:` 経由で確定してしまう。

- `NSTextInputContextKeyboardSelectionDidChangeNotification`（GPUI が既に keyboard layout 変更検知のために監視している通知）のハンドラで、key window の現在の first responder が持つ `NSTextInputContext` を `deactivate` → `activate` し、AppKit の通常の responder 遷移を再現して TSM を新しい入力ソースへ即座に再同期させる。active な context に `activate` だけを直接呼んでも、既存の入力ソースとの紐付けは更新されない。
- key window / first responder / input context が存在しない場合は何もしない。フォーカスが無いときや通常のフォーカス遷移時の動作は変更しない。
- text input context の再入は抑止し、1回の入力ソース変更通知につき再同期を1回だけ行う。
- キーバインド解決用の `MacKeyboardMapper` 再構築ロジックは変更しない。

### 検証

macOS の `platform.rs` test は実際の keyboard-selection-change 通知を `APP_DELEGATE_CLASS` へ配送し、text responder の context に対する1回の `deactivate` → `activate` と、key windowなし・non-text responder・contextなしの安全な no-op を確認する。実際の日本語 IME の composing 状態と OS のフォーカスに依存するため、Hane 側の GUI 検証（`.agents/skills/hane-gui-test`）でも、Hane 起動後に日本語入力ソースへ切り替えてから通常段落・リスト項目へ日本語を入力し、変換結果が一度だけ期待 source offset へ commit されることを確認する。

## Issue #228: macOS トラックパッド pinch のズーム統合

GPUI 0.2.2 はマウスホイール（`NSScrollWheel`）だけを `PlatformInput::ScrollWheel` へ変換しており、トラックパッドの pinch（`-[NSResponder magnifyWithEvent:]`、`NSEventTypeMagnify`）は `NSView` のどのセレクタにも束縛されていない。上流にも独自の pinch 表現は存在しない。Hane のメインパネルを Ctrl/Cmd+wheel と pinch の両方で連続的にズームできるようにするため（Issue #228）、新しい cross-platform `PlatformInput` variant を追加して Windows/Linux/テスト backend にも空の match アームを増やす代わりに、pinch を既存の `ScrollWheelEvent` ストリームへ最小限の変更で合流させる。

- `window.rs`: `NSView` サブクラスに `magnifyWithEvent:` を追加登録し、既存の `scrollWheel:` などと同じ `handle_view_event` へ配送する。
- `events.rs`: `NSEventTypeMagnify` を、`modifiers.control` を強制的に立てた `ScrollWheelEvent` として組み立てる。`-[NSEvent magnification]`（`cocoa` 0.26 の `NSEvent` トレイトには無いため直接 `msg_send!` で読む）はその1イベント分の相対倍率の増分で、Apple 自身のサンプルコードも `scale += event.magnification` という加算前提の量である。これを `PINCH_ZOOM_SCROLL_PIXELS_PER_UNIT`（600px = 倍率1.0分）で合成 `ScrollDelta::Pixels` に変換し、Hane 側の `crates/ui/src/view.rs` の `EditorView::on_scroll` が `ZOOM_WHEEL_SENSITIVITY_PX`（同じ 600px）で割り戻して元の倍率増分に復元する。2つの定数は対になっており、どちらかを変えるときはもう一方も合わせる。
- 通常のマウス/トラックパッド `NSScrollWheel` は変更しない。`modifiers.control` を合成する pinch イベントは、実際に Ctrl キーが押された `NSScrollWheel` と全く同じ経路（`EditorView::on_scroll` の ctrl 分岐）を通る。

### 検証

`cocoa`/AppKit の実イベントは Rust 単体テストから送れないため、macOS 実機での GUI 検証（`.agents/skills/hane-gui-test`）で、トラックパッドの pinch アウト/インでメインパネルが連続的に拡大縮小し、指を離した位置の文書内容が画面上で大きくずれないことを確認する。`crates/ui/src/view.rs` 側の倍率計算・スナップ・アンカー・再レイアウトのロジックは、実際の `ScrollWheelEvent`（`modifiers.control = true`）を合成して駆動する通常の cargo test で検証する。

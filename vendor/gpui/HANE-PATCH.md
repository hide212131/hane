# Hane の限定パッチ（Issue #101）

crates.io の GPUI 0.2.2 を同梱。Apache-2.0 ライセンスは `LICENSE-APACHE` に保持。
配布 crate の SHA-256: `979b45cfa6ec723b6f42330915a1b3769b930d02b2d505f9697f8ca602bee707`。
製品コードの変更は `src/platform/mac/text_system.rs` のみ。

macOS の CoreText は `.SystemUIFont` の英字を斜体にできても、日本語を直立の Hiragino にフォールバックする。GPUI 0.2.2 は解決後の PostScript 名で FontId を再利用するため、Hane が FontStyle::Italic を渡すだけでは字形が変わらない。

- CoreText の属性として斜体の意図を保持し、フォールバック後の各 run へ引き継ぐ。
- 選ばれた書体が斜体を持たない場合だけ、12度の合成斜体を適用する。ネイティブの斜体とカラー絵文字には重ねて適用しない。
- 合成斜体は独立した FontId を持ち、通常字形のキャッシュを汚染しない。書体の features / fallbacks と字送りは維持する。
- 描画と raster bounds に同じせん断変換を適用する。座標の上下方向の違いを反映し、アンチエイリアス用に1ピクセルの余白を取る。空白字形はゼロの描画範囲を維持し、不要なspriteを作らない。
- 解析、ソース範囲、表示ポリシー、カーソルの字送り計算は変更しない。Windows の描画処理は変更しない。OS のフォントを使用し、フォントファイルは同梱しない。

## 検証

macOS の実フォント・CoreText・Core Graphics を使う回帰テスト:

```sh
cargo test --manifest-path vendor/gpui/Cargo.toml --features runtime_shaders --lib hane_oblique
```

通常／太字の漢字・ひらがなについて、解決後 FontId、字形画像、字送り・配置、通常→斜体→通常のキャッシュを比較する。1倍・2倍の倍率とサブピクセル位置を検証し、拡張キャンバスとの比較で文字切れも検出する。英字のネイティブ斜体と絵文字への二重適用を検証する。通常の workspace テストのテスト用フォント実装では実字形を検証できないため、このテストを macOS CI でも実行する。

上流版で既定のシステムフォールバックを含めて同じ問題が解消されたら、実字形の回帰テストと Hane の GUI 確認を通したうえでパッチを削除する。明示的な fallback descriptor に traits を追加するだけの変更では、日本語の合成斜体は解消しない。

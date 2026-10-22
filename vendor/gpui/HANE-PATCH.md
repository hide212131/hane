# Hane の GPUI 0.2.2 パッチ履歴

旧採用版の crates.io GPUI 0.2.2 を履歴資料として同梱する。
Apache-2.0 ライセンスは `LICENSE-APACHE` に保持。
配布 crate の SHA-256: `979b45cfa6ec723b6f42330915a1b3769b930d02b2d505f9697f8ca602bee707`。

## 現行版との関係と差分の範囲

2026-10-23、main `499c44639822e76518a93661e77578b23ff7b72a` で確認した。
現在の workspace は `gpui-pre 0.3.6` を採用し、このディレクトリの
`gpui 0.2.2` は `[patch.crates-io]` と `Cargo.lock` の採用先ではない。
旧コード・ライセンス・検証経緯を参照できるよう保持している。

配布 archive との比較では、既存 source の差分は次の **14ファイル**にある。
当初の合成斜体と入力ソース再同期の2ファイルだけが変更範囲ではない。
パスはこのディレクトリからの相対パスで、test用の変更も含む。

| 差分の責務 | 旧版で変わったファイル | 現行の所有者・確認状況 |
| --- | --- | --- |
| 入力モードの照会・通知 | `src/app.rs`, `src/app/context.rs`, `src/platform.rs`, `src/platform/mac/keyboard.rs`, `src/platform/mac/platform.rs`, `src/platform/windows/events.rs`, `src/platform/windows/platform.rs` | 照会は `crates/ui/src/input_mode.rs`、購読は Hane と GPUI の既存 App subscription。macOS の context 再同期と Windows の IME mode 通知は現行 platform vendor に保持 |
| trackpad の倍率変更と macOS test 接続 | `src/elements/div.rs`, `src/interactive.rs`, `src/platform/mac/events.rs`, `src/platform/mac/window.rs`, `src/window.rs` | 現行 GPUI 標準の `PinchEvent` / `on_pinch` と Hane の `view/viewport.rs` を使用。旧 mac/window の差分には context 再同期 test 用の input handler hook も含む |
| 日本語の合成斜体 | `src/platform/mac/text_system.rs` | 現行 macOS vendor に保持 |
| Wayland の空 commit | `src/platform/linux/wayland/client.rs` | 旧 helper と7件の test は現採用の `gpui-pre-linux 0.3.6` に存在せず、契約移行は未確認。旧 test を現経路の成功証拠にしない |

配布との差分照合、現所有者への対応、未確認事項の根拠は
[Issue #313 の監査記録](https://github.com/hide212131/hane/issues/313#issuecomment-6346215768)
と、そこから参照する archive 照合・Wayland 診断にある。
所有者が見つかったことだけで挙動同等性や旧 patch の解除を受け入れたとは扱わない。

現行 platform patch の範囲・検証入口・解除条件は
[macOS](../gpui-pre-macos/HANE-PATCH.md) と
[Windows](../gpui-pre-windows/HANE-PATCH.md) を参照する。
以下の Issue #101 / #126 の説明と `vendor/gpui` を指定するコマンドは
旧採用時の履歴であり、現行製品 backend の検証コマンドではない。
現在の CI は現行 platform vendor を指定して検証する。

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
- 同じ入力ソースの通知（コンテキスト更新やスクロールなどに伴う再通知を含む）では同期的な TSM 再活性化を繰り返さず、入力ソース ID または IME の ASCII 可否が実際に変わった通知だけを再同期する。
- key window / first responder / input context が存在しない場合は何もしない。フォーカスが無いときや通常のフォーカス遷移時の動作は変更しない。
- text input context の再入は抑止し、1回の入力ソース変更通知につき再同期を1回だけ行う。
- キーバインド解決用の `MacKeyboardMapper` 再構築ロジックは変更しない。

### 検証

macOS の `platform.rs` test は実際の keyboard-selection-change 通知を `APP_DELEGATE_CLASS` へ配送し、text responder の context に対する1回の `deactivate` → `activate` と、key windowなし・non-text responder・contextなしの安全な no-op を確認する。実際の日本語 IME の composing 状態と OS のフォーカスに依存するため、Hane 側の GUI 検証（`.agents/skills/hane-gui-test`）でも、Hane 起動後に日本語入力ソースへ切り替えてから通常段落・リスト項目へ日本語を入力し、変換結果が一度だけ期待 source offset へ commit されることを確認する。

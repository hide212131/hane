# Hane の限定パッチ

crates.io の pulldown-cmark 0.13.4 を同梱。MIT ライセンスは LICENSE に保持。

Issue #99: CommonMark 0.31.2 §4.2 に従い、`parse_atx_heading` の末尾空白除去と閉じ側 `#` の直前空白判定にタブを含める（`src/firstpass.rs` の2箇所のみ変更）。独自の見出し判定や入力の正規化は行わない。

元コード: https://github.com/pulldown-cmark/pulldown-cmark/tree/v0.13.4
仕様: https://spec.commonmark.org/0.31.2/#atx-headings

上流版で同等の修正が公開されたら、解析層の ATX 適合テストを通したうえで `[patch.crates-io]` とこのディレクトリを削除する。

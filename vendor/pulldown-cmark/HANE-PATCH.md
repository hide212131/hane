# Hane の限定パッチ

crates.io の pulldown-cmark 0.13.4 を同梱。MIT ライセンスは LICENSE に保持。

Issue #99: CommonMark 0.31.2 §4.2 に従い、`parse_atx_heading` の末尾空白除去と閉じ側 `#` の直前空白判定にタブを含める（`src/firstpass.rs` の2箇所のみ変更）。独自の見出し判定や入力の正規化は行わない。

元コード: https://github.com/pulldown-cmark/pulldown-cmark/tree/v0.13.4
仕様: https://spec.commonmark.org/0.31.2/#atx-headings

Issue #101: CommonMark 0.31.2 §2.1 / §6.1 に従い、`make_code_span` で CRLF を1つの改行として消費し、1つの空白に変換する。CR と LF をそれぞれ空白にすることで、両端の空白除去後にも余分な空白が残っていた。次行の quote/list prefix を読み飛ばす前に CRLF 全体を消費し、元の source range は変更しない（`src/parse.rs`）。

仕様: https://spec.commonmark.org/0.31.2/#code-spans 、https://spec.commonmark.org/0.31.2/#line-ending

回帰テスト: `tests/errors.rs` の `code_span_line_endings_normalize_once_and_preserve_source_offsets` が LF / CRLF / CR、本文中と両端の改行、全空白、quote/list の継続行、元のバイト範囲を検証する。Hane の `code_padding_tracks_semantic_spaces_instead_of_container_bytes` でも、除去対象となる CRLF の2バイト範囲を通常の workspace テストで確認する。

上流版で両方の修正が公開されたら、解析層の ATX / code span 適合テストを通したうえで `[patch.crates-io]` とこのディレクトリを削除する。

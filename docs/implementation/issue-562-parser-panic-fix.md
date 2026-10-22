# Issue #562: offset付き解析のパニック修正

## 対象と基準

- 対象Issue: [#562](https://github.com/hide212131/hane/issues/562)
- 対象フェーズ: `pulldown-cmark 0.13.4` の既存root causeに対する独立した製品修正
- 基準main: `7e757ef220556d38cb040e6afdb353425f342973`
- upstreamの元修正: `edee73b7b81b7888c942c6835b5b9a4ab9456f51`
- 0.13系列へのbackport: `078feb520e2d0898f71a7477792e26e2b3675651`（上流PR #1181）
- 再現・Release profile・到達経路・上流差分の記録: [Issue #562 comment](https://github.com/hide212131/hane/issues/562#issuecomment-6346459126)

この文書はClaude実装用Draft PRの準備記録です。製品コード・テストはまだ変更していません。

## 観測された問題

入力 `- [o]:u\n    \t`（UTF-8、13 bytes、hex `2d205b6f5d3a750a2020202009`）で、vendor内の `pulldown-cmark` offset iterator が `parse.rs:2203` の `unwrap(None)` でpanicします。macOS arm64 / Rust 1.98.1の通常Release library（opt-level=3、debug assertions無効、panic unwind）でも、Parser既定option、Haneと同じoption、`hane_markdown::parse_document`、`BlockIndex::from_buffer`、表示用 `local_block_index` の5入口で再現済みです。

元の上流修正と0.13 backportの製品source差分は、`firstpass.rs` の `scan_next_line_or_lazy_continuation` にある1 hunk（3行追加・2行削除）です。4個のSPACEを消費した後、残りがblank lineならlazy continuationとして返さない条件を加えます。現行vendor内の旧contextは1箇所だけに一致し、Hane独自の3 patchとは場所・条件が重ならないことを読み取り確認済みです。実際の適用・修正後検証は未実施です。

## 今回の変更範囲

1. 上流backport `078feb520e2d0898f71a7477792e26e2b3675651` から、このroot causeに必要な `firstpass.rs` の1 hunkだけをvendorへ最小backportする。
2. Haneの通常 `parse_document` 経路で上記13-byte入力がpanicせず解析できる回帰テストを追加する。source内容とbyte範囲が保持されること、および同じblank-line/lazy-continuation root causeを覆う隣接ケースも確認する。
3. vendorの既存Hane独自修正（ATX見出し末尾TAB、code span CRLF、SPACE型hard break）と対応テストを維持し、適用後に回帰を確認する。

## 対象外

- pulldown-cmark全体を0.13.5へ更新すること、vendor全体の置換、無関係な上流変更の取り込み
- `catch_unwind`、既定値への置換、panic全般の無視などで症状を隠すこと
- #548または#561への変更の同梱、AADWやworkflowなど運用基盤の変更
- 実アプリの終了・保存影響、データ損失、Windows Releaseの結果を未観測のまま断定すること
- merge、release、Issue close

## 受入・検証

- 通常 `parse_document` の13-byte回帰テストと同root causeの隣接ケースが成功し、sourceとbyte範囲の不変条件を確認できる。
- `cargo test --workspace --locked`、`cargo test --workspace --all-features --locked`、`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` を実行する。変更したvendorに必要な既存テストも実行する。
- 同じ通常Release条件で13-byte入力のpanicが解消することを確認する。
- macOS / Windowsのrequired CIと、current-headの最終reviewを確認する。
- 実アプリのfocused GUI確認は親Commanderが枠を調整する。必要なfixture、ビルド識別、再現手順、未観測範囲を親へ渡し、GUI未実施をpassに読み替えない。

この準備PRおよび準備文書だけでは、実装・検証・Issueの受入完了を意味しません。

# RF2-B / #302 P2 検証記録

状態: 製品コードとテストの検証は下記のコードSHAで成功。証拠文書の更新によりPR headが変わるため、更新後のcurrent-head CIとCodeRabbit reviewは未実施であり、P2の受入はまだ完了していない。

## 基準と変更

- P2採用設計: [refactor-rf2b-p2-implementation-spec.md](refactor-rf2b-p2-implementation-spec.md)
- PR: [#468](https://github.com/hide212131/hane/pull/468)。PR #467の設計採用merge commitは `b7f665e1a68c0c884b8faa239c07f1b9ec39f8d3`。
- P2開始base: `b7f665e1a68c0c884b8faa239c07f1b9ec39f8d3`。
- 検証した製品コードhead: `4683949dcd95da24958f5fdac118d430ac5e5ba2`。このあと本記録を更新し、新しいPR headが作られる。
- branch: `refactor/rf2b-p2-height-index-20261012`。
- test-only commit: `0da5e4b83e4d6c0e3fc57800665e9e393066ba59`。
- 機械的な製品移動commit: `94597ebe750eb7060975e4e548ef75bad60b7abd`。
- 変更したRust: `crates/presentation/src/lib.rs`、`crates/presentation/src/height_index.rs`、`crates/presentation/tests/height_index_contract.rs`。その後の整形修正はintegration test内のassertionの改行だけ。
- 全体変更ファイルは上記3つと本記録。UI、Markdown、Cargo、lockfile、workflow、layout、testingは変更していない。

## 移動前の境界確認

- 4件の外部contract testをtest-only commitで追加し、移動前の製品コードで実行した。
- trusted verification [run 38202006348](https://github.com/hide212131/hane/actions/runs/38202006348): `cargo test --workspace --all-features --locked` と `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` が成功。境界テストは移動前のコードで通過した。
- 追加したテスト: `empty_height_index_round_trips_through_splice`、`zero_height_entries_preserve_boundary_and_eof_selection`、`negative_height_inputs_keep_existing_clamping`、`scroll_anchor_keeps_identity_and_visual_height_clamping`。

## 機械的な移動

- `HeightIndex`、`ScrollAnchor`、`anchored_scroll_y`と関連するprivate型・method・5件のunit testを`height_index.rs`へ移した。crate rootにはprivate module宣言と既存3公開名の再exportを置いた。
- コード比較: 移動した製品本体は旧 `lib.rs` と新 `height_index.rs` でbyte-identical。既存5 testの名前・属性・入力・assertion・本体を維持し、指定どおりmoduleだけを移動した。計算式、条件分岐、公開API、内部公開範囲を変更していない。
- 4件の境界testはintegration testに残した。既存の高さ決定、表示、layout、UI、スクロール慣性、Markdown側のprojectionは対象外。
- 画面挙動を変更していないため、P2設計§9に従い追加の実画面確認は不要と判断した。

## コードhead `4683949dcd95da24958f5fdac118d430ac5e5ba2` の検証

- OS / toolchain: macOS ARM64、`rustc 1.98.1 (48a229cea 2026-09-01)`、`cargo 1.98.1 (797e8a9bc 2026-08-05)`、`rustfmt 1.9.0-stable (48a229ceae 2026-09-01)`。
- trusted verification [run 38205096374](https://github.com/hide212131/hane/actions/runs/38205096374): workspace all-features testsとClippyが成功。
- PR CI [run 38205167179](https://github.com/hide212131/hane/actions/runs/38205167179): release classifier成功。macOSとWindowsの `cargo test / clippy` が両方成功し、OS固有テストも実行された。Windows側IME notification testも成功。
- exact-head checkoutでの通常構成: `cargo test --locked -p hane-presentation --quiet`、`cargo test --workspace --locked --quiet`、いずれも終了コード0。
- docs: `cargo doc --locked -p hane-presentation --no-deps` と `cargo doc --locked -p hane-presentation --all-features --no-deps` は終了コード0。既存 `lib.rs` 内のbroken/private intra-doc linksに関する9 warningsが出た。
- formatting: `rustfmt --edition 2024 --check crates/presentation/src/lib.rs crates/presentation/src/height_index.rs crates/presentation/tests/height_index_contract.rs` は終了コード0。`git diff --check`も終了コード0。
- `cargo fmt --all -- --check`は終了コード1。差分は今回変更していないworkspace内の既存ファイルにあり、今回の3 Rustファイルはformatter出力に含まれなかった。新たなテストファイルだけでなく移動した両source fileも個別checkで成功。無関係なファイルは変更していない。

## 文書更新後の最終確認

- この文書更新によりPR headが変わる。更新後のexact head、current-head PR CI、CodeRabbit full reviewの結果はP2受入前にPR #468のconversationへ記録する。
- ドラフト中はCodeRabbit reviewが実行されていない。更新後のPR headに対するCIが成功した後でPRをreadyにし、CodeRabbitのcurrent-head full reviewと未解決threadを確認する。
- その後、base、head、mergeability、CI、reviewを再取得してP2を受け入れる。#302はP2ではcloseせず、既存設計どおりP3→P4→P5を続ける。
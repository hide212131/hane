# RF2-B P4：projection/mod.rs 分割の検証記録

## 変更前の基準

この記録はP4実装前の基準であり、P4の受入結果ではない。変更前mainは `b55ffaca14b9741c0bff44c66b55d2e6b4714d59`、作業ブランチは `refactor/rf2b-p4-projection-20261012`。P4の実装・テスト変更はこの基準取得後に行う。

| 項目 | 変更前の結果 |
| --- | --- |
| OS / architecture | macOS arm64 (`Darwin arm64`) |
| Rust | `rustc 1.98.1 (48a229cea 2026-09-01)` |
| workspace通常構成 | `cargo test --workspace --locked` — exact main SHAでexit 0。依存を含む通常構成の全workspace test/doc-testが完了。既知のfuture-incompatibility warningは依存 `block v0.1.6`。 |
| presentation test inventory | `cargo test --locked -p hane-presentation -- --list` — exit 0。unit 66件、integration 11件（`line_break_whitespace` 1、`markdown_features` 1、`source_map_contract` 9）、doc test 0件。 |
| all-features / Clippy / native CI | P4変更前の別記録として未取得。P4受入時は実装PRのcurrent headとbaseに対するrequired checksを確認する。 |

## P4の受入記録（実装後に追記）

- [ ] 受入候補のbase/head SHA、PR、merge commitを記録。
- [ ] `lib.rs`から`projection/mod.rs`への旧→新シンボル移動表を記録。P4は行・ブロックprojection一式をまとめて移し、P5のblock子module分割を先取りしない。
- [ ] crate rootのprivate module宣言・明示re-export、公開signatureとmodule privacyの比較を記録。
- [ ] 既存unit testの名前・属性・fixture・assertion・本体と件数を比較し、移動先を記録する。integration testの配置とroot importは維持する。
- [ ] exact-headの通常構成 / all-features / Clippy、macOS・Windows CI、current-head CodeRabbit reviewと未実施事項を記録する。
- [ ] GUI要否、rollback、およびP5への引継ぎを記録する。

設計の正本は `docs/refactor-rf2b-implementation-spec.md` の§5.5、§6、§8、§9。P4は機械的移動とし、処理順、条件分岐、アルゴリズム、契約を変更しない。PR本文にも受入候補headに結び付く証拠を記録する。

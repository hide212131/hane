# RF3-A P1 検証記録

Issue: [#304](https://github.com/hide212131/hane/issues/304)  
設計正本: [入力先判定の実装仕様](https://github.com/hide212131/hane/blob/cbf468b3d622f9c3f0f2823fdb5f03bc9cc58ee0/docs/refactor-rf3a-input-routing-implementation-spec.md)  
作業ブランチ: `refactor/rf3a-p1-input-routing-20261012`  
開始base: `c17dd0e62ad6f67d1aa6db9e23df5105e5b5884e`

## P1 の範囲

通常編集キー14 actionの入力先判定を `view/input_routing.rs` へ集約する。本文・sidebar filter・inline renameの既存処理を使い、P2の特別なキーとP3のplatform入力・IME callbackは含めない。リスト編集のplannerやsource-first transactionは変更しない。

## 変更前の契約確認

| 項目 | 結果 | 対象SHA・証拠 |
|---|---|---|
| 変更前の対象テスト一覧 | 未実施 | — |
| 追加する契約テストの変更前実行 | 未実施 | — |
| テストの削除・ignore・改名差分 | 未確認 | — |

## P1候補の検証

| 確認 | 結果 | 対象SHA・証拠 |
|---|---|---|
| UI crate 通常構成 tests | 未実施 | — |
| UI crate all-features tests | 未実施 | — |
| workspace locked tests | 未実施 | — |
| workspace all-features tests | 未実施 | — |
| workspace Clippy | 未実施 | — |
| rustfmt / diff check | 未実施 | — |
| macOS focused GUI・日本語IME | 未実施 | — |
| Windows focused GUI・日本語IME | 未実施 | — |
| 独立レビュー | 未実施 | — |

この記録を作成した時点で、製品コード・テスト・コンパイル・実画面確認・レビューは未実施である。未実施の項目を成功として扱わない。候補headと実際の証拠を取得後に追記する。

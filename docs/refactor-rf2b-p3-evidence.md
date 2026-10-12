# RF2-B P3：表示型・表示方針の分割 記録

## 現在の状態

2026-10-12時点で、作業ブランチ `refactor/rf2b-p3-display-20261012` と `main` の開始点はともに `ca9b1b61e0317101f5f994f7aa3259431056584d`。本ファイルはP3の追跡用ひな形であり、**製品コードの変更、コンパイル、テスト、レビュー、画面確認が成功した証拠ではない**。

対象設計は `docs/refactor-rf2b-implementation-spec.md` の第5.2節、第6節、第8節、第9節。P3だけを扱い、P4/P5を先取りしない。

## P3の記録項目

受入候補の正確なheadに対して、次を確認して追記する。

- [ ] 旧 `lib.rs` から新 `display.rs` へのシンボル移動表と、処理本体・型定義を変えていないことの比較。
- [ ] crate rootの公開API、内部可視性の限定変更、module privacyを確認した結果。
- [ ] unit testの旧名→新しい所属、属性・fixture・assertion・本体の保持、および件数の比較。
- [ ] 設計書第9.3節の必要な検証結果、toolchain、対象SHA、ログまたはrunへの参照。
- [ ] current-head macOS/Windows CI、trusted workspace検証、CodeRabbit full reviewの対象headと結果。
- [ ] GUI検証の要否判断、未実施事項、rollbackおよびP4への引継ぎ。

この追跡欄はコードやテストの受入結果を代替しない。受入判断はPRのcurrent diffとcurrent-head evidenceを照合して行う。

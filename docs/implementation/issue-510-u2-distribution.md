# Issue #510 U2: 更新対応配布基盤の準備

状態: 保守者による専用実装PR待ち。現在の差分は変更契約であり、release workflow、署名、notarization、配布物はまだ変更・発行していない。

- 対象Issue: [#510](https://github.com/hide212131/hane/issues/510)
- 先行PR: [#512](https://github.com/hide212131/hane/pull/512) の U0/U1 実装
- 準備時の先行head: `9fe6f26f4ec12c169e1b51b98ff4bd585aa1669f`
- 基準main: `efb38c2a0167bac8d0dfa512bacd0f85ba21817b`
- 正本: [ADR-0034](../../adr/0034-zip-application-updates.md)、[実装仕様](../../application-update-implementation-spec.md)、[実装引き継ぎ](../../application-update-implementation-handoff.md)

この作業は `.github/` と配布署名を扱うため、通常の製品workerへ保護path変更を委ねない。変更は保守者が別の配布基盤PRで実装・検証し、#512の製品PRとは独立にレビューする。基盤PRはU0/U1の受け入れ後にcurrent mainへ追随し、古い準備headを前提にしない。

## 目的

ZIP配布を維持したまま、更新clientが信頼できる版・hash・targetのカタログと、両OSの検証可能な配布物を発行する。既存のCargo version整合、release要求、Windows Explorer登録、portable配布を壊さず、同じtagの公開済みZIPを上書きしない。

## 変更範囲

予定path:

- `.github/workflows/windows-release.yml`
- `.github/workflows/release-request.yml`
- `scripts/bundle-macos.sh`
- 必要最小限のcatalog / inventory生成・検証scriptと、そのfixture test
- 配布方法・bootstrap手順を説明するREADME箇所

通常製品workerの保護path制約、AADW workflow/Commander policy、一般的なrelease自動化の再設計は対象外。

## 実装契約

1. Windows x64/macOS arm64で、アプリと同じprotocol/version/source SHAを持つ `hane-updater` をbuildし、実binary metadataを検査する。
2. Windows ZIPはU1のversion別Explorer DLL layoutを保持する。署名済みinventoryはZIP内容・版・source SHAに結び付け、ZIP検査後に生成する。
3. macOS bundleへCargo版、source SHA、updater protocolを記録する。Developer ID署名、Hardened Runtime、notarization/staplingを行った後にZIP化し、署名後にbundleを変更しない。
4. 実ZIPのsize/hashとinventoryまたはbundle identityからcatalogを生成する。test鍵でないEd25519鍵でcatalogを署名し、両targetが揃わない場合は公開しない。
5. 両OS ZIP、catalog、署名をDraft Releaseへuploadし、downloadし直して署名・版・target・hashを検証した後にだけstable公開する。公開済み同tagへ `--clobber` しない。修正版は新しいCargo版を使う。
6. release-requestの完了確認はZIPの有無だけで成功にせず、catalog/signature、版、schema/protocol、両target、asset hashの整合まで検査する。
7. 公開workflowとは別に、秘密鍵を使わないfixtureと明示的なdry runで生成・検証・公開前ゲートを確認する。実tag作成やstable公開を検証の代わりにしない。

## 保守者設定と未確定値

本番鍵・Apple identityの値を設計placeholderから推測・生成・commitしない。保守者が管理する値を別途設定してから本番workflowを有効にする。

- catalog署名の秘密鍵とkey ID。本番公開鍵は `hane-update` の `TrustPolicy::production()` に対応する製品変更として埋め込む。
- macOS Developer ID Team ID、証明書、notary credentials。
- Windows Explorer package証明書と、既存release automation tokenの利用可能性。
- repository/environment secretsの名前、保護環境、rotationとkey overlap方針。

未設定時はreleaseを失敗させ、test鍵・空署名・ad-hoc署名で代用しない。秘密値をログ、artifact、PRへ出さない。

## 受入証拠

- dry runの対象head、両target、helper protocol/version/source SHA、catalog/signature schemaとhash検証結果。
- ZIPごとのarchive SHA-256/sizeとWindows inventoryまたはmacOS bundle identityの一致。
- 未設定credential、欠損target、未知schema/protocol、hash不一致、署名不正、既存tagへの上書き要求で公開が拒否された結果。
- maintainer管理の署名鍵・Team ID・notary credentialsを使った署名検証。実署名済み配布物のOS受入と二版更新はU5/U14の別証拠とする。

対応する受入条件は主にU-07とU-13。workflow unit/dry-run passだけでU-13の実macOS Gatekeeper確認やU-14の更新完了を主張しない。秘密情報設定、実tag公開、ユーザー向けbootstrapはこの準備文書の変更に含めない。

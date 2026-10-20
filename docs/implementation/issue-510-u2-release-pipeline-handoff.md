# Issue #510 U2: 配布・署名パイプライン実装ハンドオフ

状態: U2の配布基盤の実装をこのPRの差分として含める。実装head `eb988f8405dd8fc542b6b5aa9394a8707a0aa67d` について、Windows/macOS CI run 39278984279とexact-result verifier run 39277532943が成功し、Codex Review #6271746760が同じheadで重大な問題なしと報告している。これらが証明するのはPRコードの自動検査とreviewまでである。本番署名・notarization・実release発行・protected Environmentの承認は行っておらず、保守者の資格情報設定後の実run、U-13/U-14の実OS更新確認、U5は未実施・未公開である。fixture/CIの結果を実OSの更新成功や署名の証明として扱わない。

実装の所在（いずれもこのPRの差分内の変更で、上記のCI・verifier・Codex Reviewによるcurrent headの自動検査とreviewは完了。本番署名・実release・実OS更新は未確認）:

- `scripts/release_catalog.py` — inventory/catalog生成、Ed25519署名（`cryptography==50.0.2`、`scripts/release-requirements.txt`）、全入口共通のverifier、公開済みcatalogからのsequence採番、tag/latest確認、Draft/公開assetの再取得。
- `.github/workflows/windows-release.yml` — 署名設定のfail-closed確認、両OSのhelperビルドと実binaryのidentity検証、Windows inventory署名とAuthenticode `Valid` 確認、macOS Developer ID署名/notarize/staple/再展開検証、直列化された`release`ジョブ（plan → catalog生成/署名 → Draft → 再取得検証 → stable公開 → 公開後再検証）。
- `.github/workflows/release-request.yml` — 公開後に両ZIPとcatalog/署名を同じverifierで検証。
- `scripts/bundle-macos.sh` — `hane`と`hane-updater`の双方をビルドし、`Contents/Helpers/hane-updater`とInfo.plistの版/source SHA/protocolを設定。
- `crates/updater/{build.rs,src/main.rs}` — version/target/source SHA/protocolを、起動時の整合確認で実際に参照される識別recordとして埋め込み。`black_box`や`#[used]`の採用だけでは完了とせず、`release_catalog.py binary-identity`が実際にbuildしたhelper（Windowsは`hane-updater.exe`、macOSは署名前のbundle内helper）を読み取り専用で検査し、`verify-windows-zip`/`verify-macos-zip`/`verify-release`がZIP内のhelperを同じ検査にかける。helper自体は実行せず、通常の安全境界は変えない。実行できないOS/runnerのhelperは検証済みと扱わない。
- `crates/update/src/trust.rs` — 公開鍵とTeam IDをビルド時環境変数から埋め込む（未設定・不正値は何も埋め込まずfail-closed）。
- `scripts/tests/test_release_catalog.py`, `scripts/tests/test_release_workflows.py` — fixture testとworkflow配線test。CIの`rust_ci_changes`から実行する。

- 対象Issue: [#510](https://github.com/hide212131/hane/issues/510)
- 対象フェーズ: U2（ZIP配布物とstable release pipeline）
- 先行実装PR: [#518](https://github.com/hide212131/hane/pull/518)
- U2実装開始時の基準head（履歴情報であり、current mainやcurrent PR baseではない）: `00bf87fd529398d9ee7305f74fdea899065f8260`
- 設計の正本: [ADR-0034](../adr/0034-zip-application-updates.md)、[実装仕様](../application-update-implementation-spec.md)、[実装引き継ぎ](../application-update-implementation-handoff.md)

実装依頼後は対象PR headを再確認し、基準headと異なる場合はpushしない。保存済みの案はPRへの適用・承認とは別の判断であり、この文書は適用の事実を保証しない。

## 今回の目的

既存のWindows x64/macOS arm64 ZIP配布を維持し、#518の更新helperが読む、署名済みcatalogとOSごとの更新対応ZIPを再現可能に生成・検証してからstable公開する。Windowsのバージョン別Explorer DLLを保持する。production鍵やApple/Windows証明書が未設定なら、通常のstable公開を失敗させる。

## 許可する実装範囲

- `.github/workflows/windows-release.yml`、`.github/workflows/release-request.yml`、`scripts/bundle-macos.sh`。
- catalog・Windows inventoryの生成/検証に必要な専用scriptと、そのfixture test。
- `crates/update/src/trust.rs` など、公開鍵とTeam IDを本体へ埋め込み、未設定時にfail-closedを保つための最小のU2接続変更。
- `crates/updater/build.rs` 等、配布helperのversion/source SHA/protocol識別情報をrelease時に検査する最小の変更。
- `.github/RELEASE_AUTOMATION.md`、`docs/windows-release-signing.md`、README内のZIP更新配布説明。

## 実装契約

1. WindowsとmacOSのrelease binaryに、Cargo version・対象target・source SHA・updater protocolが整合することを検査可能な形で埋め込む。Windows ZIPには`hane-updater.exe`とversion別の`shell-integration/<version>/hane_shell_extension.dll`を含める。macOS ZIPはDeveloper ID署名、Hardened Runtime、notarization、stapling後の`.app`を格納し、署名後にbundleを変更しない。
2. ZIPとcatalogが、[実装仕様](../application-update-implementation-spec.md)のlayout revision・size上限・schema・署名domainと一致することを検証する。Windows inventoryはZIP内容、version、source SHAに結び付ける。macOS bundle ID/version/Team IDをcatalogと照合する。
3. 実ファイルのsize/hashから両targetのcatalogを生成し、Hane更新専用Ed25519鍵で署名する。片方のZIP・署名・設定が欠ける場合はreleaseしない。
4. ReleaseをまずDraftとして作成し、アップロード後にassetをdownloadし直す。署名、版、source SHA、target、ZIP hash、inventory/bundle metadataを検証し、成功時だけstableへ公開する。既存tagや公開済み同tagのassetを上書きしない。
5. release-request完了確認は、両ZIPに加えてcatalog/signature、版、protocol、hashの整合を検証する。通常CIとfixture testは本番秘密鍵なしで実行できる。
6. Windowsの既存lockがあるケースは更新拒否とファイル非変更を検証する。エラーcodeの固定は条件にしない。

## 資格情報と安全境界

production catalog鍵、公開鍵、macOS Team ID/notary credentials、Windows package証明書は保守者管理値である。値を推測・生成・commitせず、未設定ならfail-closedにする。fixture専用鍵はtest中にtemp領域で生成し、本番artifactやsecretsへ流用しない。実tagの作成・stable release公開・GitHub secrets/variablesへの値設定は今回の実装依頼に含めない。

## 対象外

- #518のU4 helper・journal・rollback・UI/終了freezeの再設計。
- #500 Finder入力、#507 file dropそれぞれの個別変更。共通入口freezeは#518の既存契約を使う。
- #510 U5 bootstrap発行、実Windows/macOSでの署名済み二版更新、Explorer登録状態保持、Gatekeeperの実機証拠。
- 更新対象OS・CPUの追加、自動更新、prerelease、MSIX全体配布への移行。

## U2の受入証拠

- production secretなしのfixtureで、署名・catalog・ZIP hash・inventory/bundle検査が成功し、欠損target・不正署名・hash改変・既存tag上書きは拒否される。
- release jobがDraft作成後に再取得したassetを検査し、検査成功時だけ公開する構成をCIで確認する。
- Windows/macOS両方のZIPに、helperと更新helperが必要とする配置・識別情報が含まれることを確認する。
- 実署名資格情報が未設定の間は、本番stable公開が失敗し、署名済みreleaseが存在しない。

fixture/CI結果を実OSの更新成功やU-13/U-14の証明として扱わない。Issue #510はU5を含む全受入条件が満たされるまでopenのままにする。

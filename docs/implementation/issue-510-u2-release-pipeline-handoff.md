# Issue #510 U2: 配布・署名パイプライン実装ハンドオフ

状態: U2の配布基盤の実装をこのPRの差分として含める。このPR #526の対象はU2の範囲に限り、Issue #510全体の完了ではない。

検査・review結果の扱い（head別に区別する。current headの証拠は都度GitHubで確認し、過去headの結果を流用しない）:

- 過去head `eb988f8405dd8fc542b6b5aa9394a8707a0aa67d` に対する過去結果: Windows/macOS CI run 39278984279、exact-result verifier run 39277532943、Codex Review #6271746760（重大な問題なしと報告）。これらは当時のheadの結果であり、current headの結果ではない。
- head `e0b1bfc1def27fb21233db97b93d41cd87b6b124` に対する結果: CI run 39442507034はWindows/macOSとも成功。CodeRabbitの最終レビューは同headで完了し、この文書について2件を指摘した（本修正の対象）。
- この文書を修正するcommitで新しいheadになった場合、そのheadのCI・review・GUI等の証拠は別途取得する。上記の結果は新headの証拠にならない。

CI・verifier・reviewが証明するのはPRコードの自動検査とreviewまでである。本番署名・実release発行・protected Environmentの承認、実Releaseの公開、実OSでの更新は未実施である。fixture/CIの結果を実OSの更新成功や署名の証明として扱わない。

実装の所在（いずれもこのPRの差分内の変更。本番署名・実release・実OS更新は未実施・未確認）:

- `scripts/release_catalog.py` — inventory/catalog生成、Ed25519署名（`cryptography==50.0.2`、`scripts/release-requirements.txt`）、全入口共通のverifier、公開済みcatalogからのsequence採番、tag/latest確認、Draft/公開assetの再取得。
- `.github/workflows/windows-release.yml` — 署名設定のfail-closed確認、両OSのhelperビルドと実binaryのidentity検証、Windows inventory署名とAuthenticode `Valid` 確認、macOS bundle内inventoryのEd25519署名と再展開検証（Apple Developer ID・notarizationは使わない）、直列化された`release`ジョブ（plan → catalog生成/署名 → Draft → 再取得検証 → stable公開 → 公開後再検証）。
- `.github/workflows/release-request.yml` — 公開後に両ZIPとcatalog/署名を同じverifierで検証。
- `scripts/bundle-macos.sh` — `hane`と`hane-updater`の双方をビルドし、`Contents/Helpers/hane-updater`とInfo.plistの版/source SHA/protocolを設定。
- `crates/updater/{build.rs,src/main.rs}` — version/target/source SHA/protocolを、起動時の整合確認で実際に参照される識別recordとして埋め込み。`black_box`や`#[used]`の採用だけでは完了とせず、`release_catalog.py binary-identity`が実際にbuildしたhelper（Windowsは`hane-updater.exe`、macOSは署名前のbundle内helper）を読み取り専用で検査し、`verify-windows-zip`/`verify-macos-zip`/`verify-release`がZIP内のhelperを同じ検査にかける。helper自体は実行せず、通常の安全境界は変えない。実行できないOS/runnerのhelperは検証済みと扱わない。
- `crates/update/src/trust.rs` — 更新用公開鍵をビルド時環境変数から埋め込む（Team IDは使わない）（未設定・不正値は何も埋め込まずfail-closed）。
- `scripts/tests/test_release_catalog.py`, `scripts/tests/test_release_workflows.py` — fixture testとworkflow配線test。CIの`rust_ci_changes`から実行する。

- 対象Issue: [#510](https://github.com/hide212131/hane/issues/510)
- 対象フェーズ: U2（ZIP配布物とstable release pipeline）
- 先行実装PR: [#518](https://github.com/hide212131/hane/pull/518)
- U2実装開始時の基準head（履歴情報であり、current mainやcurrent PR baseではない）: `00bf87fd529398d9ee7305f74fdea899065f8260`
- 設計の正本: [ADR-0034](../adr/0034-zip-application-updates.md)、[実装仕様](../application-update-implementation-spec.md)、[実装引き継ぎ](../application-update-implementation-handoff.md)

実装依頼後は対象PR headを再確認し、基準headと異なる場合はpushしない。保存済みの案はPRへの適用・承認とは別の判断であり、この文書は適用の事実を保証しない。

## 今回の目的

既存のWindows x64/macOS arm64 ZIP配布を維持し、#518の更新helperが読む、署名済みcatalogとOSごとの更新対応ZIPを再現可能に生成・検証してからstable公開する。Windowsのバージョン別Explorer DLLを保持する。production鍵やWindows証明書が未設定なら、通常のstable公開を失敗させる。

## 許可する実装範囲

- `.github/workflows/windows-release.yml`、`.github/workflows/release-request.yml`、`scripts/bundle-macos.sh`。
- catalog・Windows inventoryの生成/検証に必要な専用scriptと、そのfixture test。
- `crates/update/src/trust.rs` など、公開鍵を本体へ埋め込み、未設定時にfail-closedを保つための最小のU2接続変更。
- `crates/updater/build.rs` 等、配布helperのversion/source SHA/protocol識別情報をrelease時に検査する最小の変更。
- `.github/RELEASE_AUTOMATION.md`、`docs/windows-release-signing.md`、README内のZIP更新配布説明。

## 実装契約

1. WindowsとmacOSのrelease binaryに、Cargo version・対象target・source SHA・updater protocolが整合することを検査可能な形で埋め込む。Windows ZIPには`hane-updater.exe`とversion別の`shell-integration/<version>/hane_shell_extension.dll`を含める。macOS ZIPは`Contents/Resources/hane-install.json`と`.sig`（更新用Ed25519鍵で署名したinventory）を含む`.app`を格納し、署名後にbundleを変更しない。Apple Developer ID・Team ID・notarization・stapleは要求しない（`_CodeSignature/CodeResources`があれば検査対象になるが必須ではない）。
2. ZIPとcatalogが、[実装仕様](../application-update-implementation-spec.md)のlayout revision・size上限・schema・署名domainと一致することを検証する。Windows inventoryはZIP内容、version、source SHAに結び付ける。macOSはbundle内の署名済みinventoryをversion・target・source SHAとZIPの実展開内容に結び付け、bundle ID/version/inventory hashをcatalogと照合する。
3. 実ファイルのsize/hashから両targetのcatalogを生成し、Hane更新専用Ed25519鍵で署名する。片方のZIP・署名・設定が欠ける場合はreleaseしない。
4. ReleaseをまずDraftとして作成し、アップロード後にassetをdownloadし直す。署名、版、source SHA、target、ZIP hash、inventory/bundle metadataを検証し、成功時だけstableへ公開する。既存tagや公開済み同tagのassetを上書きしない。
5. release-request完了確認は、両ZIPに加えてcatalog/signature、版、protocol、hashの整合を検証する。通常CIとfixture testは本番秘密鍵なしで実行できる。
6. Windowsの既存lockがあるケースは更新拒否とファイル非変更を検証する。エラーcodeの固定は条件にしない。

## 資格情報と安全境界

production catalog鍵、公開鍵、Windows package証明書は保守者管理値である（Apple Team ID・notary credentialsは使わない）。値を推測・生成・commitせず、未設定ならfail-closedにする。fixture専用鍵はtest中にtemp領域で生成し、本番artifactやsecretsへ流用しない。実tagの作成・stable release公開・GitHub secrets/variablesへの値設定は今回の実装依頼に含めない。

## 対象外

- #518のU4 helper・journal・rollback・UI/終了freezeの再設計。
- #500 Finder入力、#507 file dropそれぞれの個別変更。共通入口freezeは#518の既存契約を使う。
- Issue #510の実機・実OS確認（下記「Issue #510の現行完了条件」）。実Windows/macOSでの署名済み二版更新、Explorer登録状態保持などの実地証拠は、このPRのU2範囲では取得しない。
- 更新対象OS・CPUの追加、自動更新、prerelease、MSIX全体配布への移行。

## U2の受入証拠

- production secretなしのfixtureで、署名・catalog・ZIP hash・inventory/bundle検査が成功し、欠損target・不正署名・hash改変・既存tag上書きは拒否される。
- release jobがDraft作成後に再取得したassetを検査し、検査成功時だけ公開する構成をCIで確認する。
- Windows/macOS両方のZIPに、helperと更新helperが必要とする配置・識別情報が含まれることを確認する。
- 実署名資格情報が未設定の間は、本番stable公開が失敗し、署名済みreleaseが存在しない。

fixture/CI結果を実OSの更新成功の証明として扱わない。

## Issue #510の現行完了条件

Issue #510本文の現行の完了条件は次の4点である。旧U-01〜U-15の全項目やU5を一律に実機確認する条件ではない。

1. 作者本人のMacとWindowsで、旧版から新版へ更新し、通常の編集ができることを実確認する。
2. 不正署名・破損ZIP・対象OS違いを、適用前に拒否することを確認する（既存の自動テストを利用してよい）。
3. 未保存作業を保留し、文書・設定・AI接続・Windowsの右クリック機能を保つことを確認する。
4. 代表的な失敗の後に、旧版とデータを保ち、自動復旧または短い手順で再利用でき、失敗を成功と表示しないことを確認する。

このPR #526のU2範囲は上記のうち配布物・署名・catalogの生成と検証の基盤に関わる部分であり、実Releaseの公開と実OSでの更新確認（1〜4の実地部分）は未実施である。Issue #510は、これらが満たされるまでopenのままにする。

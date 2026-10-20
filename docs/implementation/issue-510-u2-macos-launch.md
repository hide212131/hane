# Issue #510 U2: macOS ZIPの起動時署名検証

状態: PR #535 はDraftで未完了。署名除去方式の実装後のCI検証、実Macでの起動許可、Issue #510 の実Mac/Windows更新確認は未完了。

方針変更: ad-hoc署名の再付与とcustom resource rules方式は、実Macの配布ZIPでmacOSのcodesignが「resource envelope is obsolete (custom omit rules)」として拒否したため撤回した。現在は、`scripts/bundle-macos.sh` が `Contents/MacOS/hane` と `Contents/Helpers/hane-updater` のリンカー由来ad-hoc署名だけを `codesign --remove-signature` で取り除く。実ファイルの一時コピーでは両方が「code object is not signed at all」になり、helperのversion/target/source SHA identity checkは成功し、quarantine付きコピーのspctl assessmentは「rejected / source=no usable signature」だった。これはGatekeeper回避ではなく、Issue #510が許容する本人によるアプリ単位の初回許可を維持する。`codesign --verify` の成功は受入条件にしない。現時点で実際に起動できるとは扱わず、最終ZIPでの検査と本人による起動確認は別に記録する。

## 対象と基準

- 対象Issue: #510。完成条件はIssue本文を変更せず、そのまま適用する。
- 対象: macOS Apple Silicon ZIP内の `Hane.app` と、通常のZIP展開後にmacOSが行うコード署名検証。
- 基準main: `97c279acc063ef51ad5d5ee12af51a0edee174d2`。
- 関連済み実装: #518（U4の適用・復旧）、#526（U2の配布・更新署名）。どちらも現在mainへ取り込み済み。
- 関連する準備PR #517 は古いU0 branchをbaseとした文書のみのDraftであり、この修正へ転用しない。

## 観測した問題

GitHub Release `v0.27.0` の `hane-macos-arm64.zip` を取得し、ZIPのCRC検査に成功した。ZIP内の `Hane.app` では、macOSの `codesign --verify --deep --strict` が次のエラーを返した。

```text
code has no resources but signature indicates they must be present
```

これは利用者が報告した「壊れているため開けません」というmacOS表示と整合する。したがって、単なる初回起動許可の不足として扱わず、配布bundleの署名状態を修正する。

配布ZIPから作った一時コピーでad-hoc bundle signingを行うと、同じコード署名検査は成功した。一方、Gatekeeperのassessmentは依然としてrejectedだった。これはDeveloper ID・Apple公証を含まない場合にユーザー操作による個別の起動許可がなお必要になり得ることを示す。これは製品要求を変更する根拠ではない。

検査対象の公開ZIP SHA-256:
`3c5f075c5e36ebefe5abfe2a99faa30679fda4c8be03ba480f4c07f768fba8f5`

## 今回直す範囲

macOS release workflowで、最終的に公開するアプリbundleがmacOSの署名整合性検査を通るようにする。通常のZIP生成、GitHub Release、Ed25519更新catalog・bundle inventoryは再利用する。

実装では次の両方を同時に保つこと。

1. bundle内のHane本体とupdater helperに署名が残っておらず、`_CodeSignature` もない。bundle作成時にhelper identityも再検証する。
2. 最終ZIPの全内容が既存のEd25519 inventory / catalog検証に一致し、version・target・source commitも一致する。署名除去後にファイルを追加・変更した状態を成功扱いにしない。

既存のworkflow順（bundle → inventory → ZIP）を使い、inventoryは署名除去後の最終bundleの全ファイルから生成する。既存の安全な検証を省いたり弱めたりしない。

リリースCIでは、既存の `verify-macos-zip` が実際にZIPへ格納した後の再展開bundleを検査し、2つの実行ファイルに `LC_CODE_SIGNATURE` が残っていないことと、Ed25519 inventoryの全件一致を確認する。`.github/**` は変更しない。

利用者向けREADMEには、未確認開発元に対する本人の一度限りの許可だけを案内する。「壊れている」警告を突破させる案内にはしない。

## 維持する条件・対象外

- Issue #510の方針どおり、Apple Developer ID、有料Developer Program、Team ID、公証、stapleを必須化しない。
- AppleのOS全体の保護を弱めない。quarantineを削除したり、Gatekeeperを迂回する処理は追加しない。
- Ed25519による更新catalog・inventory検証、ZIP中身・OS・版の検査、利用者データ保護、Windows配布を変えない。
- #518で決めた更新時の「ad-hoc再署名しない」方針を変えない。この作業はリリース時に作るMac配布bundleだけを扱う。
- v0.27.0の既存Release assetは上書きしない。修正は次の新しいpatch releaseとして公開する。
- このPRと実装CIの成功だけでIssue #510全体を完了としない。MacとWindowsの実更新・通常編集・データ保持・代表的な失敗復旧は別途実機確認する。

## PR単位の完了条件

- macOS release pathが、同梱実行ファイルに署名が残る場合を検出して公開を止める。
- 最終ZIPを検査し、署名が残っていないこととEd25519 inventory/catalogの両方が成功する。
- READMEに初回起動の必要な操作が短く説明されている。
- #510全体の未実施実機確認は未完了として記録される。

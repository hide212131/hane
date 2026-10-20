# Issue #510 U2: macOS ZIPの起動時署名検証

状態: PR #535 はDraft・未merge。実装はこの文書の「現在の設計」に変更済みだが、この変更に対するCI・実Macでの署名検証・Finder起動・更新はまだ確認していない。CIの成功はIssue #510の完了ではない。実Finderでの起動、およびMac/Windowsの実更新と復旧は未確認。

## 設計の変遷

1. 初期案: ad-hoc署名の再付与とcustom resource rules方式。実Macの配布ZIPでmacOSのcodesignが「resource envelope is obsolete (custom omit rules)」として拒否したため撤回した。
2. 前の案（このPRの以前のhead）: `codesign --remove-signature` で2つの実行ファイルの署名を除き、bundleを未署名にしてinventoryをアプリ内に置く。Apple Siliconは有効なコード署名のない実行コードを起動しないため、これは誤りだった。署名を外したarm64実行ファイルは実Macで起動せず、ad-hoc署名を付け直すと起動することを確認した。
3. **現在の設計**: 完成した`Hane.app`を内側からad-hoc署名で封印し、Hane自身のEd25519 inventoryと署名はアプリの**外**に置く。署名（Apple）とinventory（Ed25519）が互いのファイルを変更しないため、両方が成立する。

## 現在の設計

- `scripts/bundle-macos.sh` はInfo.plistなどをすべて置いた後、helper → `Contents/MacOS/hane` → bundle の順に `codesign --force --sign - --timestamp=none` で封印し、`codesign --verify --deep --strict` が成功することを確認する。`Contents/_CodeSignature/CodeResources` ができる。Developer ID・Team ID・notarization・stapleは追加しない。quarantineの削除やGatekeeperの迂回もしない。
- inventory（`hane-install.json`/`.sig`）は封印後の`Hane.app`の全ファイル（`CodeResources`を含む）から生成し、ZIPの `hane-macos-arm64/Hane.app.update/<version>-<source_sha>/` に置く。アプリ内（`Contents/Resources`）には置かない。ZIP layout revisionは2。
- release verifier（`scripts/release_catalog.py`）は新layoutの許可済みsidecarだけを受け入れ、未知file・余分な実行物・パス逸脱・別版/複数世代・欠落・改変・`CodeResources`なし・Mach-Oの署名なし・アプリ内の旧位置のinventoryを拒否する。ZIP展開後は `verify-macos-codesign` がAppleの `codesign --verify --deep --strict` を実行し、ad-hocかつTeam IDなしを確認する。
- 更新側（`crates/update`）は、インストール済みbundleのInfo.plistの版/source SHAが指すsidecar世代を、アプリごとの外部データ `.hane-update-<InstallId>/inventories/` と、bundleの隣の `Hane.app.update/` の順に探す。見つかった世代が不正なら拒否し、旧データへfallbackしない。
- 公開済みv0.27.0（inventoryがアプリ内、layout 1）は、どこにも世代がない場合に限り、旧inventoryとして読む。新版の更新が新世代を `inventories/` に作ることで、新layoutへ1回移行する。旧世代・旧inventoryはhealth commitまで、そのあとも削除しない。
- 適用では、新sidecarを交換前に `inventories/` へ書く（既存世代は上書きせず、完全一致のときだけ再利用、不一致なら停止）。bundleの原子的交換、限定health、commit前の再検証、rollbackは変えていない。

## 維持する条件・対象外

- Issue #510の方針どおり、Apple Developer ID、有料Developer Program、Team ID、公証、stapleを必須化しない。
- AppleのOS全体の保護を弱めない。quarantineを削除したり、Gatekeeperを迂回する処理は追加しない。
- Ed25519による更新catalog・inventory検証、ZIP中身・OS・版の検査、利用者データ保護、Windows配布を変えない。
- 更新時にad-hoc再署名しない。ad-hoc署名はリリースbuild時に1回だけ付ける。
- 現在のCargo version（0.27.1）を維持し、公開v0.27.0のassetは変更しない。
- このPRと実装CIの成功だけでIssue #510全体を完了としない。

## release workflowの接続変更（未反映）

`.github/**` は保護対象のためこのPRの作業者は変更していない。新layoutにするには `.github/workflows/windows-release.yml` の `build-macos` job で次を変える必要がある。これが反映されるまで、リリースのmacOS部分は新layoutで公開できない（`verify-macos-zip` は旧layoutのZIPを拒否する）。

1. 「Generate and sign macOS inventory」: 署名したinventoryを `$app/Contents/Resources/` ではなく、ZIPのstagingの `hane-macos-arm64/Hane.app.update/<version>-<source_sha>/hane-install.json(.sig)` へ置く（`python scripts/release_catalog.py macos-sidecar-dir --version … --source-sha …` が相対パスを出力する）。`macos-inventory` は封印済みのbundleから実行する。
2. 「Package macOS app zip」: `Hane.app` と並べてその `Hane.app.update/…` を同じstagingに入れ、`ditto` でZIP化する。
3. 「Verify extracted macOS zip」: 展開した `Hane.app` に `python scripts/release_catalog.py verify-macos-codesign --app "$app"` を実行し、`macos-inventory` を再生成して展開済みsidecarと `cmp` する。
4. `scripts/tests/test_release_workflows.py` のうち旧layoutを前提にするworkflow検査（`Contents/Resources/hane-install.json.sig`、workflowに `codesign` を含めない検査）は、上記の変更と同時に更新する。

## 実Macでの確認（未実施）

次はすべて未確認で、完了として扱わない。

- 新しいrelease ZIPをFinderで展開し、`codesign --verify --deep --strict` が成功し、Apple Siliconで起動できること。
- 展開後の `Hane.app` と `Hane.app.update` がEd25519検査に成功すること。
- v0.27.0 → 新版の実更新、health失敗時のrollback、保存データの保持。
- Windowsを含む#510全体の実機確認。

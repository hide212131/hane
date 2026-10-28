# Issue #510 更新受入向け隔離検証フィード

状態: 実装担当へ渡す範囲を準備中。製品コードの変更・実機受入は未実施。

関連Issue: [#510](https://github.com/hide212131/hane/issues/510)
関連実装PR: [#656](https://github.com/hide212131/hane/pull/656)
調査基準main: `ad4b80b155bf219a9c5686858dba3c87f14c3e77`
調査時の#656 head: `589918e73db9feaa3b66b88a01c81f68a667d4d8`

## 目的

通常インストール、通常設定、公開Release、実際の署名秘密鍵に触れず、Issue #510 の更新画面・ダウンロード・検証・適用・再起動を、署名済みの使い捨て更新パッケージでMacとWindowsから確認できる検証経路を用意する。

現在のアプリはGitHub Releasesの取得先を固定し、HTTPSと限定したredirect先を検査する。`ScriptedTransport` はRustテスト用で、通常起動するアプリの更新画面からローカル配布物を取得する入口ではない。#656のWindows helper名修正を含む新候補も、公開済み更新成果物には含まれていない。そのため、公開Releaseを先行作成せずにcurrent-head候補を実機受入する経路が必要である。

## 採用する方針

通常の配布機能へruntime overrideを追加せず、専用の明示的なvalidation buildだけが、ループバック上の検証サーバーから更新情報と署名済みパッケージを取得できるようにする。標準Releaseでは検証用feed、検証用公開鍵、HTTP例外を一切有効にしない。

validation buildでも、署名検証・対象OS/architecture確認・archive検査・未保存文書保護・helper適用・health確認・commit/rollbackは通常の更新処理を通す。差し替えるのは配布元と使い捨て公開鍵だけとし、通常版のGitHub endpoint・production trust policyは不変にする。

## 実装範囲

1. Cargoのopt-in validation build/profileを設け、標準の`debug`/`release` buildへ自動では含めない。通常Releaseの配布経路から検証機能を誤って有効化できない境界をコード側で明示する。
2. validation build専用のfeed設定を追加する。設定可能なのは`127.0.0.1`または`[::1]`のephemeral portだけとし、plain HTTPはこのloopback先に限る。userinfo、任意host、外部redirect、proxy経由のloopback迂回は拒否する。通常buildでは環境変数やstateファイルを読んでendpointを変更する経路を持たない。
3. validation buildだけが使える、実行ごとに生成する使い捨て公開鍵を用意する。対応する秘密鍵は検証fixture作成側だけに保持し、ソース、PR、通常ユーザーのcredential storeへ保存しない。
4. ephemeral portの小さなローカルfixture serverから、GitHub Release形式のmetadata、署名済みcatalog、signature、対象OS用ZIPを返す。HTTP client、URL検査、signature/archive検証、staging、OS適用、再起動は実アプリの経路を使い、`ScriptedTransport`だけでend-to-end確認を代替しない。
5. 旧版とcurrent candidateを同じ隔離validation profileで作成し、旧版から新しいSemVerの候補へ更新できるfixtureを作る。current source SHA、両binary hash、package hash、target、検証鍵ID、隔離install/state pathを記録する。versionの順序を偽装するために製品の通常version値や公開tagを書き換えない。
6. 専用bundle/app identity、install path、working directory、`HANE_STATE_DIR`を使い、通常版Haneとuser dataから分離する。AI設定保持確認に使う場合も、使い捨てcredentialだけを検証用keychain/storeへ置く。
7. 検証用candidateの終了・再起動後に、候補binaryへの切替とfixture/stateの後片付けを確認できる記録を出す。通常版や検証serverの所有外にあるプロセスを停止しない。

## 受入条件

- 標準のdebug/release buildでは、既存の固定GitHub endpoint、HTTPS要件、redirect許可host、production trust policyが変わらず、validation設定を渡しても切り替わらない。
- opt-in validation buildはloopback外のURL、HTTP external host、userinfo、外部redirectを拒否し、通常ユーザーのproxy・DNS・certificate・公開Release設定を変更しない。
- ephemeral test keyで署名した正しいMac/Windows packageを、隔離旧版アプリの通常UIから取得・検証し、実際に適用・再起動できる。再起動後のbinary hashとsource/build provenanceが候補と一致する。
- 不正signature、破損archive、対象OS不一致、代表的な適用失敗を拒否し、旧版と隔離データを保持して再試行可能な状態になる。既存unit testの成功だけを実機更新の証拠と扱わない。
- 検証対象head/base、old/new version、OS/architecture、binary/ZIP SHA-256、fixture server設定、実PID/path/state、適用前後の画面とcleanup結果を記録し、通常インストール・通常stateには変更がないことを確認する。
- 通常Release workflowや保護対象の`.github/`設定は変更しない。通常Releaseへ誤ってvalidation featureを含めない保証をworkflow側で追加する必要が判明した場合は、ここで変更せず運用基盤専用PRを別途提案する。

## 対象外

- 公開Release、tag、署名秘密鍵、本番のGitHub catalogへの変更。
- 通常アプリでの任意URL設定、一般ユーザー向けproxy/DNS/TLS設定、署名検証やredirect制限の緩和。
- #656のhelper名修正そのもの、Issue #510全体の受入済み判定、Mac/Windows GUI受入の代行。
- 通常インストール、通常のHane state、実credential、Windows既存Explorer登録を検証fixtureとして書き換えること。

## 検証と引き継ぎ

実装担当は、まずvalidation build境界とHTTP transportの現行契約を確認し、feature/profileを含まない通常Releaseとの差分を明示する。変更するのはこの検証経路に必要なコードと直接関連する回帰testだけとする。`.github/`、Commander Policy、AGENTS、Claude手順は変更しない。検証server起動・GUI実行・Release作成はworkerでは行わず、コードのpush後にCommanderがexact-head verifier/CIを確認し、GUI ValidatorがIssue #510のfocused scenarioを別途実施する。

この準備文書はtestabilityの実装契約であり、#510のMac/Windows更新受入を完了した証拠ではない。PR #656やこの準備PRだけでIssue #510をcloseしない。

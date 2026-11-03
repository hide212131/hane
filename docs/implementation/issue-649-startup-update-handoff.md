# Issue #649 起動時更新の実装準備・引き継ぎ

作成日: 2026-11-04（JST）。製品未実装・未検証。これはClaude実装待ちの準備文書であり、更新機能の完成報告ではない。

## 依頼と正本

利用者原文: 「よくアプリにある、起動時に更新チェック＆更新してほしい」。確認だけでなく、取得・適用・再起動後に使えるところまでが目的である。

- 要求・受入条件: [Issue #649](https://github.com/hide212131/hane/issues/649) A1〜A12。同一要求のため新規Issueは作らない。
- 具体仕様: [更新通知設計](../design/application-update-notifications.md)（[PR #651](https://github.com/hide212131/hane/pull/651)で採用済み）。本書はcurrent mainとの差分と実行先の準備を補う。
- 継続する設計判断: [ADR-0034](../adr/0034-zip-application-updates.md)。既存判断を変更しないので新規ADRは作らない。
- 本体更新・実適用の契約: [Issue #510](https://github.com/hide212131/hane/issues/510)、[本体更新仕様](../application-update-implementation-spec.md)、[Windows UAC仕様](issue-510-windows-uac.md)。通知の完成で#510を完了扱いにしない。
- Commanderの判断・担当: default branchの`docs/aadw-command-policy.md`と`docs/aadw-claude-execution.md`。本文・ソースは事実として読み、判断ルールを複製しない。

## 2026-11-04のcurrent facts

調査基準はmain `1e0d345e46d8c1805659968f8fad1148b0ae37a8`、workspace版`0.39.0`。GitHubのlatest Releaseもv0.39.0。着手時は再取得し、当時の記録をcurrent evidenceとして流用しない。

| 項目 | 確認結果と実装への意味 |
|---|---|
| 対象OS/CPU | Windows x64、macOS Apple Silicon。Intel Mac、Windows ARM、他OSは今回追加しない |
| 配布 | GitHub Releasesの`hane-windows-x64.zip`、`hane-macos-arm64.zip`。v0.39.0には`hane-update.json`と`hane-update.json.sig`も公開されている。assetの存在確認は署名検証や更新成功の証拠ではない |
| リリース入口 | `.github/workflows/windows-release.yml`。Cargo版とtagを照合し、両OSの成果物・更新署名を検査して公開する。tag/手動dispatch等の既存入口を使う。今回はworkflowや鍵を変更しない |
| Windows ZIP | 本体・helper・署名付きinventory、版別Explorer DLL、署名済みShellIntegration MSIXと証明書等。ZIP全体をMSIX配布へ変更しない。MSIX署名は本体更新用Ed25519署名と別の契約 |
| macOS ZIP | ad-hoc署名した`Hane.app`と外部の`Hane.app.update/<version>-<source SHA>/`。並置を維持する。Developer ID・有料登録・公証は現行#510の必須条件ではない |
| 真正性 | 既存の埋込み更新用公開鍵によるcatalog/inventoryのEd25519署名、ZIP hash/size・版・対象OS・配置・所有関係の検査を使う。HTTPSだけで真正性確認済みとしない。署名不正を手動承諾で迂回させない |
| 更新サービス | `crates/app/src/app_update.rs`に手動check/download/Ready/restartの既存経路。自動確認日程・購読APIはまだない。`main.rs`は通常windowへcontrollerを接続するが起動後自動確認をarmしていない |
| UI | `view/app_update.rs`に設定カードと再起動入口、`view/footer.rs`に幅別レイアウト。更新専用footer入口は未実装 |
| 初期化分離 | [PR #638](https://github.com/hide212131/hane/pull/638)はmerge済み。field初期値は`crates/ui/src/view/initialization.rs`。旧view.rsに初期化を再導入しない |
| 既存適用 | #512/#518/#526/#535/#539/#557、proxy #628/#631、Windows UAC #662と隔離検証feed #657はmerge済み。mergeは両OSの実更新・復旧が完了したことを意味しない。#510はopen |
| 重複/稼働 | 調査時open PR一覧は0件。#649の実装PRなし。最新run一覧はcompleted。Claude依頼直前には同Issue・同フェーズのrunを再確認する |

既存仕様の冒頭に残るv0.27.0や#535 Draft等は履歴であり、現在の配布状態にはこの調査とGitHub current factsを使う。

## 今回実装する範囲

採用済み設計のとおり、通常編集window表示後30秒かつローカルReady復元完了後にmetadata確認を一回開始する。以後は確認完了から1時間。Readyがあれば通信せず提示する。限定health起動・helper・debug・未対応配置では自動通信しない。起動・編集は待たせない。

起動時に新版が見つかったら一度だけ非モーダルに案内し、以後は下部ボタンで知らせる。「あとで」でも入口を残す。「更新」でその候補の取得を承諾し、検証後に別の「再起動して更新」で適用を承諾する。自動取得・自動終了・強制再起動はしない。設定画面へ移動せず完結させる。

未保存文書、IME、設定の未保存入力、保存等の処理中は既存restart gateを通して再起動を保留する。候補を保持し、編集へ戻って保存後に再試行できる。helper準備前に本体を終了しない。Windowsの必要時UACと取消保護、macOSの配置検査、限定health・durable commit・失敗復旧は既存経路を使う。

オフライン・timeout・403/429・署名不正でも通常起動と編集を継続する。確認不能を「最新版」に変換しない。失敗は既存診断へ残し、1/2/4時間の上限付きbackoffを使う。手動retryも同じ日程を守る。候補・操作世代を照合し、定期確認でDownload/Readyを差し替えない。

## 並行開発・先行条件

- 変更責務はapp所有の自動確認日程と状態変化通知、通常起動配線、更新パネル/footer入口。保存・署名・適用の別基盤を作らない。
- #638の初期化境界、#623のnetwork snapshot/HTTPS transport、#510のservice/restart gateはmainの受入済み経路を使える。今回の準備を待たせる未merge先行PRは確認されなかった。
- #309のopen/save/draft、#368のbackground job寿命、#567のUI分割、#312のrelease整理は関連する契約を照合するが、全体完了を開始条件にしない。実装前に該当Issue/PRを再取得し、同じ状態を変更中なら必要最小範囲だけ調整する。
- #510の実機残件は通知実装開始を止めない。ただしA5/A6/A12の取得→再起動→実更新の最終受入では、対象SHA/baseと主張が合う両OSの証拠が必要。通知だけのfixture成功で代用しない。
- #309にはdraft書込とchecked flushの完了順等のunknownが残る。通常終了のbest-effort flushで更新終了を許可せず、再起動保護に影響する範囲を制御テストと現行証拠で確認する。不足をfollow-up/passへ読み替えない。
- 既存共有作業ツリーの`crates/ui/src/view.rs`には未commit変更があるため、本準備は独立worktreeで行った。後続もその変更を破棄・取り込みしない。

## 検証と残る確認事項

設計書§10のT1〜T10、G1〜G4を使い、A1〜A12と対応付ける。fake clock/runnerで30秒・1時間・休止復帰・backoff・競合・終了時破棄を確認し、通常待機中の短周期poll/再描画を追加しない。署名・wrong target・破損拒否の既存回帰と、未保存時の保留を維持する。

製品変更前のmainと候補を同じOS/profile/fixtureで比較し、起動から入力可能まで、30秒付近の入力遅延、idle CPU/wake/再描画、request数・転送量・RSSを記録する。測定せず性能回帰なしとしない。

macOS/Windowsそれぞれで、起動案内→あとで→footer、設定なしの取得/取消、狭幅/IME/保存警告、未保存保留→保存→更新→一般権限で通常編集を確認する。署名された旧版→新版の実更新と代表的適用失敗からの復旧は#510の証拠を照合し、不足なら補う。OS/版/SHA/hash/配置を記録する。

Windows GUIは利用者が許可した既存Windowsチャットから直接行う。Mac経由VM操作は使わない。担当チャットの特定や検証枠が必要ならPR担当が既存記録を調べ、必要な利用者操作だけ依頼する。未実施/blocked/unknownをpassにしない。現在は本準備に関する製品テスト・GUI・性能測定を実行していない。

更新頻度・自動取得・再起動については既存#649に具体的な契約があり、今回の原文と矛盾しないため再質問していない。最初の更新対応版の手動bootstrapは#510どおり許容する。新たな署名資格情報・OS保護無効化・証明書自動信頼は要求しない。

## Jev判断とPR担当への境界

2026-11-04、既存local Jev CLIの`pick`をprovider `typesafe`、要求model `jev-latest`で実行。応答modelは`jev-1.14.0`、選択は`reuse`（confidence 1.0）。候補は「既存#649と採用済み設計の再利用」「利用者へ再質問」「新規Issue/ADRによる独立設計」。既存契約・main・重複PRなしの事実から再利用が選ばれた。認証情報は渡していない。

このPRは#649専用の**実装用Draft PR**。設計PR #651を転用していない。本チャットの担当は文書だけの準備commitとPR公開、別PR担当チャットへの引き継ぎまで。Claude実装依頼・受理確認以降はPR担当に任せ、本チャットから依頼を投稿しない。

PR担当はcurrent main/PR head/base・Issueの正本・権限・同じ対象の稼働runを再取得し、Jevで次の一actionを決める。実装依頼には対象branchから読める本書と設計書、A1〜A12、範囲・対象外を含め、default branchの実行手順に従う。文書準備だけを実装完了、run生成だけを受理済みと報告しない。

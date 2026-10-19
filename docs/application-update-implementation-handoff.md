# アプリ本体更新の実装引き継ぎ

作成日: 2026-10-17
状態: 設計はPR #509でreview後にmerge済み。実装は部分的に進行中で、OS実機検証・本番署名設定・配布有効化・U5は未完了。
設計時の基準main: `e82b9a9fe42280bfa78497746bcd7e21c73430c5`（設計時点のSHAであり、current mainではない。着手時にcurrent mainを確認する）
正本: [ADR-0034](adr/0034-zip-application-updates.md)、[詳細仕様](application-update-implementation-spec.md)
実装の進捗の追跡: [Issue #510](https://github.com/hide212131/hane/issues/510)

要求・受入条件の正本: [Issue #508](https://github.com/hide212131/hane/issues/508)

Issue #510は、WindowsとmacOSの実配布物で旧版から新版への更新と復旧を行い、U-01〜U-15の証拠がそろうまで完了扱いにしない。受入条件は縮小・弱化しない。

## 0. 進捗の要約

下表の状態はこの文書の更新時点（PR #518 head `b83abe9be752d189139feaec700ee1e2f96be46c`）で確認した事実である。実装時はGitHub上のcurrent factsを読み直す。

- 最新の公開Release v0.26.0にはWindows/Mac ZIPのみがあり、更新catalog/signatureは含まれない。更新対応の本番公開とU5は未完了。
- Windows/macOS実機での二版更新・復旧の証拠（U-11, U-13, U-14など）と、本番の署名鍵・Developer ID・notarization設定は未了。fixtureの成功を実OSの成功へ読み替えない。

## 1. 着手前の確認

current main、追跡Issue、関連PRと同対象の実行中runを読み直す。本設計PRを実装用に転用しない。設計採用後、各段階の実装Issueに目的・範囲・下表の受入条件と設計pathを記し、文書準備commitを持つ別の実装用Draft PRを作る。通常製品実装は[Claude実行手順](aadw-claude-execution.md)に従う。

| 重なる作業 | 調整する責務／待つ範囲 |
|---|---|
| #309 RF6-B | sessionの保存・rename・dirty/pending判定。current mainの`session_save`/`DocumentInstance`等の受入済み境界を使い、保存全体のリファクタリングを混ぜない。 |
| #312 RF7-C、release入口 | 同じrelease workflow/version/assetを変える場合は一方の受入を先に反映。#312全体の完了を一律条件にしない。 |
| #425、macOS open対応PR #500、file drop PR #507 | freeze中の外部open受付。busy/再試行を一つの入口で守り、実装時のcurrent headに合わせる。 |
| AI配布 #479/#481、設計PR #484 | 所有・catalog・保存先を共有しない。AI serviceの停止/再初期化境界だけ調整。本体更新の完成条件にruntime更新実装を含めない。 |
| #297全域リファクタリング | 更新機能固有の契約・試験を確認して進める。全域の完了待ちはしない。 |

設計中に確認したopen PRの番号は履歴であり、着手時の状態を推測しない。sourceや責務が移動したらpathを更新し、旧・新双方へ同じ更新判定を追加しない。

## 2. 実装単位と変更予定

| 段階 | 現在の状態（証拠） |
|---|---|
| U0 | merged。[PR #512](https://github.com/hide212131/hane/pull/512)（U0/U1/U3）に含まれてmerge済み。 |
| U1 | merged。PR #512に含まれてmerge済み。Windows 11実機でのExplorer登録等の確認（U-11）は未了。 |
| U2 | Draft/open。[PR #526](https://github.com/hide212131/hane/pull/526)、head `eb988f8405dd8fc542b6b5aa9394a8707a0aa67d`。baseは#518の古いhead `00bf87fd529398d9ee7305f74fdea899065f8260`のため、#518更新後の同期と再検証が必要。本番の署名設定は未完了。 |
| U3 | merged。PR #512に含まれてmerge済み。実署名metadata未準備のため適用操作は有効化しない。 |
| U4 | Draft/open。[PR #518](https://github.com/hide212131/hane/pull/518)、head `b83abe9be752d189139feaec700ee1e2f96be46c`。exact verifier run 39294842985のworkspace tests/lintは成功、Codex Review #6273442408は重大な問題なし。Windows/macOS CI run 39295116467は`action_required`でジョブ未開始のため、成功扱いにしない。両OSのU-08〜14は未了。 |
| U5 | 未完了。bootstrap出荷と二版更新の実行証拠は未取得。 |

| 段階 | 変更予定のpath／内容 | 先行条件と完了条件 |
|---|---|---|
| U0: 配置・protocolの土台 | `crates/update/{Cargo.toml,src/*}`、`crates/updater/{Cargo.toml,src/main.rs}`、root Cargo files。typed model、署名/カタログ検証、ZIP検査、lease、journal、検証用fake transport/filesystem。 | 設計採用。UI・実配布を有効化せず、U-01〜06/U-12のlibrary境界を満たす。helper単体に任意path/任意command実行入口を作らない。 |
| U1: Windows配置の更新対応 | `windows/shell-extension/{AppxManifest.xml.in,build-package.ps1,hane_shell_extension.cpp}`、`crates/ui/src/context_menu.rs`。version別DLLとrootのmarker/exe解決、登録状態のinspect/repair境界。 | U0のlayout決定。既存file/folder登録ON/OFFとCLIが維持され、ロード中旧DLLを保持した二版共存を確認。実際のZIP生成workflow変更は次の基盤PRへ分離。 |
| U2: 配布基盤専用PR | `.github/workflows/{windows-release,release-request}.yml`、必要な生成・検査script、`scripts/bundle-macos.sh`。両helper、Windows inventory署名、macOS署名/notarization、署名catalog、Draftからの一括公開、clobber禁止。 | U0/U1の仕様。利用者の今回の依頼に基づく配布設計だが、実変更は明示した基盤PRに限定。秘密鍵・Team ID・notary credentialsの設定は保守者作業。U-07/U-13を満たし、発行しない検証を先に行う。 |
| U3: 設定・取得と再起動準備 | `crates/app/src/main.rs`、新`crates/app/src/app_update.rs`、`crates/ui/src/{lib.rs,view.rs,view/app_update.rs,view/session_save.rs}`、両Cargo files。app所有service/一般設定card、Ready保持、freeze、終了許可、Result付きdraft永続化、明示AI停止。必要最小限の`crates/ai/src/service.rs`公開停止境界。 | U0と署名済みfixture。U-01〜09を満たす。実署名metadata未準備なら適用操作は無効。設定表示だけで通信しない。 |
| U4: OS適用・復旧と統合 | `crates/update/src/platform/{windows,macos}.rs`、journal/recovery、`crates/updater`、app起動最前段と`open_request.rs`。helper handshake、Windows所有file更新、macOS bundle交換、限定health、rollback、ticket復元。 | U0/U1/U3。実署名候補が必要なOS試験にはU2の対応環境も必要。U-08〜14を両OSで満たす。部分成功で更新成功表示を出さない。 |
| U5: bootstrap出荷 | README、配布手順、実行証拠。Cargo版更新は既存release入口。 | U0〜4の検証とcurrent-head reviewが揃い、`V_bootstrap`を手動導入→次の署名済みtest版への二版更新を両OSで確認後に機能を有効化。実配布の実行はこの設計PRに含まない。 |

段階分割はレビュー可能な変更単位の目安。U3とU4を独立にmergeする場合は未完成の適用buttonを利用可能にせず、実装がつながっていないのに完成済みとしない。CIのpath分類に新crateとhelper buildが含まれることもU2で確認する。

## 3. 不変条件と必要な試験

下表のIDを実装PRのtest/scenarioと結果へ対応付ける。テスト名を実装詳細の写しにせず、境界をまたぐ不変条件と実際の失敗を確認する。製品実装前の本PRでは実行済み扱いにしない。

| ID | 検証する振る舞い／代表入力 | 方法・証拠 |
|---|---|---|
| U-01 | stable/SemVer/OS選択。現在版・新版・旧版・prerelease・別CPU・重複asset・404/403/429・metadata欠落を区別。 | deterministic fixture。版文字列の辞書順で`0.9`と`0.10`を誤判定しないこと。 |
| U-02 | catalog raw byte署名、domain違い、改ざん、未知/旧鍵、期限・未来時計、sequence後退/同値別bytes、保存不能を拒否。 | 既知のtest鍵による正負署名fixture。実署名鍵はtestへ入れない。 |
| U-03 | TLS/redirectのhost検査、上限、途中切断、read timeout、disk full、cancel後の遅い結果、Readyの再検証。 | fake transport/clock/容量と、実TLS取得の限定smoke。起動・設定表示だけではrequest数0。 |
| U-04 | ZIP traversal/ADS/予約名/link/reparse/Unicode・case衝突/重複/zip bomb/余分な実行物を拒否。stage外のfile不変。 | 悪意ある小さいarchive fixtureと実filesystem試験。manifest宣言と実byte数の双方を超過させる。 |
| U-05 | 配置移動、別owner、別InstallId、別`HANE_STATE_DIR`、開発build、memory state、readonly/network filesystemで対象を誤認しない。 | 実一時配置でidentity変更・override分離。2配置の片方の更新で他方が不変。同じ配置のstate override変更で最大catalog sequenceが下がらない。 |
| U-06 | app所有service、二重クリック、カテゴリ往復、window寿命、stale op/candidate、cancel、通常終了後のReady保持。 | service/UI state試験。再起動時はローカル検証のみ、適用は明示操作後だけ。 |
| U-07 | ZIP→inventory→catalogの版/hash/source/署名整合、未知schema/protocol拒否、同tag ZIP不変、二target不足で公開しない。 | 発行しない配布fixture/構成テスト。実publishをtest代わりにしない。 |
| U-08 | dirtyなnamed/untitled/work-folder draft、autosave OFF、別tab、未保存AI/Prompts、IME、Save As取消、save/rename/AI中、draft書込失敗でquit不可。 | 保存境界とUI試験。最後の打鍵、非active tab、stale DocumentInstance、freeze直前の外部openも含む。文書byte/draft/設定が失われないことを確認。 |
| U-09 | freeze後の編集/open競合、helper起動失敗、Preparedなし、permit後の世代変化、AI停止timeout、親未終了でmutationなし。 | private IPC偽装/切断fixtureとprocess試験。Abort後は編集と必要なAI serviceが戻る。 |
| U-10 | 同一配置の別process、通常起動競合、別userの読取lease、異なるstate override、PID再利用。通常の外部open成功を偽装しない。 | 実OSの2process以上で検証。OS lockをmockだけで済ませない。 |
| U-11 | Windowsで所有fileだけ更新。使用中旧shell DLLと未知の隣接Markdownは不変。既存marker別ON/OFF、未登録、他owner、Publisher変更、MSIX失敗を区別。 | Windows 11で実Explorer登録/選択/起動。古いDLLから新旧exeにfileとfolderを渡し、更新後の別version DLL登録も確認。 |
| U-12 | 各journal flush前後、各file置換前後、bundle swap直後、health前後、durable commit直後のprocess中断で、旧版復旧または新版commitを決定。未知hash/backup破損では停止。 | checkpointごとのhelper強制終了による故障注入。一般編集processをkillして保存を検証したと主張しない。新旧混在で通常editorを開かない。 |
| U-13 | macOSの正しい署名/Team ID/Bundle ID/版/notarization、別Team・不正署名・translocation・unsupported FS・権限不足・交換失敗。 | 実macOS/APFS、隔離属性を保ったブラウザdownloadで新規/upgrade/別配置を確認。OS評価をfixtureで成功させただけで完了しない。 |
| U-14 | 実二版のDownload→手動Restart→health→Commit、起動不能候補のrollback、ticket一回復元、commit後cleanup失敗。 | Windows/macOS双方の版・source SHA・archive hash・配置・OS版付きlog/画面。文書、AI保存領域、ログイン、default folder、recent、Explorer登録を更新前後で比較。 |
| U-15 | 通常の入力/IME/保存・再読込/undo-redo、複数tab、設定Escape、外部open、AI選択処理の回帰と性能。 | 既存workspace test/Clippy/required CI＋focused GUI。本文入力中に通信/ZIP処理がなく、無操作起動のrequest数0。起動/入力遅延の変更前基準と同条件比較を残す。 |

## 4. 受入判定

- U0〜U4の実装結果は対象headとcurrent main/baseに結び付ける。通常のworkspace tests/Clippy、required CI、CodeRabbit full review、必要なfocused GUIをCommander Policyに従って確認する。
- 実機・署名credentials・外部serviceを使えない場合は、未確認のID/OSを明示する。fixture passを実OS passへ読み替えず、当該OSの適用機能を有効にしない。
- 更新対応版の利用者が「最新に更新する」から取得し、「再起動して更新」で新版へ到達できること、再起動を保留すべき場合に編集内容を守ることを主目的とする。検証が必要な最小範囲を超えて既存保存/AI/リファクタリング全体を作り直さない。
- 文書PRのmergeでは本体更新の受入条件を達成したと扱わない。bootstrapの署名鍵・Developer ID・notarization設定と二版更新の証拠が揃うまでは未出荷。

## 5. 実装担当へ渡す内容

将来の実装用PRには、対象段階、追跡Issue、40桁current head、基準main、上表の変更対象/対象外、当該U-ID、実装branchから読める上記3文書を渡す。URLだけを渡さない。通常の製品workerに配布基盤用secret、shell/Git認証、保護pathの変更権限を渡さない。

配布基盤PRは別の権限境界で扱う。署名鍵の実値やmacOS Team IDを本設計のplaceholderから推測せず、保守者が設定した実値でbuildを検証する。設計上の判断を実装中に変える必要が出たら、当該仕様/ADRへ理由と採用案を反映してから進める。

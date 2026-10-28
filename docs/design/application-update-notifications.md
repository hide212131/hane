# 起動時・定期の本体更新確認とステータスバー通知

作成日: 2026-10-28（JST）
状態: 実装前の設計。製品コード・テストは未実装、実機検証は未実施。文書PRの保存・採用と機能の完成を区別する。
要求・受入条件: [Issue #649](https://github.com/hide212131/hane/issues/649)
調査基準: main `497419203834f2a7aca413d1cb7758b37eb9dff6`、workspace version `0.36.1`。

## 1. 採用する変更と既存契約

通常の編集ウィンドウを表示してから30秒後に本体更新のmetadataを確認する。使用中は確認完了から1時間後に次の確認を行う。初回の新版発見では、入力を妨げない「更新しますか？」の案内を一度出す。以後の発見は下部ステータスバーのボタンで知らせる。取得と再起動は別々の本人操作で行う。

これは[ADR-0034](../adr/0034-zip-application-updates.md)の確認入口を追加する変更であり、署名・配布・取得・適用・復旧の別基盤を作らない。同ADRの決定1と[従来仕様](../application-update-implementation-spec.md)の起動時無通信・定期確認対象外を、本Issueの実装時に限って置き換える。従来版が自動確認を実装済みであるとは扱わない。

設定画面の表示・保存は確認のトリガーにしない。AIの接続、runtime/Skillsの取得・更新を起動時に追加しない。[HTTPプロキシ設計](http-proxy.md)のP10は、本体更新metadataの自動確認だけを例外とする。起動時に通信が増えることは製品の利用案内にも記載する。

## 2. current mainで確認した境界

| 対象 | 現行の事実と変更する責務 |
|---|---|
| `crates/app/src/main.rs` | `start_update_service`はserviceを作り、`AppOwnedUpdateService`がapp寿命で所有。通常windowに`attach_app_update`でcontrollerを渡す。限定health起動は別経路。通常windowが開きサービスが接続された後にだけ自動確認をarmする |
| `crates/app/src/app_update.rs` | `AppUpdateService` / `Inner`に署名済みOfferと操作状態がある。`start_check`は手動入口で、AvailableもCheckingに置き換える。`Ticket`によるInstallId/OperationId/CandidateId照合を維持し、確認のorigin・自動確認日程・変化通知を追加する |
| `crates/ui/src/view/app_update.rs` | controller/snapshot、`card_model`、`run_app_update_action`、`start_update_restart`が既存入口。`poll_app_update`はbusy中だけ200msごとにsnapshotを読み、終わると停止。設定外での発見には通知の購読が必要 |
| `crates/ui/src/view/settings_screen.rs` | 設定を開くと`restore_app_update_local`を呼ぶ。Ready復元は通常window接続後にも一度開始し、設定表示からの呼出しはidempotentなままにする |
| `crates/ui/src/view/footer.rs` | `FooterLayout::{Full, Compact, Narrow, Single}`と一行のfooter。全レイアウトで更新専用ボタンの幅を確保する |
| `crates/ui/src/view.rs` | controllerと`app_update_polling`、初期値を持つ。#638採用後は初期値が`view/initialization.rs`へ移る。実装開始時の配置で一度だけ更新する |
| `crates/app/src/https_transport.rs` / `crates/update/src/download.rs` | LazyHttpsTransportがoperation開始時にnetwork snapshotを固定。latest release JSON、catalog、署名の取得・検証は既存checkを使う。ResponseはETag/Retry-Afterを公開していないため、それらへの対応を今回の必須条件にしない |
| `crates/update/src/limits.rs` | metadata合計期限30秒、接続期限10秒、既存サイズ/redirect上限。自動確認でも同じ上限を使う |

既存のHTTP API実装の複製、フレームごとの確認、windowごとの独立したnetwork timerを追加しない。1回の確認はlatest JSON・catalog・署名の取得を含み得る。「1回」はHTTP 1 requestを意味しない。ZIPの取得は含まない。

## 3. 所有、起動順、終了

自動確認の日程はapp所有の更新サービスに一つだけ置く。最小の`AutoCheckSchedule`（名称は例）は次回期限、連続失敗数、起動時確認の未処理フラグ、停止/再起動準備状態だけを持ち、永続化しない。候補・結果の正本は既存serviceのままにする。

実装はapp所有の取消可能なtimerを使う。標準ライブラリで実装する場合は、単一の待機threadとCondvarで期限・状態変化・停止を待ち、期限時に既存runnerへcheckをdispatchする。待機thread自身でHTTPを行わない。待機中はserviceの強参照を無期限に保持せず、app所有のguardが停止通知とthread終了を管理する。state lockを保持したまま待機、通知の送信、joinをしない。別の同等な既存timerを使う場合も、この所有・停止・無pollingの契約を守る。汎用scheduler crateは導入しない。

起動順は次のとおり。

1. 既存startup gateを通し、通常のサービス・window・文書復元を開始する。限定health child・helper・復旧画面ではarmしない。
2. windowのcontroller接続時に変化通知を購読し、直後にsnapshotを読み直す。購読と初期snapshotの間の変化を取りこぼさない。
3. 対応配置のserviceに対してローカルのReady復元を一度開始する。ファイル検証は既存runnerへ送り、UIを待たせない。
4. window表示成功後を基点に30秒の期限をarmする。Ready復元がまだ実行中なら、その完了通知まで初回確認を保留する。30秒より前にnetwork確認しない。Readyが見つかれば通信せずReadyを提示する。

自動確認の対象はrelease build、対応stable版/OS、configured trust policy、更新可能な既知の配置に限定する。debug build・非対応配置・App Translocation・service未接続時は日程をarmしない。既存のUnavailable理由を設定で参照できる。テストはclock/runner/eligibilityを注入し、製品のtrust/placement gateを公開flagで迂回しない。

IssueのA10で破棄の対象とするtimer/task/subscriptionは、app/service終了時、serviceの差し替え時、app解放時のものである。これらの終了・差し替えでは待機を取消し、購読receiver/taskを閉じる。個々の確認・取得の完了時は、その操作用taskと一時的な進捗pollを終了し、通常稼働中の次回確認timerと状態変化の購読は維持する。再起動prepareに入る前に自動確認を一時停止する。再起動が保留/取消になったら、保持する候補と日程を復帰させる。既存のserviceの停止・helper handoff順序を変えない。終了許可後に新しいcheckをdispatchしない。

## 4. 日程と操作の競合

時刻は既存のcatalog有効期限検証用のUTC clockと分け、日程には単調時間を使う。壁時計の変更で連続実行しない。休止中は実行せず、復帰時に期限が過ぎていれば一度だけ確認する。複数回分を追いつき実行しない。

| 状態/結果 | 自動確認と次回期限 |
|---|---|
| 初回、Idle | 通常window表示から30秒が経過し、起動時のReady候補のlocal復元・再検証も完了するまでrequest数は0。両条件成立後にmetadataを一組だけ要求する。restore/手動checkと重なるrequestは完了を待って一つにまとめる |
| UpToDate / Availableの確認成功 | 確認完了から1時間後。Availableも毎時再確認するが、表示中の候補は下記の方法で保つ |
| network/timeout/rate limit等の自動失敗 | 最初の失敗完了から1時間後、連続2回目は2時間後、3回目以降は4時間後。成功で失敗数を0に戻す。短周期の自動再試行をしない |
| Downloading / Verifying / Ready / Discarding | 新しい自動確認を開始しない。期限切れは一つのpendingに集約。Readyを破棄するなどして確認可能に戻ったときに一度だけ実行し、以後はその完了から日程を立てる |
| Checking / restoring | networkを並行開始しない。checkの完了は自動/手動共通で次回期限を更新。restoreだけでは成功したnetwork確認と数えない |
| restart freeze / helper handoff | 自動確認停止。停止前の遅延結果を既存ticket guardで拒否し、Readyを差し替えない |
| Unavailable / serviceなし | 自動確認しない。期限待ちthreadを常駐させる必要もない |

「もう一度確認」「再試行」はbackoff中でなければ即時に開始し、実行中checkへ合流する。自動・手動どちらの失敗も共通の連続失敗数を1増やし、失敗完了から1時間（初回）、2時間（2回目）、4時間（3回目以降）を次回の自動・手動checkの実行可能時刻とする。backoff中の手動retryはrequestを開始せず、期限を短縮・resetできない。手動確認が成功すれば失敗数を0へ戻し、次回自動確認をその完了から1時間後に予定する。設定の開閉、文書切替、外部open、focus復帰を追加のnetworkトリガーにしない。

## 5. 既知候補を保つ確認と通知API

既存の`check()`は手動用として維持し、自動用の入口とorigin（Startup / Periodic / Manual）をservice内で区別する。内部名は変更できるが、originは操作を開始した時点で固定する。UIの表示中カテゴリから推測しない。

Availableの自動再確認では、既存のOfferを検証中のcheckとは別に保持する。snapshotは既存候補をAvailableとして返し、補助値`checking`と確認originを返せるようにする。初回や候補なしでは従来のCheckingを表示する。`is_busy`は補助checkも扱う。候補の不透明ID・版・署名済みcatalog bytesの所有はserviceに限る。

- 同じ有効候補なら表示を維持し、新しい署名済みの候補なら完了時に一括で置き換える。現在版と等しいことを検証できた場合だけUpToDateへ移る。
- 再確認が失敗した場合は、その失敗を診断と最終確認結果へ残す。既知の候補は、既存の検証で引き続き利用可能な場合だけ維持する。期限切れ・trust不適合を利用可能と扱わない。候補がなければFailedとなり、「最新版」と表示しない。
- 再確認中に本人が既知候補のDownloadを選んだ場合、確認を取消し、そのOfferを元の候補IDで取得へ移す。先に確認が完了して候補IDが変わっていれば取得せず、表示を更新して再承諾を求める。古いボタンで新しい版を取得しない。download直前のcatalog有効性も再確認する。
- Readyは定期確認で差し替えない。手動Checkも既存の準備済み候補保護に従う。取消・破棄・失敗・遅延完了の全経路でTicketを照合する。

状態変化は`AppUpdateController`に購読APIを追加して伝える。既存workspaceの`async-channel = 2.5.0`を使い、購読ごとに容量1の`Receiver<()>`を作る（app側にもworkspace依存を宣言する）。一つのreceiverをcloneして複数viewで奪い合わない。serviceは送信側だけを保持し、state lock解放後に`try_send`する。Fullは「未処理のwakeが既にある」として合流、Closedは登録を除去する。

通知はstateの正本ではなく「snapshotを読み直す」wakeである。確認開始/成功/失敗/取消、restore開始/完了/dispatch失敗、Download開始/終了、Ready、discard、restart能力/配置理由の変化など、可視状態を変える全経路で発行する。UIは受信時にsnapshotを読み、通知値を別の候補状態として保存しない。service差し替え時は旧購読を閉じ、接続世代の異なる結果を捨てる。

200msの既存pollは受信bytesを表示するDownload/Verifying等の処理中だけに限定できる。Checkingの完了と待機中の発見はwakeで反映する。アイドル時にpollを起動し続けず、同じbusyに対して二重loopを作らない。wake taskはviewの弱参照を使い、detachしたtaskがviewやcontrollerを保持し続けない。

## 6. 起動時の案内と下部の操作

設定とは独立した小さい更新パネルを、既存footerの上に必要なときだけ表示する。起動時の案内も、footerボタンを押したときも、この同じパネルを使う。描画・actionの対応は`app_update.rs`に集め、設定カードと`card_model`の状態文言・既存actionを共有する。`open_settings`を呼ぶ経路にはしない。

| serviceの状態 | 下部の表示と操作 |
|---|---|
| Idle / Checking（候補なし）/ UpToDate | 更新ボタンなし。自動確認中のspinnerを常時出さない |
| Available | `↑ 更新できます`。tooltipに候補版。押すと「Hane x.y.zに更新しますか？」と「更新」「あとで」を表示。更新でそのCandidateIdのDownload |
| Downloading / Verifying | `更新を準備中…`。押すと同じパネルで進捗と「キャンセル」。編集は続けられる |
| Ready | `再起動して更新`。押すとパネルに準備済み版と同名の明示ボタンを出す。この二つ目の操作で既存`start_update_restart`を呼ぶ。footerクリックだけでは終了しない |
| RestartBlocked | Readyボタンと候補を残し、パネルに既存の保留理由。「編集に戻る」から保存等を行い再確認できる |
| Failed | 候補なしの自動失敗は割り込みも点滅も出さず既存診断に残す。本人がパネルで開始した操作の失敗は、そのパネル内に理由と「再試行」/既存手動案内を出す |
| Unavailable | 通常の更新ボタンなし。設定の既存理由と手動案内を使う |

初回の自動確認がAvailableになった場合だけ、同じパネルを自動表示する。Readyのローカル復元でも、通信せず「更新の準備ができています」「再起動して更新」「あとで」を起動時の一回として案内できる。プロセス内に起動時案内済みのフラグを置き、「あとで」後、定期確認の新しい版、設定の開閉で自動再表示しない。次の通常起動では改めて一度案内できる。最初の自動確認が失敗/UpToDateなら、その後の定期発見を起動時案内として扱わない。初回に手動確認が合流した場合も新たな自動案内を重ねない。

自動表示でフォーカスを移動しない。本文・sidebar filter・inline rename・設定入力等のIME変換中も入力を維持する。本人が更新操作へ移る場合は、既存のcomposition保護に従い、未確定文字がある間は操作を保留する。「あとで」/閉じる操作は取得を始めず、ボタンも消さない。パネルを閉じても開始済みのDownloadは続き、取消は明示操作だけとする。

パネルが文書のviewportを縮める場合は既存layout/scroll anchor経路へ高さを反映し、入力行を隠さない。保存警告の一時statusを更新通知で上書きしない。エラーコード・候補ID・schedulerの説明は通常の文言に露出させず、既存診断へ置く。

## 7. 狭い幅とアクセシビリティ

Full / Compactでは短い文言の専用更新ボタンを右側へ追加する。Narrow / Singleでは同じ操作のアイコンボタンへ縮め、tooltipとaccessible nameに「更新できます」または「再起動して更新」を付ける。更新ありを「その他」内部だけに隠さない。保存状態・警告を左に残し、文字数/zoom/AI等の既存折り畳みを先に使う。Singleも「状態と操作」+更新アイコンの二つが収まる計算に変更する。

`footer_layout_for_width`の固定必要幅には更新ボタンの有無/状態別の幅を含める。文言が長いReadyでもoverflowさせない。許可される最小window幅、sidebar表示時、文字数の桁が多い場合を確認する。footerは既存の一行高さを保つ。キーボードから更新ボタン・パネル内の操作へ到達でき、閉じると元の有効なfocusへ戻す。自動表示時はkeyboard focusを取らず、IME中のTab等の既存処理を奪わない。

## 8. 変更範囲と実装順

一つの機能実装PRを基本とし、機械的分割を混ぜない。

1. app_update serviceにorigin、単調時間で計算する日程、取消/停止、Available保持checkとwake購読を追加する。fake clock・runner・Transportで境界を先に検証する。
2. mainの通常window成立後に購読/ローカルrestore/自動確認を接続する。限定起動・helper・非対応環境を除外し、app寿命で停止を所有する。
3. UIの共通更新パネルとfooterの専用入口を追加する。既存のDownload/Cancel/Restart/Discard経路を使う。設定カードも同じsnapshotを使い、二重に状態を管理しない。
4. 関連テスト、通常build、focused GUI、負荷比較を行い、実装結果と未確認をPRへ記録する。

想定ファイルは`crates/app/src/{main,app_update}.rs`、必要な同責務のprivate module、`crates/app/Cargo.toml`、`crates/ui/src/view/{app_update,footer,settings_screen}.rs`、viewのfield/初期値、対応tests。起動時無通信を前提としたコメント・テスト、利用案内も限定的に更新する。`crates/update`の署名・helperやAI/runtimeに機能を重複実装しない。release workflow、AADW、秘密情報、OS保護設定は変更しない。

## 9. 関連作業と着手条件

- #510のservice/card/restart gateはmainにある。今回の通知設計は#510の未完了の二版検証を待たず進められる。後続の実適用受入では元の必要な保護・OS証拠を確認する。通知テストの成功で#510をcloseしない。
- #623の受入済みnetwork contextとHTTPS transportを使用する。独自の直接接続やproxy資格情報の保存を追加しない。旧P10の無通信assertは、サービス生成/設定表示・保存と、30秒後の明示的な自動確認armを分けて更新する。
- #638は初期化の機械分離であり、調査時はopen。設計は現在のfield/初期値の所有を確認済み。実装開始時に再確認し、同じ初期化の移動が未受入ならその小範囲だけ先行受入を待つ。受入後は`view/initialization.rs`へ初期値を追加する。#567全体/#297/RF0を待たない。
- #643のsidebar hover、#640のGUI lockは独立。view.rsの差分衝突だけなら受入済みmainへ通常のGit操作で統合し、新旧双方へ同じロジックを追加しない。GUIの実行枠の確保はGUI検証の開始条件であり、設計や単体テストを停止しない。

実装開始時にcurrent main、Issue本文、関連PR/runを読み直す。本設計用PRを無断で実装用に転用せず、採用後に文書準備commitを持つ別の実装用Draft PRを用意する。通常の実装・review・GUI・mergeの担当と判断はCommander Policyに従う。今回はClaude実装依頼を投稿しない。

## 10. 試験と受入証拠の対応

以下は後続実装に必要な試験計画であり、実行済みの結果ではない。sleepで30秒/1時間を待つテストにせず、schedulerの日程計算とrunnerを注入して時刻を進める。現在の`app_update.rs`のQueued Runner/Transport fixtures、`footer.rs`の実EditorView描画テストを拡張する。

| 試験 | 条件・確認する不変条件 | 対応 |
|---|---|---|
| T1 起動 | Readyが見つかった場合はrequest 0。Readyがない場合だけ、通常window表示から30秒経過かつReadyの復元・再検証完了の両方を満たすまでrequest 0とし、成立後にmetadata一組。復元完了が30秒を超える場合も、完了前は0。通常window失敗/health child/helper/debug/非対応配置では0 | A1/A6/A10 |
| T2 日程 | check完了直前/1時間直前/期限、長時間休止からの復帰、壁時計前進/後退、window再接続/文書切替。未実行の連続dispatchなし | A2 |
| T3 失敗 | network/timeout/403/429/署名不正、手動/自動共通の失敗数と1/2/4時間backoff、期限中の手動retryで期限を短縮できないこと、成功reset。最新版と誤表示せずZIP取得0 | A6/A8 |
| T4 競合 | 初回restoreが30秒を超える、同時手動/自動、Available再check中のクリック、候補変更直後の古いクリック、取消後の遅延完了、Download/Ready/Discard/restart freeze | A7/A10 |
| T5 変化通知 | 購読前後のrace、容量1の合流、二つの独立購読、service差し替え、view解放、runner起動失敗、全terminal transition。待機中に継続pollなし | A4/A9/A10 |
| T6 案内 | Startup Available/Readyだけ一度、あとで→毎時同版/新版、初回UpToDate/失敗→定期発見、手動合流、次回起動。強制focus/Download/quitなし | A3/A6 |
| T7 footer/panel | Full/Compact/Narrow/Single、最小幅+sidebar、大きな文字数、各状態のラベル/hit bounds、設定閉のままclick、進捗/取消/失敗/再試行 | A4/A5 |
| T8 編集保護 | 自動表示前後の本文/selection/undo/dirty/scroll維持、本文/sidebar/rename/設定のIME、パネル高変更、保存警告維持、keyboard focus復帰 | A3/A4/A6 |
| T9 restart | 未保存文書/設定・未確定操作で保留、Ready維持、helper失敗でquitしない、取消で再arm、許可後のcheckなし。既存restart testsを再利用 | A5/A6/A7/A10 |
| T10 proxy | 自動checkもbegin_operationのnetwork snapshotを使う。設定変更は既存操作のsnapshotを変更しない。直接接続禁止/407/TLS/期限/redirect/redaction維持、AI/runtime request 0 | A11 |

実装後の実行候補（追加test名は実装PRで確定する）:

```sh
cargo test -p hane app_update
cargo test -p hane proxy_update_tests
cargo test -p hane-ui app_update
cargo test -p hane-ui footer
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --release --locked -p hane -p hane-updater
```

macOS / Windowsの通常CI、focused GPUI event testとnative GUIを使う。GUIでは正常な配布配置を使い、fixtureで作る安全な更新候補とfake transportで30秒/1時間境界を検証し、通常releaseでも少なくとも起動→実metadata確認→下部通知の経路を観測する。テスト用時刻短縮を通常releaseへ残さない。GUI必須scenarioは、G1 起動案内/あとで/定期発見、G2 設定なしで取得/取消/Ready表示、G3 狭幅/IME/保存警告/フォーカス、G4 未保存で再起動保留→保存後に更新、の4群。通信観測と表示対象の版/候補を対応付ける。G4の実適用は実署名された二版で行い、既存#510の証拠を使う場合もcurrent code/baseで対象主張が成立することを確認する。

Windows GUIは接続先WindowsのChatGPTアプリから直接実行する。Mac経由のVM操作は使わない。GUI占有・取得障害時は既存復旧手順に従い、未実施/Unknownの項目と再開条件を残す。fixtureのpassを二版更新・実IMEのpassへ読み替えない。

負荷は実装前基準SHAと実装候補SHA、同じrelease profile/fixture/OSで比較する。起動から編集可能までの既存timing-probe、30秒を含む起動直後のCPU/入力遅延、10分間のidle CPU・wake/再描画数、metadata request数と転送量、長時間常駐時のRSSを記録する。自動確認待機中に追加する短周期wake/再描画が0、1回の確認中も本文入力を待たせないことを確認する。大きな性能回帰は原因を調べ、平均CPUの小ささだけで入力停止を隠さない。今回の文書設計に製品性能の実測済み主張はない。

## 11. ロールバックと今回の完了境界

製品の自動確認を取り消す場合はarmとfooter/panelの接続を戻せば、既存の手動設定カードが使える。候補/設定/秘密/journalの永続形式は追加しない。既存の署名済みReadyと復旧履歴を削除する必要はない。新たなサーバー、鍵、push通知登録、常駐サービスは不要。

今回の完了はIssue・本設計・旧文書との関係・実装開始条件・試験対応の公開まで。製品実装、CIによる製品検証、GUI実行、実装worker起動、機能Issueのcloseは含まない。実装PRの受入はIssueのA1〜A12と、そのcurrent contextに結び付く証拠を改めて確認する。

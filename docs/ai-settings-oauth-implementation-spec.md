# Issue #379: OAuth・AI設定画面 実装仕様

- 対象: [Issue #379](https://github.com/hide212131/hane/issues/379)、親 [#376](https://github.com/hide212131/hane/issues/376)
- 上位判断: [ADR-0032](adr/0032-embedded-codex-app-server-ai-foundation.md)
- 作成日: 2026-09-28
- 設計状態: 実装分解・状態遷移・受入条件を定義済み。§12の安全プロファイルには実機契約検証が残る。設計を記載したことを、製品実装・実接続検証の完了としない。
- 調査したAIコード: `5b37e7a43fee5abf6f8ee591d6019b8b8d96af15`（#385 merge）
- 文書作成ブランチの基点: `d1e35016bd8dfc3d72256ef786ff747a0e105c3c`（#390 merge）。両者間の変更対象は `crates/ui/src/view.rs`、`view/viewport.rs`、リファクタリング計画。AIコードは同一。
- プロトコル・実行形式: **Codex 0.157.1 standalone `codex-app-server` / stdio**。最新Webドキュメントをそのまま採用せず、同梱版のschemaを正とする。

本書の型・関数名は追加予定の内部契約であり、現行APIが存在するという意味ではない。上位ADRのStatusは本書の追加だけでは変更しない。

## 1. このIssueで完成させるもの

既存の全画面設定にAI設定を追加し、ChatGPT/Codexへのログイン、Custom Providerとの明示的切替、モデル選択、固定入力の応答確認を提供する。ChatGPT接続1件とCustom接続1件を保持し、稼働するApp Serverは1つだけとする。

保存は推論を実行しない。ログイン・モデル一覧取得も接続テストの代わりではない。文書本文、選択範囲、編集中ファイルのパスを接続テストへ自動添付しない。

対象外はチャット画面、文書編集エージェント、履歴管理、任意プロンプト、shell/MCP/Skills/Pluginsの利用、プロバイダーの自動fallback、複数接続の同時実行。配布物への同梱・署名・インストーラーの完成は後続#380で扱う。ただし、本Issueの実機検証には製品と同じstandalone実行形式を使う。

## 2. 現行コードからの実装差分

| 現在の事実 | 本Issueでの措置 |
| --- | --- |
| #377/#378のランタイム、設定、秘密保存、ロック、journalがある | 作り直さず拡張し、GPUIから利用するアプリ所有サービスを追加する |
| `connect.rs`ではsettingsの置換後にもjournal処理が失敗し得る。旧key削除失敗時には保存成功とjournal残存が両立する | 保存APIの戻り値で「未保存」「保存済み・後処理待ち」「永続化の耐久性未確認」を区別する |
| runtimeの通知転送には2か所のbounded `try_send`があり、満杯時に通知を落とす | OAuth/turn完了を黙って捨てない。両方の境界に欠落検知を入れる |
| `call_with_generation_check`の共有ロックは単一RPC呼び出し単位 | 接続テストでは開始前からterminal/停止確認まで共有ロックを保持する |
| ChatGPT側runtime config builderは専用CODEX_HOMEを設定するが、選択モデル・安全プロファイルの反映は不足 | 両接続を同じプロファイル構築経路に通し、保存世代と適用世代を一致させる |
| `RuntimeConfig`のspawnは親環境を継承し、専用cwd指定がない | 専用cwdと環境変数ポリシーを追加する |
| 既存の実接続テストはenv変数名にApp Serverとあるが、full CLI用の`app-server`サブコマンドを指定する | standalone用fixtureを必須にし、full CLIテストとは分離する |
| 設定画面がEditorViewにあり、AIサービスのcomposition rootへの接続はない | AI専用UIモジュールを追加し、既存Editor本体の大規模改修を避ける |

参照: [connect.rs](../crates/ai/src/connect.rs)、[runtime.rs](../crates/ai/src/runtime.rs)、[settings.rs](../crates/ai/src/settings.rs)、[provider.rs](../crates/ai/src/provider.rs)、[実接続テスト](../crates/ai/tests/custom_provider_runtime.rs)、[app/main.rs](../crates/app/src/main.rs)。将来のコード変更で相対リンク先が更新されても、調査基点は冒頭のSHAで識別する。

## 3. 確定するユーザー操作の意味

| 操作 | 契約 |
| --- | --- |
| 接続方式・設定値を編集 | draftのみ。runtime・秘密ストア・保存済み設定を変更しない |
| 保存 | 検証、revision照合、必要な秘密更新、原子的保存、必要なruntime再構成。推論なし |
| 変更を破棄 | draftを保存済みsnapshotに戻す。OAuthを取り消さず、ログアウトもしない |
| ログイン | **保存済みのアクティブChatGPT接続**に対して実行する |
| ログイン取消 | 対象attemptの取消を要求し、account/readで実状態を確認する。ログアウトとは別操作 |
| 状態を更新 | account/readを実行する。必要に応じたtoken更新はApp Serverに委ねる |
| モデル一覧を更新 | ChatGPT接続のmodel/list。選択・保存・推論を自動実行しない |
| ログアウト | ChatGPT用App Serverのaccount/logoutのみ。Custom keyや一般のCodex CLI資格情報に触れない |
| 接続・応答を確認 | 保存済みの接続・モデル・資格情報で固定入力を1回実行する。課金/利用枠消費の可能性を表示する |
| ランタイムを再起動 | 保存済み設定を再適用する。ログイン/テストの自動再実行はしない |
| 復旧を再試行 | 所有権と排他ロックの下でjournal/耐久性確認を再実行し、最新状態を読み直す |

未保存のAI変更がある間、新しいログイン・ログアウト・モデル取得・接続確認・再起動を無効にして「先に保存または変更を破棄してください」と表示する。ただし進行中のログイン/接続確認の**取消は常に可能**にする。

プロバイダー切替は選択直後ではなく保存時に確定する。切替時に他方の設定・key・ChatGPTセッションを削除しない。アクティブでないChatGPTのログイン状態を調べるためだけに別のApp Serverを起動しない。

## 4. 所有者と責務

```text
app composition root
  └─ AiServiceHandle（同一アプリプロセス内で共有）
       ├─ AiService（操作受付・状態の唯一の更新者）
       ├─ bounded worker（ブロッキングI/O、既存runtime呼び出し）
       ├─ RuntimeCoordinator（既存、子プロセス/owner lock）
       ├─ settings / credential journal / SecretStore（既存）
       └─ typed account / model / probe adapter

GPUI AI settings view
  ├─ draft、入力部品、focus、view_epoch
  ├─ sanitized snapshotを購読
  └─ AiServiceHandleへコマンド送信
```

`hane_ai`をGPUI非依存のまま保つ。同期I/OをGPUI UI threadで実行しない。新しい非同期ランタイムや汎用actor frameworkは導入せず、既存std/thread/channel構成に小さなサービスを足す。

AiServiceは設定画面ではなくアプリが所有する。画面を閉じてもOAuth/接続確認は有限の期限内で継続する。再表示時に最新snapshotを取得する。画面の破棄でruntimeをdropしない。アプリ起動時にAIを必須初期化せず、AIの失敗でEditorを起動不能にしない。

長時間処理はworkerで実行し、サービスの受付ループをブロックしない。状態変更は受付側に集約する。busy中の操作を無制限にqueueへ積まず、受付時点で直ちに拒否する。取消は通常操作の後ろに詰まらない専用制御経路を使う。

### 4.1 外部への最小API

```rust
// 設計上の形。既存の型名と競合する場合は実装時に調整する。
fn snapshot(&self) -> AiSnapshot;
fn subscribe(&self) -> SnapshotSubscription;
fn try_submit(&self, command: AiCommand) -> Result<OperationId, AdmissionError>;
fn cancel(&self, target: OperationId) -> Result<(), AdmissionError>;
```

`AiCommand`はOpenSettings/RetryOwnership、Save、Recover、StartLogin、RefreshAccount、Logout、RefreshModels、Probe、Restart、Shutdownに限定する。呼び出し元から任意のRPC名/JSON/プロセス引数を受け取らない。Saveは`expected_revision`とdraft/SecretEditを持つ。戻り値のOperationIdは受付番号であり成功判定ではない。

通常のSnapshotに生RPC、API key、token、authUrl、秘密ストアの内容を含めない。保存完了・エラー表示はsnapshotの操作結果とstate_versionで確認できるようにし、UI通知1件の受信だけを成功証拠にしない。

## 5. 状態モデルと世代

巨大な単一のConnected/Disconnectedフラグを作らず、以下を分ける。

| 状態軸 | 最小表現 |
| --- | --- |
| 所有権 | Available / Owned / OwnedElsewhere / Unavailable |
| Runtime | 既存Stopped / Starting / Initializing / Ready / Stopping / Failed、同梱版、適用settings_generation |
| 永続化 | Clean / Saving / SavedCleanupPending / DurabilityUnconfirmed / RecoveryFailed |
| 適用 | NotRequired / Applying / Applied / Failed / NotConfigured |
| ChatGPT account | Unknown / SignedOut / SignedIn（APIが返した表示情報）、別に更新中/更新失敗 |
| ログイン操作 | Idle / Starting / AwaitingBrowser / Reconciling / Canceling / Finished |
| モデル一覧 | NotLoaded / Loading / Loaded / Failed、現在保存モデルが一覧で有効か |
| 接続確認 | NeverRun / Running / Canceling / Succeeded / Failed / Canceled / TimedOut / Stale |

API keyが保存されていることは「認証済み」ではない。Customの表示は「key登録済み/未登録」と「最終応答確認結果」に分ける。ChatGPTのaccount取得成功も、そのモデルで推論できる保証にはしない。

識別子は次の役割を混同しない。

- 永続`revision`: 保存の競合検出。現行仕様どおり保存時に増える。
- 永続`settings_generation`: runtimeに関わる変更。現行判定を維持し、非アクティブ接続のモデル等が変わる場合も勝手に除外しない。nameだけの変更はrevisionのみ。
- `runtime_generation`: 子プロセス1回の生存期間を識別する既存世代。
- `operation_id`: サービスが受け付けた論理操作。runtimeの内部operation_generationとは別。
- `auth_epoch`: ログイン/ログアウト/アカウント変更・資格情報失効の確定で増加するメモリ上の番号。
- `view_epoch`: 画面の寿命。閉じた画面のcallbackを無視するためだけに使用する。

処理結果は最低でも(runtime_generation, operation_id, settings_generation)で照合し、OAuthはloginId、接続確認はthreadId/turnIdも照合する。プロセス終了で過去の進行中操作を成功にしない。設定変更・runtime再起動・auth_epoch変更で古いテスト結果をStaleにする。前回アプリ起動時の成功を今の成功として表示しない。

## 6. 操作受付とロック

### 6.1 受付ルール

| 進行中処理 | 許可 | 即時拒否 |
| --- | --- | --- |
| Idle | 状態に適合する新規操作 | 非ownerのAI操作、未設定接続のProbe |
| Save / Recover / Reconfigure | snapshot閲覧、アプリ終了要求の記録 | 新規AI操作。保存transactionの途中取消はしない |
| Login（ブラウザ待ちを含む） | 対象取消、画面を閉じる、内部account整合確認 | 保存、別ログイン、Probe、Restart、Logout |
| Probe | 対象取消、画面を閉じる | 保存、Logout、Login、Restart、別Probe |
| Account/Model取得 | snapshot閲覧、画面を閉じる | 競合する新規操作 |
| 子停止未確認/通知欠落 | snapshot閲覧、停止確認・復旧 | 新規推論、成功扱い、所有権の別runtimeへの移譲 |

設定画面のボタン無効化だけに依存せず、AiServiceでも同じチェックを行う。二重クリックは最初の1件だけを受理し、残りはBusyとする。

### 6.2 OSロックの契約

既存Issueの **owner lock → settings lock** 順序を維持する。非ownerは待機・IPC・background pollingをせず即時失敗する。再表示または明示操作のときだけ再取得を試す。

保存/復旧はowner所有下でsettings排他ロックをnonblocking取得する。Probeはowner所有下でsettings共有ロックを取得し、最新設定・runtime Ready・適用世代を検査する。**共有ロックはterminalまたは子停止確認まで保持**する。turn/startのACK受信では解放しない。

世代不一致を検出したownerは共有ロックを解放し、owner lockとサービスの操作枠を保持したまま最新設定でreconfigureし、共有ロックを再取得して再検査する。再検査でも不一致ならGenerationChangedで終了し、無限に再試行しない。

OAuth中はサービスの操作枠とowner lockを保持するが、ブラウザ待ちの全時間settings排他ロックを保持しない。runtimeに関わらない他プロセスのname-only保存が許される既存経路にはrevision照合で対応する。owner取得後は常にdiskを読み直す。

**実装注意:** 既存`with_owner_lock`のcallbackはruntime coordinator側で実行される。そのcallback内から同じruntimeのstop/start/reconfigureを呼んで応答を待ってはいけない。callbackは保存/復旧とconfig生成結果の返却までとし、再構成はcallbackが終了した後に行う。その間もサービスの操作枠を解放しない。ownerを持つcoordinatorを一度dropして別のcoordinatorで起動し直す実装にしない。

停止確認に失敗した場合は、既存restart_blockedに加えてサービスを隔離状態とする。実行中の子が残り得るのに共有ロック/ownerを解放して次の処理を受理しない。Editorの利用は妨げない。

## 7. 保存・秘密更新・cleanup_pending

### 7.1 draftとAPI key

AI画面に入った時点のrevisionをdraftに保持する。秘密編集は明示的な`Keep / Replace(SecretInput) / Delete`とし、空文字を削除と解釈しない。既存keyを入力欄へ読み戻さず「登録済み」とだけ表示する。Replaceの空文字は検証エラー。

SecretInputは秘密専用の短命な入力経路に限定する。通常のViewModelやsnapshotにコピーせず、Debug/ログ/設定JSON/クラッシュ診断/画面の永続復元へ載せない。入力欄はmasked、コピー操作を禁止し、pasteは許可する。workerへ渡した後、成功・失敗・破棄・画面離脱時に欄を消去する。再入力が必要な失敗を画面で説明する。メモリ消去は使用する部品と型が保証する範囲に限定し、OSクリップボード等まで完全消去できるとは表示しない。

### 7.2 保存結果の型

```rust
enum SaveOutcome {
    NotCommitted(SafeError),
    Committed {
        settings: AiSettings,
        cleanup: CleanupState, // Clean | Pending(SafeError)
    },
    DurabilityUnconfirmed {
        observed: Option<AiSettings>,
        error: SafeError,
    },
}
```

`Applied/ApplyFailed/NotConfigured`はSaveOutcomeとは別軸で持つ。`Err`を一律「保存されませんでした」に変換しない。settings置換の前後を**connect/settingsのtransaction内部**で識別して結果を返す。外側で「エラーだから旧snapshotのまま」と推測しない。

### 7.3 手順と失敗時の表示

1. draftを検証する。接続方式、Custom name/base URL/model、秘密編集の整合性をチェックする。
2. 操作枠 → owner → settings排他を取得し、diskのrevisionとexpected_revisionを照合する。競合時は何も上書きせず最新値の再読込を促す。
3. pending journalがあれば保存を拒否してRecoverへ誘導する。通常保存からjournalを消さない。
4. 秘密変更は既存2-phase journalを使い、新key保存 → settings置換 → 旧key削除/journal完了の順序を維持する。revisionとcredential_refの再検査も維持する。
5. 置換後は保存済みsnapshotを採用する。後処理失敗は`Committed + Pending`。新しい設定を未保存扱いして旧keyに戻さない。
6. `PersistedDurabilityUnconfirmed`は第三の結果とし、読込可能な状態を表示するが耐久性を保証しない。旧keyを削除したり、曖昧なまま推論を始めたりしない。
7. cleanな保存完了後、必要な場合のみ再構成する。成功ならApplied。起動失敗でも保存は取り消さず「保存済み。ランタイムへの反映に失敗しました／再試行」を表示する。

cleanup/durabilityが未解決なら新規の保存とProbeを禁止する。旧runtimeによる新規処理も受け付けず、停止させる。復旧が済むまで旧接続へfallbackしない。フォームの再送でkey更新を重ねない。

**アクティブCustom keyの削除**は有効な保存操作である。結果は「保存済み・key未登録・NotConfigured」。旧keyを保持した子を停止し、自動的にChatGPTへ切り替えない。非アクティブ側のkey削除もその接続情報だけを変更する。

### 7.4 復旧

編集可能なAI画面を公開する前に、owner取得とpending検出を行う。pendingなら既存`recover_at_startup`と同じ手続きを実行し、完了後に**再読込**してsnapshotを作る。復旧中はread-only、失敗時はread-onlyと「復旧を再試行」を表示する。

復旧もowner → settings排他の順。実行中Probeを勝手に取消して進めない。journal記録と現在のcredential_refを照合した既存の冪等削除を再利用する。settings置換後にjournal段階更新が失敗したケースでも、diskに反映された新keyを誤削除しない。

durability未確認でjournalがない普通の保存は、同じロック下でsettingsを検証し、必要なfile/親ディレクトリのsyncを再確認する専用経路を用意する。確認のためだけにrevisionを増やさない。確認できないOS/状態ではエラーを維持し、安全性を下げて続行しない。

`cleanup_pending`はjournalから再構成できる状態として公開する。この表示だけのためにAiSettingsのschema_versionを増やしたり、秘密値をjournalへ追加したりしない。

## 8. runtime通知の信頼性

`account/login/completed`、`account/updated`、`item/completed`、`turn/completed`、プロセス終了・RPC異常を操作の制御情報として扱う。正常な通知量でこれらを破棄しない。Diagnosticsや表示用deltaとは分離する。

両方の既存転送境界に対して、bounded critical queueと**queue自身に依存しないoverflow flag/wake**を設ける。stdout readerをUI待ちでblockしない。critical queueが満杯なら黙って成功継続せずNotificationOverflowに遷移し、該当処理を失敗・停止確認へ進める。上限を無くすことで回避しない。API応答の経路は既存pending RPC解決と分離し、通知詰まりでRPC readerを止めない。

処理開始RPCを送る**前に購読**する。通知がRPC応答より先に届く場合に備え、現在の開始操作に限って未照合通知を一時保持する（最大64件/256KiB、超過は明示失敗）。応答でIDが判明したら照合し、他のIDは捨てる。別世代の通知はbufferへも入れない。値は実装定数としてテストする。

OAuth URLや生のaccount payloadをDebug表示する既存generic eventの経路を見直す。UIへはtypedでredactしたイベント/snapshotだけを渡す。OAuth/auth系に対するDebugは常にredactedとする。

## 9. ChatGPT/Codex OAuth

OAuth client、callback HTTP server、token交換、refresh token管理は**App Serverに委ねる**。`auth.json`の解析・コピー・削除は実装しない。通常の`~/.codex`や外部Codex CLIの資格情報を操作しない。

### 9.1 正常系

1. cleanな保存済みChatGPT接続、owner、未実行の操作枠を確認する。
2. ChatGPT専用CODEX_HOMEと安全プロファイルでruntimeを初期化する。account/readで現在状態を取得する。
3. ユーザーがログインを押したときだけ、購読済みの状態で`account/login/start {"type":"chatgpt"}`を送る。
4. 応答のloginIdを(runtime_generation, operation_id)へ紐付ける。authUrlは短命な秘密ラッパーでOSブラウザopenerへ直接渡し、通常snapshot/永続状態/ログへ入れない。
5. authUrlはHTTPS、userinfoなし、採用版のOAuthサーバーが返す公式認可hostをアプリのallowlistで検査する。文字列prefix比較は禁止。未検証hostを自動で開かない。allowlistの確定は同版login実装と実OAuthテストで行い、Custom Base URLから流用しない。
6. 該当loginIdの`account/login/completed`を受けたらaccount/readで整合確認する。success通知だけでSignedInにしない。
7. account/readでChatGPT accountが確認できたらSignedIn、auth_epoch更新、モデル一覧を取得する。モデルは自動保存しない。推論はしない。

通常読込はrefreshToken=false相当、明示的な「状態を更新」は同版schemaのrefreshToken=trueを利用する。通信失敗はSignedOutの証拠ではない。最後の既知accountと「状態を確認できません」を分けて表示し、古い状態だけで新しいProbeを許可しない。

### 9.2 競合・取消・期限

| 事象 | 処理 |
| --- | --- |
| ブラウザ起動失敗 | loginIdが分かればcancelし、account/readで確認。安全なエラーと再試行を表示。生authUrlのコピペ表示をfallbackにしない |
| 取消要求 | `account/login/cancel`へ該当loginIdを送る。その後account/readで確認 |
| cancel応答が`canceled` | account/readの状態を優先。取消がログアウトの代わりになったとは解釈しない |
| cancel応答が`notFound` | loginが既に完了した可能性がある。SignedOutを推定しない |
| 取消と成功が競合 | SignedInなら「ログインが完了しました」と実状態を表示し、必要ならユーザーが別操作でログアウトする。自動logoutしない |
| Start応答前にCompletedが到着 | §8のbounded bufferに保持してloginId判明後に照合 |
| 開始RPC timeoutでloginId不明 | 同じ開始RPCを自動再送しない。子停止で未特定flowを終了させ、再起動後account/readで確認する |
| 画面を閉じた | 操作を継続。破棄済みUIは更新せず、再表示にsnapshotを渡す |
| runtime再起動/終了 | 当該attemptを無効化。新runtimeのaccount/readで再判定 |
| 別loginId/旧世代の通知 | 現在attemptの成功・失敗を上書きしない |

ログインの待機上限は製品側定数10分。上流が先に期限切れを返せばその時点で終了する。期限到達時は取消・状態確認を行い、ブラウザ認証が残り得る不明状態を成功にしない。通常RPCの期限は既存20秒を基準とする。保存transactionにこのtimeoutを適用して途中でguardを捨てない。

account/updatedはaccount/readを促す契機にし、並行readを多重起動しない。logoutはaccount/logout後にaccount/readでSignedOutを確認する。logout失敗時にローカルtokenを手作業で削除して成功扱いしない。

## 10. モデル選択

ChatGPTはmodel/listをcursorが無くなるまで取得する。同版schemaのcursor/limit/includeHiddenを使用し、重複cursor、50ページ超、1000モデル超は明示エラーにする。途中取得を完全一覧と表示しない。hiddenモデルとtext非対応モデルは選択候補から除く。

**Model.idとModel.modelを混同しない。** 推論に渡して保存する値は同版Model.model。表示はdisplayNameとmodel文字列。isDefaultは候補表示に使うだけで、既存保存モデルを黙って変更しない。

保存済みモデルが一覧に無い場合は値を保持して「現在の一覧にありません」と表示する。先頭候補への自動置換・自動推論をしない。認証状態が確認済みで、現在の一覧で有効な保存モデルがあることをChatGPT Probeの条件とする。一覧取得失敗時も既存の保存設定は消さない。

Customは必須model IDを手入力する。汎用`/models`探索やChatGPT model/listでの検証は行わない。入力は前後空白を除いた非空の単一行で制御文字を拒否し、大文字小文字やprovider固有の区切り文字を勝手に変換しない。

plan名・利用制限・モデルの権限はAPIで得られる情報だけを表示する。契約プランから利用可能モデルをHane独自の固定表で推定しない。

## 11. 固定入力による応答確認

### 11.1 実行前条件

cleanな保存状態、適用世代一致のReady runtime、保存済みモデル、当該接続の認証条件、§12の検証済み安全プロファイルが必要。Customはkeyの取得に成功していること、ChatGPTは現在accountが確認済みであることを要求する。保存されたkeyの存在だけでCustom認証成功とは判断しない。

サービスが操作枠を取り、共有ロック下で設定を読み直す。thread/start→turn/start→通知待機の**全区間を1つの操作**にする。各RPCのたびに共有ロックを取得し直す実装は禁止する。

### 11.2 RPCシーケンス

```text
購読を確立、settings共有lock、世代照合
thread/start
  model: 保存モデル
  modelProvider: ChatGPTなら同梱版のOpenAI provider、Customならhane_custom
  cwd: Hane所有の空workspace
  ephemeral: true
  sandbox: read-only
  approvalPolicy: never
  baseInstructions/developerInstructions: 接続確認専用（文書を含めない）
turn/start
  threadId: 上記のthread
  input: [{"type": "text", "text": "Reply with exactly HANE_AI_OK."}]
item/completed（当該thread/turnのagentMessageを収集）
turn/completed（status/errorを検査）
結果確定、共有lock解放
```

上記は§12の安全プロファイルとセットで使う。**read-only、approvalPolicy=never、ephemeralだけではツールを無効化した証拠にならない。** 未知の`tool_choice`/`allowedTools`/空dynamicToolsを付けて解決したことにしない。

成功は、当該turnがcompleted、errorなし、空でないfinal agentMessage、禁止されたツール動作なし、世代一致をすべて満たすこと。turn/start ACKや最初のdeltaだけでは成功にしない。モデルが文言どおり返さなかっただけで疎通を失敗にしないため、HANE_AI_OKとの完全一致は必須にしない。

0.157.1のturn/completedだけで最終本文が取得できると仮定せず、item/completedをitem IDで重複排除して収集する。本文表示は最大8KiBのplain textにし、HTML/Markdownリンク等を自動実行しない。先着通知は§8のルールで扱う。

### 11.3 取消と結果

Probe全体の期限は90秒。ユーザー取消/期限切れ時、turnIdが既知ならturn/interruptを送り、最大5秒terminalを待つ。それでも確定しなければ既存の停止・強制停止手順へ進む。turnId不明なら子停止へ進む。**interrupt ACKでは共有ロックを解放しない。** 停止未確認なら隔離状態とする。

自動再試行はしない。特に401/403、429、ネットワークtimeoutを理由に別接続で再実行しない。認証不正と通信障害を分類して表示し、保存keyを自動削除しない。

直近結果はメモリ内で接続ごとに保持してよい。ただし(settings_generation, runtime_generation, auth_epoch, model, probe_profile_version)を紐付け、変化後はStale/未確認にする。response本文のログ記録・テスト履歴の永続化は本Issueでは追加しない。

## 12. 安全プロファイルと最初の契約検証

### 12.1 必須の実行境界

ChatGPT/Customで専用CODEX_HOMEを分離する。一般ユーザーのCodex設定や資格情報をコピーしない。実ユーザーのHOME/USERPROFILEを偽の空HOMEへ置き換えない。子プロセスのcwdとthread cwdはHane所有の空workspaceとし、Editorのworkspaceを渡さない。

RuntimeConfigにcwdと環境変数継承ポリシーを追加する。親のOPENAI_API_KEY/OPENAI_BASE_URL等のprovider・auth・Codex設定overrideを無条件継承しない。ChatGPTにはCustom keyを渡さず、CustomにはHANE_AI_PROVIDER_KEYだけを専用経路で渡す。PATH、証明書/proxyなど必要なOS環境は用途を明示して扱い、親環境全削除でネットワークや秘密ストアを壊す変更にしない。test fixtureに汚染環境を与え、選択接続が変わらないことを検証する。

Hane管理の生成configは同梱版**config schema**で検証する。RPC protocol schemaとは別物である。起動はstandaloneで`--strict-config --listen stdio://`を使う。0.157.1のstandalone mainがstrict-configを受けることはソース確認済み。full CLIの`app-server`サブコマンドは付けない。

Base URLは現行のResponses/Bearer制約を維持する。URLパーサーでscheme/host/userinfo/query/fragmentを検証し、userinfo、秘密を含み得るquery/fragment、制御文字を拒否する。HTTPS必須、HTTPは既存で許可するloopbackのみ。勝手に`/v1`を足さず、pathの意味を変えない。Custom name/modelにも制御文字を許可しない。HTTP loopbackにはローカル非暗号化接続の注意を表示する。

### 12.2 T00: 推論経路に先行する検証ゲート

現行コードには「全ツール無効」を保証する製品用profileがなく、本書作成時点で実機確認もしていない。上流0.157.1ではshellとutility/apply_patch等の登録条件が別であり、shell無効だけを根拠に全ツール無効とは断言できない。

最初の実装工程で`runtime_profile.rs`とstandalone実機fixtureを作り、以下を行う。

1. 同版config schemaとtool登録処理から、shell、apply_patch、画像参照/生成、web search、multi-agent、MCP、Skills/Plugins、hooks、外部拡張の露出・自動実行を抑制する**実際に有効な設定**を特定し、生成config fixtureとして固定する。未知の設定名で埋めない。
2. strict-configで起動し、mock Responsesサーバーが受けるrequestのtoolsと入力を検査する。固定Probeにツールを提供せず、Haneが与えていない文書/AGENTS/skill本文が入力へ混入していないことを確認する。
3. 隣接workspaceやユーザー側Codex領域にsentinelファイル/設定を置き、それらを読んだり実行したりしないことを検証する。読み込み検証は本文に出なかったことだけで済ませず、利用可能なファイルアクセス記録や上流config loaderの契約テストと組み合わせる。
4. providerが予期せぬtool callを返すnegative fixtureを用意し、shell/ファイル変更/別ネットワーク接続が実行されないことを確認する。通知を見てから停止するだけでは実行防止の証拠にならない。
5. 設定で保証できない場合は**互換性の阻害要因として報告**し、上流の制限API利用または最小の専用runtime対応を別判断にする。Hane側の通常ログイン/設定実装は進められるが、Probeを安全性未確認のまま公開して#379を完了にしてはいけない。上流fork、version変更、セキュリティ要件緩和をworker判断だけで混ぜない。

これは「あとで何となく検証」ではなく、**P0の終了条件**である。成功したprofileはバージョン付きfixtureと証拠を保存し、以後の実装は同じprofileを使う。実OAuthでは同じprofileでログインから固定応答まで検証する。mock Customの成功をChatGPT経路の実証と取り違えない。

## 13. 画面構成と入力/focus

既存設定の「←アプリに戻る」、全画面占有、スクロールしないナビゲーションを維持する。別windowやEditor横のAI panelにはしない。現行gpui-kit/既存アダプターで利用できる部品を優先し、このIssueで全UI移行・GPUI更新を必須化しない。

AIページは上から次の構成とする。

| 区画 | 内容 |
| --- | --- |
| Runtime | 同梱版、Stopped/Ready/Failed、適用状態、再起動、非owner説明/再試行 |
| 使用する接続 | ChatGPT/Codex / Custom選択。保存済み接続とdraftが異なるときは明示 |
| ChatGPT | account状態、APIが返すアカウント表示、login/cancel/refresh/logout、モデル選択/更新 |
| Custom | name、Base URL、model ID、key登録状態、変更/削除。秘密値のreadbackなし |
| 接続・応答確認 | 課金/利用枠の注意、実行/取消、最終結果・時刻・Stale表示、短いplain text応答 |
| 保存操作 | 保存/変更を破棄、未保存表示。保存済みと適用失敗/cleanup_pendingを別メッセージで表示 |

フォームはロード中も突然消さず、操作可否と状態表示を更新する。snapshot更新のたびにInput entityを作り直さず、IME compositionとカーソルを維持する。タブ順は表示順、masked keyにaccessible label、各エラーは対応項目へ紐付ける。

AI変更がdirtyのまま戻る/別設定カテゴリへ移動する場合は「保存して移動／破棄して移動／編集を続ける」を確認する。保存して移動はcleanなcommitを確認してから実行し、cleanup/durabilityの問題がある場合は復旧表示に留める。画面離脱でOAuth/Probe自体は取り消さない。進行中であることは再表示時に確認できる。

### 13.1 Editorを壊さない規則

設定中はsettings専用focus handle/key contextでイベントを受け、Editorのkeydown、text input、mouse、wheel、actionへ落とさない。Ctrl/Cmd+SはAI保存にscopeするか無効化し、背後文書を保存しない。

既存の「EditorがIME変換中は設定を開かない」を維持する。設定入力側のEscapeはまずその入力部品のcompositionを処理し、同じキーで画面を閉じない。現行action guardのCancelComposition例外が背後Editorへ到達しないようにする。

戻る時はEditorへのfocusを復帰するが、画面を開いた時のDocumentSessionを丸ごと書き戻さない。DocumentSession/dirty/undo/selection/scrollの保存・復元をAIサービスにさせない。Windowsの外部ファイルopen等の正当なイベントがあった場合も、古いsnapshotでその変更を消さない。外部open経路の無条件`window.focus(editor.focus_handle)`を設定表示中には行わない。

非同期UI更新にはweak entityとview_epochを使い、閉じた画面への更新を破棄する。サービス側の操作結果は破棄しない。

## 14. エラーと診断

UIへ渡すSafeErrorは分類、安定したエラーコード、ユーザー向け文言、再試行可能性だけにする。分類はBusy、OwnedElsewhere、Validation、RevisionConflict、RecoveryRequired、CredentialUnavailable、RuntimeUnavailable、LoginFailed、AccountUnknown、ModelUnavailable、Unauthorized、RateLimited、Network、ProtocolMismatch、NotificationOverflow、TimedOut、SafetyProfileUnsupportedを基本とする。

provider応答本文、RPC全文、stderr、Authorization、token、authUrl、keyは通常ログ/画面へ流さない。外部エラーをそのままformatしてSafeErrorに詰めない。HTTP status等の非秘密情報だけをallowlistで取り出す。loginId/threadId/turnId等も必要最小限の診断に留め、アカウントemailを通常ログへ残さない。

認証失敗時は再ログイン/key変更へ誘導し、通信失敗時は設定を保持して再試行へ誘導する。429は別接続へfallbackする理由にしない。evidenceには実key/token/URL/emailを記録せず、redactionの対象と検証した範囲を明記する。

## 15. 変更ファイルと実装順序

### 15.1 ファイルの責務

| 場所 | 変更 |
| --- | --- |
| `crates/ai/src/service.rs`（新規） | アプリ所有サービス、受付、snapshot、世代、操作完了、busy/取消 |
| `crates/ai/src/account.rs`（新規） | 同版account型、OAuth state machine、ID照合、reconcile |
| `crates/ai/src/models.rs`（新規） | model/list paging、候補投影、保存モデル検証 |
| `crates/ai/src/probe.rs`（新規） | 全ライフサイクルの共有lock、thread/turn照合、結果集約、取消 |
| `crates/ai/src/runtime_profile.rs`（新規） | 両接続の起動config/cwd/env、安全プロファイル、同版対応 |
| `connect.rs` / `settings.rs` / journal関連 | post-commit結果、durability、復旧結果の公開。既存原子性を維持 |
| `runtime.rs` / 必要な`rpc.rs` | critical通知の欠落検知、cwd/env、redaction。owner保持と既存stop/restart契約を維持 |
| `crates/ai/src/lib.rs` | 最小の公開service/snapshot型をexport |
| `crates/ui/src/ai_settings.rs`（新規、または同等のview配下） | 入力entity、draft、表示、settings専用focus。生RPCを扱わない |
| `crates/ui/src/view.rs`と既存settings/action glue | AIページ挿入とfocus guardのみ。viewport等の再移動はしない |
| `crates/app/src/main.rs` | サービス生成/共有、browser opener、終了処理、外部open focus調整 |
| `crates/ai/tests/` | fake server/実standalone、journal/owner/generation、OAuth/Probeテスト |
| 既存UI test/GUI検証経路 | IME、focus、dirty/undo/selection/scrollの退行 |

名前は現在のモジュール構造に合わせた小変更を許すが、サービスをEditorViewへ埋め込んだり生RPCをUIへ公開したりする変更は不可。追加依存は既存のserde/URL/secret型を再利用できるか確認し、今回のためだけの大きなframeworkを入れない。

### 15.2 順序と各工程の終了条件

| 工程 | 内容 | 次へ進む条件 |
| --- | --- | --- |
| P0 | **T00安全プロファイル/standalone契約確認**とAPI fixture確定 | 対象版の生成configと起動引数、禁止動作テストの結果がある。保証不能なら阻害要因を限定して報告 |
| P1 | 保存戻り値/復旧、critical通知、cwd/env基盤 | T01〜T08がpass。post-commit失敗を未保存扱いしない |
| P2 | AiService、OAuth、account、モデル | T09〜T17がpass。取消・画面寿命・世代の競合をfakeで再現可能 |
| P3 | Probeの共有lockとterminal/停止確認 | T18〜T23がpass。保存とProbeが線形化され、禁止動作なし |
| P4 | 全画面AI UIとapp接続 | T24〜T28、必要なGUI証拠。Editor退行なし |
| P5 | 実OAuth/Customとcurrent exact head検証 | T29〜T31を満たす。Issue完了判定 |

P0と推論を含まないP1/P2の実装は独立に進められるが、P3の実接続機能の公開はP0完了後。小さなPRへの分割を推奨するが、大量の新Issueや並列開発基盤は追加しない。途中PRで#379を自動closeしない。

## 16. 受入テスト一覧

| ID | ケース | 合格条件 |
| --- | --- | --- |
| T00 | 同版standaloneと安全プロファイル | §12の5段階を満たす。full CLI/単なるread-onlyの証拠で代用しない |
| T01 | 通常保存/name-only保存 | 保存と推論が分離。name-onlyはrevisionのみ。モデル変更は世代増加 |
| T02 | settings置換前の失敗 | NotCommitted。保存済み設定と有効keyを維持 |
| T03 | settings置換後のjournal段階更新失敗 | 保存後の状態として表示。新keyを誤削除しない |
| T04 | 旧key削除/journal完了失敗 | cleanup_pending表示、再保存/Probe拒否、再起動/Retryで冪等復旧 |
| T05 | durability未確認 | 未保存/完全成功のいずれにも誤分類せず、新規推論を禁止 |
| T06 | revision競合/credential_ref競合 | 古いdraftで上書き・key変更をしない |
| T07 | active key削除/再構成失敗 | 保存済みを維持、旧子停止、NotConfigured/ApplyFailed。fallbackなし |
| T08 | 通知overflow（2境界それぞれ） | terminal欠落を検知し明示失敗。無期限待機・偽成功なし |
| T09 | 新規login成功 | start→browser→completed→read→model list。read確認前にSignedInにしない |
| T10 | cancel成功/成功との競合/notFound | account/readを優先。cancelをlogoutと誤解しない |
| T11 | CompletedがStart ACKに先行 | boundedに照合し、正しいattemptだけ完了 |
| T12 | browser失敗/Start timeout/10分期限 | 明示的な取消・reconcile/停止。勝手なlogin再送なし |
| T13 | 画面close→login完了→再表示 | サービスは完了、破棄UI更新なし、再表示に最新状態 |
| T14 | runtime再起動後の古いlogin通知 | 新しいaccount/attemptを上書きしない |
| T15 | logout/アプリ再起動account/read | App Server APIだけ使用。一般Codex資格情報・Custom key変更なし |
| T16 | model paging/id≠model/hidden/text非対応 | 保存値と推論値が正しい。cursorループ上限あり |
| T17 | 保存モデルが一覧から消失/一覧失敗 | 保存値保持、未確認表示、自動置換なし |
| T18 | Probe正常完了/ACKだけ/最終error | terminal成功と最終textの両方が必要。ACKだけで成功にしない |
| T19 | item/terminalの重複・先着・他thread/turn | ID/世代で照合し誤った本文・成功が混ざらない |
| T20 | Probe中保存/Logout/Restart | 即時Busy、Probeを暗黙取消しない。共有lockはterminalまで保持 |
| T21 | Probe取消/timeout/停止失敗 | ACKでguardを解放しない。停止未確認は隔離 |
| T22 | owner競合/owner終了/世代不一致 | 非owner即時失敗、明示再操作で最新設定によるtakeover |
| T23 | 401/403/429/timeout/不正RPC | 安全な分類、secret露出なし、別providerへのfallbackなし |
| T24 | AI画面各操作/dirty離脱 | draftと保存済みを区別。Saveで推論しない。秘密欄を再表示しない |
| T25 | 日本語IME/Escape/Tab/Cmd+S | 入力先だけへ作用し背後Editorへ漏れない |
| T26 | settings中外部open/非同期更新/戻る | focus漏れ・古いDocumentSessionへの巻戻しなし |
| T27 | dirty/undo/selection/scroll比較 | AI操作前後で無関係なEditor状態が不変 |
| T28 | 二重click/非owner/cleanup復旧画面 | UIとservice双方で操作可否が一致し、再試行が実操作に繋がる |
| T29 | 実ChatGPT OAuth→固定応答→logout→再login | 対象版・同じ安全profileで証拠取得。認証操作は許可された実アカウントで実施 |
| T30 | 実standalone→mock Responses→Custom切替往復 | POST path/Bearer/model/入力/terminalを確認。両設定が保持される |
| T31 | current exact headのCI/review/GUI | 対象SHA、コマンド、exit code、実施/skip範囲を記録。未解決の有効review指摘なし |

### 16.1 テストの実行と証拠

新しいstate machineと通知競合はfake App Serverと制御可能な時計で検証する。実時間10分待つunit testにしない。保存/journalの各段階をfault injectionできるようにし、プロセス間lockは別プロセスで検証する。

基本コマンドは`cargo fmt --all -- --check`、`cargo test -p hane_ai`を基点に、対象headのCargo package名と既存CI定義を確認して実行する。UIは既存workspaceのCI・test設定に従い、Linuxでnative GUI検証ができないことをmacOS/Windowsでの合格と取り違えない。

実standaloneテストはbinary未指定時のskipを可視化する。既存の`REQUIRED_REAL_APP_SERVER_EVIDENCE_NOT_OBTAINED`を隠さず、skipをT00/T29/T30の合格としない。fixtureはStandaloneとFullCliを明示型で分け、製品受入はStandaloneのみで判定する。

GUI証拠はmacOS/Windowsの該当プラットフォーム、対象SHA、実行ファイルの版、入力手順、期待/実結果を記録する。実OAuthのURL/key/token/emailをスクリーンショットやログへ残さない。テスト後のkeyや専用CODEX_HOMEはテスト所有領域だけを片付け、利用者の通常設定を消さない。

## 17. 引継ぎ時の最初の確認

実装者はmainのcurrent exact head、#379本文/コメント、#377/#378の変更、同梱版、実行形式、本書との差分を再確認する。本書作成時のSHAを現行値として使い続けない。最新コードに合わせて実装位置を調整しても、保存後失敗の区別、所有権/共有lock、OAuthのauthority、secret境界、Editor不変条件を弱めない。

設計PRのmergeだけで#379をcloseしない。P0の安全性が未証明のまま「UIができた」「fake testが通った」を完成理由にしない。完了はT29〜T31を含めた実装証拠で判定する。

## 18. 上流契約の参照

すべて`openai/codex`のtag `rust-v0.157.1`を基準に確認した。Hane内の保持schemaとの整合をfixtureで検査する。

- [LoginAccountParams](https://github.com/openai/codex/blob/rust-v0.157.1/codex-rs/app-server-protocol/schema/typescript/v2/LoginAccountParams.ts): managed ChatGPT loginの入力。
- [CancelLoginAccountStatus](https://github.com/openai/codex/blob/rust-v0.157.1/codex-rs/app-server-protocol/schema/typescript/v2/CancelLoginAccountStatus.ts): canceled / notFound。
- [ThreadStartParams](https://github.com/openai/codex/blob/rust-v0.157.1/codex-rs/app-server-protocol/schema/typescript/v2/ThreadStartParams.ts)、[TurnStartParams](https://github.com/openai/codex/blob/rust-v0.157.1/codex-rs/app-server-protocol/schema/typescript/v2/TurnStartParams.ts): thread/turn境界。
- [Model](https://github.com/openai/codex/blob/rust-v0.157.1/codex-rs/app-server-protocol/schema/typescript/v2/Model.ts): idとmodel、表示名、hidden、inputModalities。
- [standalone main](https://github.com/openai/codex/blob/rust-v0.157.1/codex-rs/app-server/src/main.rs): listen/strict-config等の起動引数。
- [tool登録処理](https://github.com/openai/codex/blob/rust-v0.157.1/codex-rs/core/src/tools/spec_plan.rs): shellと他のツールの登録条件が別であること。configでの完全無効化を実証する必要性。

本書の追加ではRust変更・ビルド・実OAuth・GUI操作を実施していない。ここに記載したテストは実装時の合格条件であり、実施済み結果ではない。

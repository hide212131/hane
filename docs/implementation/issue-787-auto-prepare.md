# Issue #787 設計書 — 選択 AI を開いたときの自動接続準備

**推奨案は (a) ポップアップを開いた時点での発火です。** 有効な選択範囲からポップアップを開き、接続が未準備なら、既存の `PrepareConnection` を一度送ります。利用者が指示を書いている間に準備を進め、成功後は同じポップアップで送信できるようにします。接続準備に使うのは既存の固定プローブです。選択範囲や入力した指示の送信には、引き続き利用者の送信操作が必要です。

調査対象は v0.56.1 の `20e26b8f` です。ローカルの `HEAD` と `origin/main` が同じ SHA であることを確認しました。既存の #724 の接続準備と #449 の選択変換を使います。コード変更・テスト実行はしていません。（設計セッションの読み取り専用サンドボックスからは Jev へ到達できなかったため、Jev の判断は経ていません。設計は Issue 本文の方針と受入条件に沿っており、意味判断の分岐は残っていないため、司令塔がこのまま採用します。）[Issue #787](https://github.com/hide212131/hane/issues/787)

## 1. 発火点と処理の流れ

キーボードの `Alt-I` は `OpenAiSelection` → `open_ai_selection_action` を通り、フッターの「AIに指示」と省スペース時のメニューも `EditorView::open_ai_selection` に集約されます。このメソッドで選択範囲と文書の世代を取得し、`AiSelectionState` を `Composing` にして `open = true` にします。以降、`render.rs` が `ai_selection_overlay` 経由でポップアップを描画します。準備の送信は `open_ai_selection` の正常終了経路、最後の `cx.notify()` の前に置きます。空白だけの選択、IME 変換中、すでに開いている場合など、既存の早期リターンでは発火しません。[actions.rs](crates/ui/src/actions.rs#L247) · [footer.rs](crates/ui/src/view/footer.rs#L358) · [ai_selection.rs](crates/ui/src/view/ai_selection.rs#L994) · [render.rs](crates/ui/src/view/render.rs#L670)

| 開いた時点の状態 | 動作 |
|---|---|
| 送信可能 | 既存の接続先とモデルを表示し、そのまま送信可能にする |
| 未準備でサービスが空いている | `PrepareConnection` を一度送る。起動直後の `OwnershipState::Unknown` も対象にする |
| 接続準備が進行中 | 既存の操作を購読し、「接続を確認しています…」と表示する |
| 別の処理が進行中 | 「別のAI処理が進行中です」と表示し、完了後に送信可否を更新する。まだ準備が必要ならポップアップの開き直しを案内する |
| サービスが利用できない、または受付に失敗した | 安全な短い理由を表示する。設定で解決できる場合は「設定 → AI接続」へ案内する |

起動直後の `AiSnapshot` は既定値なので、ポップアップ側で保存済み接続の有無や種類を先に決めません。既存の worker が所有権を取得して保存済み設定を読み込み、**ChatGPT** ではアカウント確認、モデル一覧取得、必要なら初回モデルの保存、固定プローブへ進みます。**Custom** では現行の `Step::Skip` に従い、所有権の取得と設定の読み込みまでで完了します。設定が揃っていればそのまま送信できます。Custom の自動プローブはこの設計の範囲外です。[service.rs](crates/ai/src/service.rs#L956) · [connection_preparation.rs](crates/ai/src/connection_preparation.rs#L332)

## 2. 準備中の表示と二重送信防止

`ServiceBusyReason` は処理の**段階**を表します。`PrepareConnection` の受付と設定読み込みでは `Recovering`、アカウント・モデルの取得では `Preparing`、モデル保存では `Saving`、プローブでは `Probe` になります。`PreparationStatus::Running(_)` は準備の各段階を識別できますが、受付直後の `Recovering` はカバーしません。また `Recovering`・`Saving`・`Probe` は単独の操作でも使われます。[service.rs](crates/ai/src/service.rs#L375) · [service.rs](crates/ai/src/service.rs#L1660)

そこで `service.rs` の既存の排他枠に、**接続準備として確保した `OperationId`** を持たせ、スナップショットに公開します。受付・段階移行・終了を通じて同じ ID を使い、排他枠と同時に設定・解除します。UI はマーカーと `busy` の ID が一致するときに「接続を確認しています…」を出します。単独のプローブや変換は別処理として扱います。準備中は送信ボタンを無効にし、Enter・保存済みプロンプト・再生成も `start_request` の最新スナップショットによる判定で止めます。ポップアップを閉じてもサービス所有の準備は継続します。

`Busy` をエラー表示せず購読結果を待つ点は `AiSettingsPage::activate()` と揃えます。ただし現行の `enqueue` は、別処理で使用中に `PrepareConnection` を受けると `deferred_open_settings` を立てる場合があります。**ポップアップ用の受付は同じコマンドと排他枠を使い、別処理に競り負けた場合の遅延登録だけを行わない**ようにします。設定画面の既存の遅延受付は維持します。同じ準備への重複要求は、受付直後を含めて排他ロック内の操作識別で合流させ、追加コマンドも遅延フラグも残しません。準備終了後の別の開く操作は新しい起点として扱い、既存の成功プローブキャッシュと同一条件での失敗後の再プローブ抑止を使います。[service.rs](crates/ai/src/service.rs#L517) · [service.rs](crates/ai/src/service.rs#L890) · [connection_preparation.rs](crates/ai/src/connection_preparation.rs#L375)

## 3. 完了通知と失敗時の案内

**同じサービスハンドルを使っている間は、現在の購読経路で、ポップアップを開いたまま送信可能に切り替わります。** `EditorView::attach_ai_service` がハンドルを両画面に渡し、`AiSettingsPage::attach` が設定画面の表示有無にかかわらず購読します。サービスが結果と `busy = None` を公開すると、購読タスクが `EditorView` に `cx.notify()` を出し、`ai_selection_compose_element` が最新の `service.snapshot()` からボタンと状態行を再評価します。通知がまとめられても最新値を読むため、途中の通知をすべて受け取る必要はありません。一方、サービス再接続時には `subscription_started` により購読先が旧ハンドルのまま残る経路があります。既存の購読タスクを差し替え可能にし、`attach` のたびに現在のハンドルへ結び直します。[view.rs](crates/ui/src/view.rs#L3731) · [ai_settings.rs](crates/ui/src/view/ai_settings.rs#L307) · [app_update.rs](crates/ui/src/view/app_update.rs#L2185) · [service.rs](crates/ai/src/service.rs#L4152)

`text_transform_status_line` を状態別の表示に変更し、原因が分かる場合だけ具体的な対処を添えます。案内は状態行に置き、設定画面への自動遷移は行いません。

| 経路・根拠 | 表示例 |
|---|---|
| ChatGPT の `NeedsLogin`、または確認済みの `SignedOut` | 「ChatGPTにログインしていません。設定 → AI接続でログインしてください。」 |
| ChatGPT の `AccountReadFailed` / `account_refresh_failed` | 「ログイン状態を取得できませんでした。設定 → AI接続で再試行してください。」`Unknown` や取得失敗からログアウトを推定しない |
| ChatGPT のモデル一覧取得失敗・候補なし・保存済みモデルなし・保存失敗 | `PreparationFailure` ごとに理由を表示し、AI接続での再試行またはモデル選択を案内する |
| 今回のプローブが認証拒否・通信失敗・タイムアウト | `ProbeStatus` の分類を使い、認証、通信、応答待ちのどこで止まったかを示す。認証拒否から資格情報の誤りを断定しない |
| Custom の設定不足 | 保存済みの名前・URL・モデル ID・認証情報の参照を確認し、「設定 → AI接続で接続先とAPIキーの登録を確認してください」と表示する |
| Custom のキー取得・認証・通信の失敗 | 開いた時点では参照の有無まで判定できる。実際のキー取得や通信の失敗は既存の変換結果で表示し、原因に応じて AI接続へ案内する |
| 所有権・復旧・隔離・ランタイムの問題、または原因不明 | `ownership`、`recovery_required`、`runtime_isolated`、ランタイムの分類済み状態から具体化する。特定できなければ「接続を確認できませんでした。設定 → AI接続で状態を確認してください。」 |

`probe_diagnostic` は直近の失敗を保持するため、今回の準備と `operation_id` の対応を確認できる場合だけ、`probe_status` の具体的な理由を採用します。失敗後の再表示でプローブが抑止された場合など、対応が確認できなければ `PreparationFailure::ProbeFailed` の案内を使います。プロバイダーの生エラーや認証情報は表示しません。

**送信可否は既存の変換条件を基本にします。** `runtime_isolated` はサービス側の拒否と UI を揃えて送信不可とします。ChatGPT のプローブ成功を変換全体の新しい必須条件にはしません。たとえばプローブがタイムアウトしても、アカウント・モデルなど既存の変換条件が揃っていれば、失敗理由を表示したうえで利用者の明示的な送信は可能です。Custom も設定完備で送信可能とし、疎通確認済みとは表示しません。[ai_selection.rs](crates/ui/src/view/ai_selection.rs#L436) · [ai_selection.rs](crates/ui/src/view/ai_selection.rs#L486) · [probe.rs](crates/ai/src/probe.rs#L97)

## 4. コメントと採らない案

`ai_settings.rs` の `activate()` は従来どおり `PrepareConnection` を送ります。変更する不変条件は、**自動準備を開始できる UI の明示操作に、選択 AI を開く操作を加えること**です。描画・スナップショット受信・購読開始で外部通信を発火させない契約は維持します。`connection_preparation.rs` の `OpenSettings` に関する説明と、既存の接続準備仕様 §3.1 にも機能側の起点を追記します。[ai_settings.rs](crates/ui/src/view/ai_settings.rs#L406) · [接続準備仕様 §3.1](docs/ai-settings-oauth-implementation-spec.md#L59)

```rust
// Explicitly opening AI settings or the selection AI popup can start
// automatic connection preparation. Rendering and snapshot updates
// never submit it.
```

| 採らない案 | 理由 |
|---|---|
| (b) `start_request` のみ | 無効な送信ボタンからは到達できず、操作経路によって準備の始まり方が変わる。指示を書いている間も準備が進まない |
| (c) 開く時と `start_request` の両方 | 一度の利用で起点が二つになり、準備中の Enter や失敗直後の送信操作に再試行の意味まで持たせることになる |
| 保存済み接続がある場合に起動時から準備 | 初回の待ち時間を隠せる一方、AI を使わない起動にもアカウント確認・モデル一覧取得・プローブの通信が発生し得る。ポップアップを開いてから指示を入力する間に進めれば、設定画面への往復を解消できる |

## 5. テスト計画

| ファイル | テスト名案と検証内容 |
|---|---|
| `crates/ui/src/view/ai_selection.rs` | `opening_unprepared_selection_submits_prepare_once` — サービスを正式に attach し、キーボード入口から開く。準備コマンドが一件で、再描画・通知・開いた状態での再操作では増えない。準備済みと無効な選択ではゼロ |
| 同上 | `preparing_selection_shows_progress_and_blocks_all_send_paths` — 受付直後、モデル保存、プローブの各段階で「接続を確認しています…」を表示。クリック・Enter・保存済みプロンプト・再生成で、追加の準備も変換もゼロ。単独の `TextTransform` や `Probe` は別処理表示 |
| 同上 | `preparation_completion_enables_send_without_reopening` — 開いたまま購読経由で成功を受け、画面を開き直さず実際の送信ボタンをクリックして変換が一件発行される。成功通知だけでは変換ゼロ。ハンドル差し替え後にも同じ検証を行う |
| 同上 | `preparation_failure_shows_reason_and_settings_guidance` — 未ログイン、アカウント取得失敗、今回の認証拒否・通信失敗、古い診断、Custom 設定不足を確認する。別操作の診断を今回の原因に使わない |
| `crates/ai/src/service.rs`、`crates/ai/tests/account_service.rs` | `overlapping_settings_and_selection_share_one_preparation` — 受付直後、各段階、終了直前で両入口の順序を入れ替えても追加受付・遅延実行がなく、固定プローブは一回。別処理中にポップアップが競り負けても遅延登録されず、設定画面の従来の遅延受付は動く |
| `crates/ai/tests/account_service.rs`、`crates/ui/src/view/ai_settings.rs` | `saved_connection_is_loaded_on_first_ai_use`、`opening_ai_settings_still_prepares_connection` — 起動だけでは AI RPC が発生せず、初回利用で保存済み ChatGPT が準備完了する。保存済み Custom は読み込み後に送信可能でプローブはゼロ。AI 設定の表示は従来の準備を開始する |

GPUI 側は既存の `open_popup_for_keyboard_tests` とクリック・キーボード入力のテストを土台にします。完了通知を検証するケースでは `attach_ai_service` を通し、受付キューを観測しながらサービスと同じ通知経路にスナップショットを公開するテスト用ハンドルを使います。テスト側から `cx.notify()` を直接呼ぶと、購読漏れを検出できません。サービス側は既存の `fake_app_server`、RPC カウント、アカウント読み取りの保留を再利用し、実ネットワークなしで検証します。[ai_selection.rs](crates/ui/src/view/ai_selection.rs#L3478) · [ai_settings.rs](crates/ui/src/view/ai_settings.rs#L4169) · [account_service.rs](crates/ai/tests/account_service.rs#L900)
---

## 司令塔の追記（採用と進め方）

- この設計を採用します。設計は Codex（GPT-6.2 Sol / medium）が v0.56.1（20e26b8f）のコードを読んで作成しました。設計セッションの読み取り専用サンドボックスからは Jev に到達できなかったため Jev 判断は経ていませんが、設計は Issue 本文の承認済み方針（機能側からの自動準備）と受入条件に沿っており、意味判断の分岐は残っていないため、司令塔がこのまま採用します。
- 実装は2フェーズに分けます。P1 はサービス側（`crates/ai`）: 接続準備として確保した OperationId を排他枠に保持してスナップショットへ公開すること、ポップアップ起点の受付が別処理に競り負けても遅延登録（deferred_open_settings）を残さないこと、重複要求の合流。P2 は UI 側（`crates/ui`）: `open_ai_selection` の正常終了経路での発火、状態行の状態別表示と失敗理由の案内、購読の結び直し、`ai_settings.rs` のコメントと接続準備仕様 §3.1 の追記。
- 起動時の自動準備は入れません（設計 §4）。設定画面の従来動作は変えません。
- この文書は準備コミット時点のもので、実装・検証はまだ行われていません。

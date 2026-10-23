# Issue #553: 更新・AI接続・検証失敗の診断テキスト — 実装メモ

対象: https://github.com/hide212131/hane/issues/553

要求と受入条件の正本はIssue本文とする。このメモは実装の所在と検証方法を記録する。

## 発端（履歴）

macOSのApp Translocation下で更新に失敗した利用者が、失敗段階と具体的原因を共有できなかったことが発端である。配置エラーそのものの修正は#510で扱う。この事例の実障害再現や新リリース待ちは、現行の受入条件ではない。

## 現行の範囲

更新・AI接続（AI probe）・代表的な設定検証の失敗について、本人が設定画面から診断テキストを取得して共有できるようにする。

- 更新: 確認・取得・検証・適用準備などの失敗段階、診断コード、内部詳細、取得可能なOSエラー種別/番号を保持する。
- AI接続: probeの失敗を `ai_probe/<stage>`（例: `ai_probe/turn`、`ai_probe/validate_settings`）、安定code、安全な原因として同じ診断に記録する。設定検証の拒否は `validate_settings` として区別する。
- 同じoperationのsnapshotが重複しても、記録は一度だけ（UI側 `take_unrecorded_probe_diagnostic` が `operation_id` で判定する）。

## 実装

- `crates/app/src/update_diagnostics.rs`: 更新state配下（`<state>/updates/<InstallId>/diagnostics.log`）へ、上限付き（20件・16KiB・詳細400文字）の1行レコードを保存する。保存と設定からのコピーは同じ秘匿規則を通る。各レコードにrun idを持たせ、現在の起動と以前の起動を分けて表示する。保存できない場合はその起動中の記録だけを表示し、理由を明記する。編集は妨げない。
- 更新の失敗段階: `check` / `write_download` / `download` / `verify` / `save_history` / `publish` / `start` / `discard` / `restore` / `prepare` / `placement`。
- OS情報: `io_detail`（`os_kind`、取得できた場合だけ `os_error`）を使い、取得できないものを捏造しない。
- AI probe: `crates/ai/src/service.rs` が `ProbeDiagnostic`（`operation_id` / `stage` / `code` / `cause`）をsnapshotに載せる。`cause` は固定語または安全に整形した分類で、providerの生メッセージ、probe入力、モデルID、URL、資格情報を含めない。`crates/ui/src/view/app_update.rs` の `record_ai_probe_diagnostic` が `ai_probe/<stage>` として更新診断のcontrollerへ渡す。
- 設定の更新カードの「診断情報をコピー」で取得する。Unavailableでも表示し、更新操作は再許可しない。設定を開くだけでは通信しない。

## 秘密の除外

認証情報、API key、token、署名付きURLのquery/userinfo、ユーザー名を含むパス、AI入力・応答本文、文書本文、providerの生レスポンスを記録しない。保存と共有は同じ秘匿規則を通す。

## 決定的テスト（ネットワーク・実リリース・手動GUI不要）

- `crates/ai/tests/account_service.rs`
  - `probe_failures_leave_stage_code_and_safe_cause_for_the_local_diagnostics`: fakeのapp-serverで401/429/不正なturn ackを発生させ、stage・code・安全な原因が残り、provider生メッセージ・probe入力・secretが含まれないことを確認する。
  - `probe_settings_validation_failure_names_the_rejecting_check`: 未サインインの設定検証失敗が `validate_settings` として残ることを確認する。
- `crates/app/src/app_update.rs`
  - `a_real_ai_probe_failure_reaches_the_saved_diagnostic_text_without_secrets`: 実際の `AiService` の設定検証失敗から得た `ProbeDiagnostic` を診断controllerへ渡し、共有テキストと保存ファイルの両方に `ai_probe/validate_settings`・code・安全な原因が含まれ、probe入力とモデルIDが含まれないことを確認する。
  - `an_ai_probe_failure_is_saved_beside_update_failures_without_secrets`: 更新失敗と並べて保存され、secretがredactされることを確認する。
  - 更新側の `a_failed_check_is_recorded_with_its_stage_and_code`、`a_write_failure_after_the_download_is_not_reported_as_a_failed_check`、`a_candidate_that_cannot_be_moved_keeps_its_cause`、`a_runner_that_cannot_start_fails_the_operation_instead_of_hanging` など。

## ログの取得

設定画面の更新カードで「診断情報をコピー」を押すと、現在の起動と以前の起動の記録を含む診断テキストがクリップボードに入る。保存先は上記の `diagnostics.log` で、アプリbundle内には書かない。

## 検証状況

この実装メモの変更時点では、worker環境でテストを実行していない。テストとlintは信頼済みの後段job（CI）で確認し、レビュー・GUI・mergeは別のCommander判断とする。

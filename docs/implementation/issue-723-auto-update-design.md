# Issue #723 設計（採用版）

## 司令塔による修正（Jev 判断、2026-11-13）

ドラフト（Codex GPT-6.1 Sol / medium）に対し、意味判断を Jev に諮った結果で1点だけ変更して採用する。

- **尋ねる段階**: Ready になってから（ready_first、Jev 0.98）— ドラフトどおり
- **表示方式**: **オーバーレイ帯に変更**（Jev 0.89）。ドラフトの「レイアウトへ組み込み、本文上端の計算を6ファイルで統一」は不採用（0.10）。エディタ本文の座標計算（viewport / link_hover / ai_selection / a11y_links / reading_mode）には一切触れず、本文領域の上端に重ねて表示する。帯は Ready のときだけ現れて先頭数行を一時的に覆うが、「後で」で即座に消え、フォーカスを奪わない。これに伴い、変更ファイル一覧のうち座標補正を目的とする viewport.rs・link_hover.rs・ai_selection.rs・a11y_links.rs・reading_mode.rs の変更は**行わない**。render.rs へはオーバーレイの配置のみを追加する
- **Ready 保持中は新版を確認しない**（保持候補の再検証のみ）: Jev 0.64 で採用
- **タイミングと抑制**（起動30秒後＋6時間間隔、最終試行の永続化、「後で」で同バージョン6時間抑制）: Jev 0.64 で採用
- **修正版全体**: Jev 0.63（ドラフト全体は 0.34）

以下はドラフト本文（表示方式と変更ファイル一覧のうち上記に抵触する部分は本修正が優先する）。

---

# Issue #723 自動更新確認・再起動案内 設計書

## 概要

起動後に遅延して更新を確認し、その後は **6時間間隔**で確認する。自動確認で更新を検出した場合は、既存のダウンロード・検証処理を使って `Ready` まで進め、メインウィンドウに非モーダルの案内を表示する。

> 新しいバージョンがあります。再起動して更新しますか？  
> ［再起動して更新］［後で］

再起動は利用者の操作でのみ開始し、既存の未保存文書・IME・保存中処理等のガードと適用フローを通す。自動処理の失敗は既存のローカル診断へ記録し、手動確認用のエラーテキストは表示しない。

本書のコード確認対象は `fe715608b131521c114446cab106cb41aa43b829`。ローカルの `HEAD` と `origin/main` は一致している。[Issue #723](https://github.com/hide212131/hane/issues/723) の本文も確認した。リモート main の最新 SHA と関連する進行中 PR は未確認であり、実装着手時に再確認する。

以下、既存シンボルと、追加を提案するシンボルを区別する。コード変更・テスト実行は行っていない。

## 現状の接続点（ファイルと行の根拠）

| 接続点 | 確認した根拠 | 設計への影響 |
|---|---|---|
| サービス生成 | `crates/app/src/main.rs:212`、`start_update_service` | 現在は明示的なボタン操作後だけ通信する契約。サービス生成と自動処理開始を分離して、このコメントも更新する |
| アプリ単位の所有 | `crates/app/src/main.rs:208`、`AppOwnedUpdateService`。同 `:918` 付近で global に格納 | ウィンドウごとに確認タイマーを作らず、アプリ単位で一つ持つ |
| 更新状態機械 | `crates/app/src/app_update.rs:132`、`Stage`。同 `:177`、`State` | `Checking → Available → Downloading → Ready` を再利用する |
| 確認開始の制約 | `crates/app/src/app_update.rs:625`、`Inner::start_check` | 処理中・復元中・`Ready`・`Unavailable` では確認を開始しない。`Ready` を周期確認で上書きしない |
| 検証済み候補の生成 | `crates/app/src/app_update.rs:708`、`Inner::check_latest` | 署名・対象環境・バージョン等の既存検証を通過した候補だけを使う |
| 非同期結果の鮮度 | `crates/app/src/app_update.rs:187`、`Ticket`。同 `:659`、`Inner::run_check` | `InstallId + OperationId + CandidateId` の照合を維持する |
| UI 契約 | `crates/ui/src/view/app_update.rs:43`、`AppUpdateSnapshot`。同 `:114`、`AppUpdateController` | 自動確認設定と通知状態を、この境界を通して UI へ渡す |
| 手動操作 | `crates/ui/src/view/app_update.rs:803`、`EditorView::run_app_update_action` | `CardAction::Check` は `controller.check()` を呼ぶ。既存の操作を残す |
| UI 更新 | `crates/ui/src/view/app_update.rs:773`、`EditorView::poll_app_update` | 200ms 間隔だが、処理が終わると停止する。将来自動で始まる処理の通知には単独では不足する |
| ローカル復元 | `crates/app/src/app_update.rs:1052`、`Inner::start_restore` | 現在は `Idle` から一度だけ復元する。起動時に再利用するが、`Ready` の再検証とは分ける |
| 再起動ガード | `crates/ui/src/view/app_update.rs:1112`、`prepare_update_restart`。同 `:1226`、`start_update_restart` | 案内のボタンも既存経路へ接続する |
| 適用可能性 | `crates/app/src/app_update.rs:1182`、`Inner::restart_supported` | trust・対象環境・配置が有効な場合だけ自動取得・再起動案内を許可する |
| 設定カード | `crates/ui/src/view/app_update.rs:863`、`app_update_card`。`crates/ui/src/view/settings_screen.rs:491` | 自動確認トグルを既存の更新カードへ追加する |
| 設定保存 | `crates/session/src/store.rs:52`、`Settings`。同 `:454`、`impl SettingsRepository for FileStateStore` | `settings.conf` の既存形式へ設定キーを追加する |
| 時計 | `crates/update/src/clock.rs:5`、`Clock` | 壁時計と単調時計を新設せず再利用する |
| テスト時計 | `crates/update/src/testing.rs:79`、`FixedClock` | `advance` と `set_now` で実時間待機なしに検証できる |

既存 UI パターンは次のとおり。

- `crates/ui/src/view/ai_settings.rs:2068` の `action_confirmation` は、説明文とボタンを持つ画面内確認 UI の例。
- `crates/ui/src/view/app_update.rs:937` 付近では、`restart_message` を更新カード内に表示している。
- `crates/ui/src/view/render.rs:476` では、ヘッダー下に検索バーを通常レイアウトの兄弟要素として配置している。通知帯の配置はこの方式に倣う。
- `crates/ui/src/view/initialization.rs:121` 付近の周期処理、および `crates/app/src/main.rs:1025` 付近のアプリ側ポーリングは、`cx.spawn` と `background_executor().timer(...)` を使っている。

共通の通知センターや、今回そのまま流用できる汎用バナー API の存在は未確認。新たな通知基盤全体は作らず、更新専用の表示を追加する。

## 設計判断（各判断に理由）

### 1. 起動後30秒、以後6時間間隔とする

初回の自動確認は、メインウィンドウ生成後 **30秒以降**に開始する。保存済みの最終試行から6時間未満なら、その残り時間も待つ。

```text
初回待機 = max(30秒, 最終試行時刻 + 6時間 − 現在時刻)
```

最終試行がなければ30秒後とする。

理由は、文書復元・初回フォルダー選択・起動直後の入力とネットワーク処理を集中させず、短時間の再起動による確認の連発も防ぐためである。

- 最終「成功」ではなく、実際に受理した最終「試行」の開始時刻を保存する。
- オフラインや取得失敗でも、次の自動試行は6時間後とする。
- 手動確認は間隔制限を受けない。受理した手動確認も最終試行時刻へ反映し、直後の自動確認を避ける。
- 復元中や処理中で開始できなかった場合は、試行済みとして記録しない。
- スリープ復帰時に経過分をまとめて実行せず、期限超過なら一回だけ実行する。

プロセス内の期限判定には `Clock::monotonic_ms`、再起動をまたぐ経過判定には `Clock::now_unix` を使う。未来の保存時刻は異常値として扱い、30秒後に一回確認して時刻を修復する。壁時計の逆行で無期限に停止しない。

### 2. タイマーはアプリ単位で一つ所有する

追加する `AutoUpdateCoordinator` が、GPUI の `Task` と確認期限を所有する。`AppOwnedUpdateService` と同様にアプリ寿命へ結び付ける。

実装方式は既存の `cx.spawn` と `background_executor().timer(...)` に倣う。

- 待機中は期限まで待つ。ただし一回の待機を最大60秒に区切り、復帰後の時刻を再評価する。
- 処理中のみ既存と同じ200ms間隔で snapshot を観測する。
- 状態が変わった場合だけ、登録されたウィンドウへ再描画を通知する。
- 無効化時は待機タスクを取り消す。再有効化時は一つだけ作り直す。
- ウィンドウ参照は弱参照とし、閉じたビューを保持しない。

設定画面の開閉や `attach_app_update` 自体を通信開始点にはしない。自動処理の開始点は coordinator の期限到達に限定する。

### 3. 自動処理は `Ready` まで進めてから尋ねる

自動経路は次の順序とする。

```text
自動確認
  → 署名等を検証した Available
  → 既存の download
  → ハッシュ・サイズ・展開等の検証
  → Ready
  → 再起動案内
```

`Available` ではアーカイブが未取得なので、「再起動して更新」と案内するには早い。`Ready` まで進めることで、ボタンの意味を既存の適用操作と一致させる。

自動処理によるダウンロードは、自動確認が生成した候補だけを対象とする。手動確認で得た `Available` を、後から自動処理が引き取ってダウンロードしない。

手動経路は引き続き次の操作を必要とする。

```text
手動確認 → Available → 利用者がダウンロード → Ready → 利用者が再起動
```

### 4. 自動・手動の実行元を内部状態に持つ

追加する `OperationOrigin::{Manual, Automatic}` を、確認・ダウンロードの実行コンテキストへ持たせる。実行元は開始時に確定し、完了処理でも参照する。

これにより、以下を区別する。

- 自動確認成功後だけ、自動ダウンロードを連鎖させる。
- 自動失敗は診断へ記録し、手動用の失敗表示へ変換しない。
- 無効化で取り消すのは、自動処理だけとする。
- 手動のキャンセル・破棄を、自動処理が直ちに再開しない。

自動確認中に `check()` が明示的に呼ばれた場合は、同じ通信へ合流しつつ手動扱いへ昇格する。その結果は通常の手動確認として表示し、自動ダウンロードへの連鎖を止める。

実行元の変更・キャンセル・次段階の開始判定は、既存の状態ロックと Ticket 照合の下で行う。

### 5. `Ready` は保持し、次の周期では再通知の可否を評価する

既存の `start_check` は `Ready` を置き換えない。この契約を維持する。

`Ready` がある場合の周期処理は、新しい候補のネットワーク確認ではなく、保持中候補のローカル再検証と再通知判定にする。期限・署名・対象・配置等を既存の検証ロジックで確認し、成功した場合だけ再通知を許可する。

既存の `restore_local()` は `Ready` を再検証しないため、追加する内部処理 `revalidate_ready_for_prompt` で対応する。helper の起動や再起動準備は行わない。

再検証失敗時は案内を取り下げ、診断へ記録する。検証できない候補を `Ready` のまま案内し続けない。

理由は、次の版を調べるために取得済み候補を捨てたり、未検証の期限切れ候補を繰り返し勧めたりしないためである。保持中にさらに新しい版が公開されても、その版の取得は候補の適用・破棄後になる。

## 状態と永続化

### 設定

`Settings` に以下を追加する。

```text
auto_update_check: bool = true
```

`FileStateStore` の既存の読み書きに、次のキーを追加する。

```ini
auto_update_check=true
```

- キーがない既存設定は `true` とする。
- 明示的な `true` / `false` 以外の値は自動確認を停止し、診断へ記録する。
- 設定ファイルが読み取り不能の場合も、自動通信は開始しない。ファイル不存在の初回起動とは区別する。
- 既存のネットワーク設定の不正値保持・読み取り失敗時の上書き拒否を維持する。

保存先は既存の `FileStateStore::root()/settings.conf`。ルートの解決は `crates/session/src/store.rs:595` の `resolve_state_root` と同 `:627` の `platform_default_directory` に従う。

| 環境 | 既定の保存先 |
|---|---|
| macOS | `~/Library/Application Support/Hane/settings.conf` |
| Windows | `%LOCALAPPDATA%/Hane/settings.conf`。未設定時の既存 HOME fallback も維持 |
| その他 | `$XDG_CONFIG_HOME/hane/settings.conf`、未設定時は `~/.config/hane/settings.conf` |
| 隔離実行 | 既存の pinned root / `HANE_STATE_DIR` を優先 |

### 実行履歴と通知抑制

頻繁に更新する履歴は、設定ファイル全体の保存から分離する。

提案する保存先：

```text
<state root>/updates/<InstallId>/auto-check.conf
```

提案する保存項目：

| 項目 | 意味 |
|---|---|
| `schema` | 保存形式の版 |
| `last_attempt_unix` | 最後に受理した確認試行の開始時刻 |
| `prompt_version` | 最後に案内した版 |
| `prompt_not_before_unix` | 同じ版を再案内してよい最早時刻 |

既存の `hane_update::fsutil::durable_write`（`crates/update/src/fsutil.rs:156`）で原子的に保存する。読み取りサイズを制限し、形式や時刻を検証する。署名検証の sequence floor、journal、更新済み判定には利用しない。

通知の抑制キーは **InstallId・対象環境・バージョン**とする。操作対象の照合には引き続き `CandidateId` を使う。

`CandidateId` は `crates/update/src/catalog.rs:208` でカタログのハッシュと対象環境から生成される。同じ版のカタログ再発行だけで抑制が解除されないよう、通知キーにはバージョンを使う。

### 「後で」の意味

- 表示した時点で、同じ版の再表示を6時間抑制する記録を保存する。
- 「後で」は全ウィンドウの案内を閉じ、抑制期限を操作時点から6時間後へ延ばす。
- 期限が過ぎても、処理結果を200msごとに見るだけでは再表示しない。次の周期評価と候補再検証を通す。
- 異なる新バージョンは、前の版の抑制対象にしない。
- ウィンドウを閉じる、設定画面へ移る、アプリを再起動する操作だけでは抑制を解除しない。
- 設定カードの手動操作は抑制しない。

履歴保存に失敗した場合は、同じプロセス内の抑制を保持して診断へ記録する。再起動後にも繰り返し表示しないため、履歴が読み取り不能・破損している起動では、起動直後の案内を見送り、起動から6時間経過後の周期評価まで待つ。

### 主な遷移

| イベント | 更新処理 | 案内 |
|---|---|---|
| 自動期限到達、開始可能 | 自動 `Checking` | 表示しない |
| 自動確認で更新検出 | 同じ候補を自動ダウンロード | 表示しない |
| 自動ダウンロード・検証成功 | `Ready` | 抑制条件を満たせば表示 |
| 自動確認・取得失敗 | 診断記録、次周期待ち | 表示しない |
| 「後で」 | `Ready` を保持 | 閉じて6時間抑制 |
| `Ready` の次周期 | ローカル再検証 | 成功・抑制期限経過なら再表示可能 |
| 再起動操作 | 既存のガード・適用経路 | 結果に応じて更新 |
| 自動確認を無効化 | 自動処理のみ取り消す | 自動案内を閉じる |
| 手動キャンセル・破棄 | 既存処理を実行 | 次周期まで自動再開しない |

## UI

### 案内の配置と表示条件

メインウィンドウのヘッダー下に、専用の通知帯を配置する。文書を覆うオーバーレイにはしない。

表示条件はすべて満たすこととする。

- 自動確認が有効。
- snapshot が `Ready`。
- `restart_supported == true`。
- 復元・再検証・再起動準備中ではない。
- 候補 ID とバージョンが存在する。
- 同じ版の通知抑制期間外。
- 設定画面や既存の確認 UI によって案内表示を延期していない。

文面には候補バージョンも付記する。

通知帯の表示だけではフォーカスを移さず、選択・IME・スクロール位置を操作しない。本文での Enter / Escape を通知帯へ割り当てない。「後で」を閉じる操作とし、別の閉じるボタンを設ける場合も同じ意味にする。

設定画面を開いた場合は通知帯を隠し、更新カードを利用できるようにする。設定画面の開閉だけでは抑制記録を更新しない。

### レイアウトと入力座標

通知帯の高さを、文書の表示領域と座標変換へ一貫して反映する必要がある。

既存コードでは検索バーの高さを、以下のファイルで入力座標等へ反映している。

- `render.rs`
- `viewport.rs`
- `link_hover.rs`
- `ai_selection.rs`
- `a11y_links.rs`
- `reading_mode.rs`

通知帯と検索バーを含む共通の本文上端計算を追加し、描画・クリック・選択・IME 関連の位置計算が同じ値を使う。表示だけを挿入する変更では完了としない。

### ウィンドウの扱い

確認した `open_editor`（`crates/app/src/main.rs:879`）では、一つのメイン編集ウィンドウを生成している。

通知状態はビューごとに複製せず、coordinator が共有する。同じサービスを参照する編集ウィンドウが複数ある場合は、アクティブな一つだけに表示し、「後で」は全体へ反映する。非アクティブ時にウィンドウを前面化しない。

ただし、既存の再起動ガードと `RestartTicketSnapshot` は対象ビューを基準としている。**複数編集ウィンドウを維持したままの安全な再起動は未確認**である。実装時に複数ウィンドウ経路が存在すると分かった場合は、全編集ウィンドウのガードと復元情報の保存を確認するまで、通知帯からの再起動を許可しない。

### 再起動ボタン

表示時の候補 ID をボタンへ結び付け、クリック時に current snapshot の `Ready` 候補と一致することを再確認してから、既存の `start_update_restart` へ進む。

未保存文書や IME 等で拒否された場合は、理由を通知帯にも表示する。現在は `restart_message` が設定カード内にあるため、カードを開いていない利用者にも理由が見える接続を追加する。

拒否後にガード条件が解消しても、自動で再起動を再試行しない。

### 設定トグル

更新カード内に配置する。

> ☑ 自動で更新を確認する  
> 起動後と6時間ごとに確認し、更新をダウンロードします。適用前に確認します。

操作時は既存の `SettingsRepository` へ保存し、成功後に coordinator へ反映する。保存失敗は利用者が明示的に行った設定操作の失敗としてカード内に表示し、保存済みと誤認させない。

無効化すると次を行う。

- 待機タイマーを取り消す。
- 実行中の自動確認・自動ダウンロードへキャンセルを通知する。
- 遅れて返った結果から次段階へ進まない。
- `Ready` の取得済みファイルは削除しない。
- 手動処理・進行中の明示的な再起動操作は止めない。

検証処理を即座に中断できない場合も、無効化後の結果から通知・再起動へ進めない。再有効化は初回と同じ期限計算を使い、操作のたびに即時通信しない。

### 失敗・オフライン・利用不可ビルド

自動経路の失敗は、既存の `Diagnostics::note` / `flush`（`crates/app/src/update_diagnostics.rs:419`、`:444`）へ一操作につき一度記録する。

自動失敗では以下を行わない。

- `CardModel` に手動確認用のエラー文を渡す。
- 「最新版です」と表示する。
- `Available` を `Ready` と扱う。
- 短時間で再試行する。
- トーストやダイアログを表示する。

snapshot の自動失敗結果は中立な状態へ戻し、診断の存在だけを保持する。手動で開始した処理の `Failed` 表示は従来どおりとする。

trust 未設定、対象環境非対応、配置不正の場合は自動通信・自動取得・通知を開始しない。既存カードの `Unavailable` と診断表示は維持する。自動確認設定が有効であることを、更新可能性の証明として扱わない。

## テスト計画

以下は実装時に追加・実行する計画である。

| 配置案 | テスト名案 | 検証内容 |
|---|---|---|
| `crates/app/src/auto_update.rs` | `first_check_waits_thirty_seconds` | 初回30秒未満では確認しない |
| 同上 | `recent_attempt_throttles_across_restart` | 保存時刻から6時間未満の再起動で通信しない |
| 同上 | `failed_attempt_waits_until_next_interval` | 失敗後も短時間で再試行しない |
| 同上 | `resume_runs_one_overdue_cycle` | スリープ相当の時間経過で一回だけ実行 |
| 同上 | `future_timestamp_does_not_disable_checks_forever` | 未来時刻・時計逆行を処理 |
| 同上 | `busy_skip_does_not_record_an_attempt` | 未開始の処理を履歴へ記録しない |
| 同上 | `later_survives_process_restart` | 「後で」の抑制を再起動後も維持 |
| 同上 | `catalog_reissue_does_not_bypass_version_snooze` | 同版の候補 ID 変更で再通知しない |
| 同上 | `unreadable_history_defers_startup_prompt` | 履歴異常でも起動ごとの連続通知を防ぐ |
| `crates/app/src/app_update.rs` | `automatic_check_downloads_only_its_own_offer` | 自動確認由来の候補だけを自動取得 |
| 同上 | `manual_check_still_stops_at_available` | 既存の手動段取りを維持 |
| 同上 | `manual_join_stops_automatic_download_chaining` | 合流後の実行元と表示を検証 |
| 同上 | `automatic_failure_is_diagnostic_only` | 自動失敗のコード・文言が手動エラーへ出ない |
| 同上 | `disable_drops_late_automatic_results` | 確認・取得・検証の各段階で無効化 |
| 同上 | `ready_revalidation_failure_removes_prompt_eligibility` | 不正・期限切れ候補を案内しない |
| 同上 | `unconfigured_trust_never_starts_automatic_network` | fail-closed を維持 |
| `crates/session/src/store.rs` | `missing_auto_update_key_defaults_to_enabled` | 既存設定の移行 |
| 同上 | `auto_update_setting_round_trips` | 無効値・有効値の保存と再読込 |
| 同上 | `invalid_auto_update_value_disables_background_checks` | 不正値で自動通信しない |
| `crates/ui/src/view/app_update.rs` | `prompt_requires_ready_and_restart_support` | 表示条件の組合せ |
| 同上 | `later_dismisses_all_views_without_changing_ready` | 抑制共有と候補保持 |
| 同上 | `stale_prompt_cannot_restart_another_candidate` | 古いボタン操作を拒否 |
| 同上 | `restart_blocker_is_visible_in_prompt` | 未保存・IME の拒否理由を表示 |
| 同上 | `prompt_does_not_take_editor_focus` | 表示後も編集を継続可能 |

スケジューラの単体テストには既存の `FixedClock` を使う。更新サービスの状態遷移には既存の `with_runner` と `ScriptedTransport` を使い、処理完了・キャンセル・遅延結果の順序を制御する。

GPUI テストでは仮想時間を進め、設定画面を開かなくても通知へ到達することを確認する。既存の「サービス接続・設定画面表示だけでは通信しない」テストは維持し、自動開始までの時間経過を別テストで明示する。

実 OS の GUI 確認は対象 SHA の Debug / development build と隔離 state、既存の `validation-feed` を使う。確認項目は次のとおり。

- 文書編集中に通知が現れても、入力・選択・IME が維持される。
- 検索バーと同時表示しても、クリック・ドラッグ・リンク・AI 表示位置がずれない。
- 狭いウィンドウでも両ボタンを操作できる。
- 「後で」、アプリ再起動、再通知期限経過が仕様どおり動く。
- 未保存文書・IME 中の再起動拒否と、保存後の既存適用経路が動く。
- オフライン・署名不正・trust 未設定で案内が出ない。
- 設定無効化後も手動確認・ダウンロード・適用が利用できる。

Windows GUI は Windows 環境から直接実施する。実装時には `hane-gui-test` の手順を適用する。

## 変更ファイル一覧

以下は製品実装時の予定であり、本設計依頼では変更しない。

| 区分 | ファイル | 変更概要 |
|---|---|---|
| 新規 | `crates/app/src/auto_update.rs` | 期限計算、履歴保存、通知抑制、アプリ単位の coordinator、単体テスト |
| 変更 | `crates/app/src/main.rs` | coordinator の生成・所有、ウィンドウ接続、起動後タイマー開始、旧コメント更新 |
| 変更 | `crates/app/src/app_update.rs` | 実行元、候補を限定した自動連鎖、自動失敗の分離、自動処理キャンセル、`Ready` 再検証 |
| 変更 | `crates/session/src/store.rs` | 設定フィールド・既定値・保存キー・読込状態・保存テスト |
| 変更 | `crates/ui/src/view/app_update.rs` | controller 契約、更新カードのトグル、通知帯、候補照合、再起動拒否理由の表示 |
| 変更 | `crates/ui/src/view.rs` | 通知表示・本文上端計算に必要な状態とメソッド |
| 変更 | `crates/ui/src/view/initialization.rs` | UI 状態初期化 |
| 変更 | `crates/ui/src/view/render.rs` | 通知帯の配置と表示領域計算 |
| 変更 | `crates/ui/src/view/viewport.rs` | 通知帯を含む入力座標・表示領域計算 |
| 変更 | `crates/ui/src/view/link_hover.rs` | リンク位置の補正 |
| 変更 | `crates/ui/src/view/ai_selection.rs` | 選択 AI の表示位置補正 |
| 変更 | `crates/ui/src/view/a11y_links.rs` | アクセシビリティ用位置の補正 |
| 変更 | `crates/ui/src/view/reading_mode.rs` | 本文上端に依存する計算の補正 |
| 必要時 | `crates/ui/src/lib.rs` | 新しい公開契約型の再公開 |

公開 API・トレイトへの影響：

- `Settings` にフィールドを追加するため、構造体リテラルの利用箇所を更新する。
- 設定読込不能と初回起動を区別するため、`SettingsRepository` に読込状態を取得する契約を追加する。既存の `load` / `store` の意味は維持する。
- `AppUpdateController` に自動確認設定の取得・変更、および通知抑制操作の契約を追加する。設定保存の失敗を UI へ返せる非同期結果の形にする。
- `AppUpdateSnapshot` には自動確認の有効状態と通知対象を追加する。既存の phase 列挙値は増やさない。
- `PreparedUpdate`、`Clock`、Transport、署名・journal・helper の公開契約は変更しない。

設定ファイルは全体保存なので、更新トグルの保存後に古い `Settings` が書き戻されないよう、アプリ内の設定保存経路へ最新値を共有する。更新処理のワーカースレッドから `settings.conf` を直接書き換えない。

## 対象外・リスク

- helper、journal、署名検証方式、リリースカタログ、配布方式は変更しない。
- 自動適用・強制再起動、OS 通知、版の永久スキップ、間隔のユーザー編集は対象外。
- 6時間間隔と30秒遅延は今回の設計値であり、既存仕様から導いた値ではない。
- 自動ダウンロードは通信量とディスク容量を使用する。設定説明で明示する。従量制回線の自動判定は対象外。
- `Ready` 保持中はさらに新しい版を確認しない。既存候補を保持する安全性を優先する制約として明示する。
- 通知帯の追加は本文座標に影響するため、描画だけの確認では不足する。入力・リンク・選択・アクセシビリティの回帰確認が必要。
- 複数プロセスでの通信回数の完全な一元化は対象外。既存の候補 lease・更新時の排他は維持する。
- 複数編集ウィンドウの再起動安全性、関連 PR との競合、リモート main の最新状態は実装着手時の未確認事項である。全域リファクタリング完了を先行条件にはせず、更新・設定・本文座標の重なる契約に限って確認する。
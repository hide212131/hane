# Issue #653 外部変更追従の実装準備

Refs #653

基準main: `ad4b80b155bf219a9c5686858dba3c87f14c3e77`。
【準備commit時点の履歴】準備commitはClaude実装待ちの文書準備のみで、その時点では製品機能・テスト・GUI・受入条件は未実装・未検証だった。この記述はcurrent headの状態ではない。current headの実装・検証状況は下記「current headの状態」と「実装記録」を参照する。

## current headの状態

対象headは `6cd241f85fba86a166dee5d14448b4a79a57fef9`（base / current main `29ed353d1548db7d2df61ffa85085dc4457c98cc`）。

- 実装: 以降の「実装記録」の範囲がcurrent headに含まれる。受入条件を満たしたことを意味しない。
- exact-head verifier（[run 41098937044](https://github.com/hide212131/hane/actions/runs/41098937044)）: success。
- current-head CI（[run 41099201210](https://github.com/hide212131/hane/actions/runs/41099201210)）: success。両OSのworkspace testsとClippyはpass。
- Windows GUI matrix: 14 PASS / 3 UNKNOWN / 0 FAIL。次の3件はUnknownのままで、passとして扱わない。
  - 通常Saveとautosaveの競合
  - アプリ生成draftの再起動後end-to-end復旧
  - watcher失敗警告の可視性
- Mac GUI: PR本文にある部分確認のみ。上記Unknownを含め、その範囲を越えて主張しない。
- Issue #653のGUI条件は未完了のため、PRはDraftを維持する。この文書の更新は製品受入やIssue完了を意味しない。
- 上記は観測時点の値であり、headが変わった場合は旧headの証拠として使わない。次headのCI・review・GUIは取得するまで成功扱いにしない。

### 検証履歴（旧head、current headではない）

- head `89eb9dfe9139cb08058aac354a0b55570fdab258`（base `cc2c19979726195a111a32913d145846c8b651af`、観測時のcurrent main `29ed353d1548db7d2df61ffa85085dc4457c98cc`）。
- exact-head verifier（[run 40923553155](https://github.com/hide212131/hane/actions/runs/40923553155)）: workspace regression testsとlintはsuccessで完了した。
- 通常PR CI（[run 40923856998](https://github.com/hide212131/hane/actions/runs/40923856998)）: `action_required` / jobs 0のままで、成功ではなかった。
- 当時のMac / Windows GUI確認: 未完了。

## 正本

- 要求・全受入条件: https://github.com/hide212131/hane/issues/653
- 採用済み設計: [外部変更の実装仕様](../external-file-change-implementation-spec.md)。設計PR #654は設計文書のみとしてmerge済み。
- 実行・判断: current mainのCommander PolicyとClaude実行手順。

## 対象

開いている作業フォルダ内／単独の保存済み文書の外部変更を背景で確認する。未編集時の自動再読込、未保存編集・IME・undo/redo・draftの保護、保存前の外部版照合、文書別非モーダル通知、差分確認・確認付き再読込・別名保存・明示上書きを実装する。採用設計のT01〜T12を検証表とし、両OSのCIとT01/T02/T03/T04/T07/T08 focused GUIを対象SHAに結び付ける。

## 共有契約の着手確認

- #309: 既存DocumentSession / SessionSet / FileService / DraftStoreを再利用する。current mainのfinish_openはloading_paths.removeをgeneration guard前に行う。旧完了による新owner markerへの影響は未証明で、reloadで同経路を無条件に流用しない。必要な局所整理が判明した場合は範囲と証拠を報告し、機能へ混ぜず別PRで受け入れる。
- #309の過去のdraft write順unknownに対し、current mainには#610でwrite_snapshot、next_draft_snapshot_sequence、同draft排他とstale拒否が入り、a_stale_snapshot_arriving_after_a_newer_flush_does_not_roll_it_back等がある。既存の受入済み経路を使う。今回のreload generationとの接続・remove後の後着write・復旧内容は改めて制御検証する。source確認は新しいtest実行の証拠ではない。
- #308: 保存slot / SaveTicketと命名pending / in_flight / scheduled / deferredの同時成立を保持し、DocumentInstance、workspace epoch、revisionを混同しない。命名状態の全域整理は含めない。
- #368: 文書置換時のparse/cache/AI結果失効を既存identity契約へ接続し、退役jobも実行数に残す寿命・上限を維持する。
- #567 / PR #638: 初期化の機械的移動は未merge。初期化全体の移動は混ぜず、featureに必要なfield/hookのみを追加する。candidate確定時にcurrent baseとの重なりを確認する。
- #555: 受入済みOS watcher一覧経路を利用し、一覧更新のために未open本文を読む契約へ変えない。本文確認は開いた対象文書に限定する。

#297 / RF0全体は先行条件にしない。同責務の実行中writerは準備時点でなし。必要な局所契約の証明は省略しない。

## 実装境界と検証

session/identity/service/watch、UIのopen/save/watch/footerと比較・解消UI、必要なeditor/IME/draft接続、直接関係する回帰テストを対象とする。意味・所有権の全域整理や新旧二重経路を追加しない。

baselineと観測外部版を分け、内容fingerprint・file identityで同stamp更新とatomic置換を扱う。結果受理時と保存実行直前にinstance/path/revision/sequence/IME/I/O状態を照合する。古い承認を再利用せず、通常保存の再実行を強制上書きにしない。消失・読込不可・監視失敗はunknown/失敗として本文を維持する。

変更前後の大文書・複数タブの応答、背景確認時間、保持メモリ、snapshot上限を比較する。未実行の計測・CI・GUIを成功扱いにしない。Windows GUIは接続先WindowsのChatGPTから直接操作し、Mac経由VMを使わない。

## 実装記録（Claude worker、未検証）

このworkerはshell・test・build・lintを実行していない（worker自身の未実行という履歴）。以下はコードを書いた範囲の記録であり、worker側では検証済みを意味しない。current headで別途取得された検証結果は上記「current headの状態」に記し、worker自身の実行とは区別する。検証はCommanderが対象SHAへ紐付けて取得する。

### 実装した範囲

- `identity.rs`: `ContentFingerprint` / `Fingerprinter`（保存形式のbytesから作る内容fingerprint、2レーン+長さ）。`FileState` にbaselineを追加。open直後は未確立（UI threadでhashしない）で、最初の背景確認、または保存成功時に確立する。
- `service.rs`: `observe_disk`（有界3回の整合読込。handleのstampと読込後のpath stampが一致しない読込は再試行し、収束しなければ `Unreadable`）、`DiskVersion`、`OverwriteGuard::ExpectContent`（保存直前にwatcher到達に依存せず実体とbaselineを照合。消失・読込不可・不一致は書かない）、`SavedFile.fingerprint`。
- `session.rs`: 背景確認の `begin_external_check` / `finish_external_check`（ticketにgeneration・path・要求sequence・revisionを束縛、文書あたり1 jobと1件の追従に集約）。未編集・IMEなし・保存中でない場合だけ新generationで自動再読込し、カーソルは有効な文字境界へ丸めて復元。それ以外は本文・履歴を保持して `Changed` / `Missing` / `Unverifiable` を記録。`ResolutionApproval`（revision・外部版・操作種別に束縛した一回限りの承認）と `request_confirmed_overwrite` / `apply_confirmed_reload`、`SaveIntent::Confirmed`。通常保存・自動保存は常に `ExpectContent` で、キュー経由でもForceへ昇格しない。`apply_file_event` は通知だけでstampを進めない。
- UI: `view/external_change.rs`（watcher batchで開いている文書だけを背景確認、結果受理時にDocumentInstanceとticketを照合、競合記録、保存状態メニューの「名前を付けて保存／ディスクの内容を読み直す／この内容で上書き」と確認ダイアログ）、`folder_watch.rs` からの接続、`session_save.rs` の競合時再確認と `ApprovalExpired`、`footer.rs` の案内文修正。
- テスト: `crates/session/tests/external_change.rs`、`view/external_change.rs` 内のGUIイベントtests、`conflict_rules.rs` の削除後保存（黙って再作成しない契約へ更新）。

### 監視寿命（watch lifecycle）の追加実装（Claude worker、未検証）

このworkerはshell・test・build・lintを実行していない。コードを書いた範囲の記録であり、検証済みを意味しない。

- `crates/session/src/watch.rs`: `FolderWatcher::start_directory`（非再帰の親ディレクトリ監視）と `WatchedNames`（開いているファイル名だけを通す、実行中に差し替え可能な集合）を追加。既存の `start` / debounce / failure batch / Drop は同じ経路を共有し、通知集中は既存のquiet / max-waitに集約される。ディレクトリ自体の消失・overflow・通知errorは名前で絞らず常に通す。
- `crates/ui/src/view/document_watch.rs`（新規）: 開いている保存済み文書と登録済み監視の差分を、renderごとのpath集合比較（I/Oなし）で同期する。作業フォルダwatcherが正常に接続され、そのrootに含まれる文書は親監視を作らない（二重登録なし）。それ以外は親ディレクトリごとに1監視を共有し、最後のownerが消えると監視と受信taskを解放する。untitledは対象外。work folder generationが変わると全登録を破棄し、旧tokenの遅延batchは棄却される。
- 監視登録後に文書ごとの初回確認を開始する（新open・同path再open・保存先変更・rename・フォルダ切替）。作業フォルダ内の文書も、既存watcherが正常に接続された後に同じ経路で初回確認する。本文確認は既存の `start_external_check`（文書ごとに1 job + 1 pending、結果はDocumentInstance / ticketで照合）を使い、未openファイルは読まない。
- 失敗（開始失敗・overflow・通知error・監視root消失）は成功扱いにしない。通知由来の失敗は再登録して開いている文書を再確認する。再登録できない場合は文書に紐付けた警告をフッターに残し（tab切替・status更新で消えない）、タイマー1本で再試行する。再試行自体は本文を読まず、復旧時だけ確認する。保存状態メニューに「ディスクの内容を再確認」を追加（`ResolutionAction::Recheck`）。
- アプリ復帰: 初回renderで一度だけ `observe_window_activation` を登録し、アクティブ化時に失敗中の監視を再登録して開いている文書を再確認する。IME中・編集中の保護は既存のsession判定と結果受理時のticket照合に任せる。
- テスト: `document_watch.rs` のGPUI deterministic tests（実OS threadは起動せず既存のtest batch注入を再利用）と、`watch.rs` の名前絞り込み・実FS（atomic置換）tests。いずれも未実行。
- 既存テストへの影響の懸念（未確認）: render時に初回確認が走るため、描画を伴う既存テストで開いた直後にディスクを書き換えるものは、初回確認の結果（未編集なら自動再読込）の影響を受ける可能性がある。

### 再読込承認の鮮度（T04、Claude worker、未検証）

- 確認ダイアログの返答後、`apply_confirmed_reload` を直接呼ばず、`DocumentSession::begin_confirmed_reload`（現在のIME・保存・approval・path・generation・revisionを再確認し、要求sequenceを発行）→ 背景の `run_reload_verification`（既存の `observe_disk`、UI threadでは読まない）→ `finish_confirmed_reload` の順に進める。
- 返答後に読んだ版のfingerprintが、確認画面で示した版と同一の場合だけ適用する。変わった／消えた／読めない場合は本文・undo/redo・draftを保持し、新しい観測を外部版として記録して旧approvalを失効させ、非モーダルの案内（status、dirty時はConflict表示）で再確認を求める。旧requestや編集・文書切替後の結果は `Stale` として捨てる。
- テスト: `crates/session/tests/external_change.rs` に、確認後・適用前にディスクだけが再変更される場合、消失・読込不能、編集後／旧requestの破棄、通常成功の回帰テストを追加。UI(GPUI)側の専用テストは追加していない。
- 未実行: build・tests・Clippy（macOS / Windows）、focused GUI。上書き経路は変更していない。

### 差分確認（AC4、Claude worker、未検証）

このworkerはshell・test・build・lintを実行していない。コードを書いた範囲の記録であり、検証済みを意味しない。

- `crates/ui/src/view/external_comparison.rs`（新規）: 読取専用の比較。左にsessionが観測済みのディスク版（`observed_document()`）、右に編集中版を、開いた時点の固定snapshot（String）として表示する。文書id・`DocumentInstance`・編集revision・外部fingerprint（`observed_fingerprint()`）に束縛。ファイル読込・hash計算はせず、メモリ上の2本文のコピーのみ。観測済み本文がない場合は空や古い内容を出さず、比較を開かず案内statusを出す。
- 鮮度: `Current` / `Updated`（手元編集または外部版更新）/ `Gone`（文書instance切替）。`Updated` では「内容が更新されました」と「最新の内容で再比較」を表示する。再比較は現在のsessionの観測で撮り直し、既存の `start_external_check`（背景確認）を呼ぶ。`Gone`・非active文書の比較はrenderで破棄し、作業フォルダ切替でも破棄する。
- 古い比較中は `run_resolution` の再読込・上書き・別名保存を拒否する（`external_comparison_is_stale`）。比較画面自体には再読込・上書き・保存の操作を置かない。
- 入口: 保存状態メニューに「差分を確認」（`ResolutionAction::Compare`、再読込と同じく外部版を保持している競合時のみ）。表示は各版の先頭2,000行まで（超過は注記）。強調は共通の先頭／末尾行を除いた範囲の行単位で、上限内の行のみで計算する。
- 変更: `view.rs`（field・mod・切替時破棄）、`view/render.rs`（prune・overlay）、`view/external_change.rs`（action・stale拒否）、`view/footer.rs`（menu項目）。
- テスト（未実行）: `external_comparison.rs` に、開く／閉じるで本文・revision・dirty・ディスク内容が不変、手元編集・外部版更新・文書置換後のstale／Gone、再比較、観測なしで開かない、footer actionとstale時の再読込・上書き・別名保存の拒否、行範囲・行数上限の単体テスト。undo/redo・draftの直接検証、IME中の比較は追加していない。
- 未実行: build・tests・Clippy（macOS / Windows）、focused GUI（T04）。

### 未実装・未検証（受入条件を満たしたと扱わない）

- 監視寿命以外の独立root（下記の差分確認、ディスク版の整合読込・fingerprint・OS file identity、保存承認鮮度、同一実体のSave As、全域リファクタリング）。
- 別名保存で同一実体（symlink・ハードリンク等）を選んだ場合の保護。現状は字句上の同一pathだけを同一ファイルとして扱う。
- IME中の延期と非アクティブ文書の自動再読込のGUI経路テスト、再読込時のdraft write / AI・解析結果との接続のUIテスト（sessionのgeneration更新までは実装）。
- 保存前の内容照合は対象ファイル全体を背景で読む。大文書・複数タブでの入力応答、背景時間、メモリ、snapshot上限の前後比較（T12）は未測定。
- 外部processとの原子的compare-and-swapは保証しない。照合から置換までの間に他者が書く競合窓が残る（検出可能なのは照合時点の不一致のみ）。
- 両OSの通常PR CI（run 40923856998は `action_required` / jobs 0）とfocused GUI（T01/T02/T03/T04/T07/T08）は、current headで未完了。exact-head verifierのworkspace regression testsとlintがsuccessであることは上記「current headの状態」のとおりで、これで通常CIやGUIを代替しない。

## 対象外と完了

共同編集、自動マージ、排他ロック、クラウド専用同期API、全共有方式の通知保証、永続履歴、新runtime、永続形式移行、無関係なリファクタリング、保護パス・workflowの変更は対象外。

本PRにはRefs #653を使い、製品AC完了前にIssueをcloseしない。review・CI・必須GUI・current head/base・expected SHA guardを満たして受け入れる。設計PRはrelease対象外とし、機能merge後のrelease batchは機能追加を含める。releaseの実行窓口は親チャットで、本PR担当はmerge状態を記録する。

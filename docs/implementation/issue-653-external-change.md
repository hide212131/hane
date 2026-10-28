# Issue #653 外部変更追従の実装準備

Refs #653

基準main: `ad4b80b155bf219a9c5686858dba3c87f14c3e77`。
本commitはClaude実装待ちの文書準備のみ。製品機能・テスト・GUI・受入条件は未実装・未検証。

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

## 対象外と完了

共同編集、自動マージ、排他ロック、クラウド専用同期API、全共有方式の通知保証、永続履歴、新runtime、永続形式移行、無関係なリファクタリング、保護パス・workflowの変更は対象外。

本PRにはRefs #653を使い、製品AC完了前にIssueをcloseしない。review・CI・必須GUI・current head/base・expected SHA guardを満たして受け入れる。設計PRはrelease対象外とし、機能merge後のrelease batchは機能追加を含める。releaseの実行窓口は親チャットで、本PR担当はmerge状態を記録する。

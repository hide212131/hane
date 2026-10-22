# Issue #553: 更新失敗の診断テキスト — 実装準備

対象: https://github.com/hide212131/hane/issues/553
基準main: `7de394127383b50641b7345aee4866b6c3b16be0`

このcommitはClaude実装先の準備のみ。製品コード・テストは未変更、受入検証は未実施。要求と受入条件の正本はIssue本文とする。

## 範囲

macOSの通常起動で更新に失敗した本人が、設定画面から失敗段階と具体的原因を診断テキストとして取得・共有できるようにする。既存のアプリ所有更新サービスへ最小限で追加する。

- 確認・取得・検証・適用準備の失敗段階、診断コード、内部詳細、取得可能なOSエラー種別/番号を保持する。背景スレッド開始やファイル移動の失敗も詳細を捨てない。
- 時刻、現在版・候補版、PID、実行パス、更新stateの識別を含め、複数のHane/stateを区別する。
- ユーザー名を含むパス等は共有時に伏せる。認証情報、トークン、署名付きURLのquery、文書本文、AI入出力を記録しない。未加工のエラー文字列を無条件に保存しない。
- 既存state配下等に記録量を制限して保存し、アプリbundle内の書込を前提にしない。
- 設定の更新カードから開くかコピーする最小導線と、段階に合った日本語の失敗説明を追加する。
- 保存不可でも編集を妨げず、更新失敗を成功扱いにしない。署名・ZIP検証、未保存文書の保護、明示操作のみの通信を維持する。

## 先行する最小範囲

#510の配置エラー修正PR #557が、同じ`start_update_service`、更新サービスの利用可能性・Ready復元・取得、設定カードを変更する。#557の受入・main反映後にその経路を確認して実装依頼する。廃止する旧fallbackと新経路への二重実装を避ける。#297/RF0全体や独立したタブ・sidebar・閲覧モード・依存整理の完了待ちは不要。

App Translocationで不適切な保存先へ進む問題そのものは#510。巨大な汎用ログ基盤、テレメトリ、quarantine削除、OS保護の緩和、未公開API、文書/AIデータ変更、保護された運用基盤の変更は対象外。

## 読む場所

`AGENTS.md`、`docs/aadw-command-policy.md`、`docs/aadw-claude-execution.md`、`docs/adr/0034-zip-application-updates.md`、`docs/application-update-implementation-spec.md`、`crates/app/src/main.rs`、`crates/app/src/app_update.rs`、`crates/app/src/https_transport.rs`、`crates/update/src/error.rs`、`crates/ui/src/view/app_update.rs`と関連controller/presentation契約。具体的な型や変更ファイルは実装時のcurrent codeで再確認する。

## 受入条件と検証

1. 通常起動のmacOS検証アプリで代表的な失敗を再現し、設定から診断テキストを取得する。実PID・実行パス・隔離state・対象headを記録し、特別なターミナル起動を利用者の取得条件にしない。
2. 確認失敗とダウンロード後の失敗を区別し、ファイル書込失敗が`io`だけに潰れず、具体的原因と取得可能なOS情報を含むことを自動テストとfocused GUIで確認する。
3. 複数のアプリ/stateの識別、再起動後の参照、過去記録と現在状態の区別を確認する。
4. 秘密を含むURL・パス、長い詳細、反復失敗に対し、秘匿・保存量上限・文字境界を確認する。
5. 書込不可でも編集継続・誤成功防止を確認し、署名検証・文書保護の既存テストを維持する。

必要なfocused tests、required CI、current-head最終レビュー、macOS GUI evidenceを揃えてCommanderが受入判断する。Windows更新成功の実機確認を追加せず、共有コードのWindows CIは維持する。GUI失敗時は`docs/gui-test-recovery.md`で復旧を続ける。

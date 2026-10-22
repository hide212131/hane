# Issue #510: macOS更新配置エラーの修正準備

- 対象: https://github.com/hide212131/hane/issues/510 の「macOS隔離起動の実機不具合と最小修正（2026-10-22）」
- 基準main: `7de394127383b50641b7345aee4866b6c3b16be0`
- 状態: Claude実装先の準備のみ。製品コード・テストは未変更、修正後の検証は未実施。

## 原因と今回の範囲

`platform::locate` はApp Translocationを拒否するが、`start_update_service` が `.ok()` で理由を捨て、実行ディレクトリ内receiptsへのfallbackと更新serviceのIdleを許す。`download_into` はZIP検証後にread-onlyなreceiptsを作ろうとして `io` になる。

実機では0.27.1→0.28.0のダウンロード後に失敗し、FinderでApplicationsから再起動しても隔離起動が再現した。正規実行ファイルの直接起動ではseen保存・更新適用が成功し、0.28.0の正規パス実行とjournal `Committed` を確認した。元OSエラー本文は破棄されており、errnoの保存ログがあるとは扱わない。この旧版の確認は修正後の合格証拠には使わない。

Issue本文の受入条件を正本とする。

1. 配置確認の失敗理由を保持し、隔離/非対応/破損/権限の失敗を正常な未管理配置として扱わない。
2. 更新できない配置から、不適切なreceiptsへのfallbackを使ったダウンロード・Ready復元・適用準備へ進めない。設定画面で理由と実際に成立する対処を日本語で示す。
3. `/Applications` に置く、Finderから開き直すだけで隔離が必ず解消するとは案内しない。正常なMac更新全体を恒久的な手動ZIP置換へ変えない。
4. 正常配置の取得・署名検証・seen保存・再起動適用と、Windowsの既存経路を維持する。起動/設定表示だけでは通信しない。
5. 原因経路の関連回帰テストを追加する。既存service/カードのテストと必要な配置Resultの注入を優先する。

## 読むもの

`AGENTS.md`、`docs/aadw-command-policy.md`、`docs/aadw-claude-execution.md`、`docs/adr/0034-zip-application-updates.md`、`docs/application-update-implementation-spec.md`、`docs/application-update-implementation-handoff.md`、`crates/app/src/main.rs`、`crates/app/src/app_update.rs`、`crates/update/src/platform/macos.rs`、`crates/ui/src/view/app_update.rs`。

## 対象外・検証

診断ログのテキスト保存/コピーは #553で別に扱う。汎用ログ基盤、quarantine削除、OS保護緩和、private API、他インストールの書換え、運用基盤変更を含めない。未保存保護・署名・inventory検証・文書とAIデータの保護を維持する。

Commanderが関連自動テスト、focused GUI、current head/baseのreview等を確認する。GUIは一意bundle・専用stateを用い、利用者の通常Haneや他テストHaneを停止/更新しない。

#297/RF0全体を待つ依存はない。既存更新serviceを使い、#553は本修正後の経路へ追加する。独立したサイドバー・タブ等の作業は止めない。

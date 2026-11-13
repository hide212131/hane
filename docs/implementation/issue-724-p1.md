# Issue #724 P1 実装準備

- 対象 Issue: [#724](https://github.com/hide212131/hane/issues/724)「AI設定画面の見直し — 使えるようになるまでのボタンを極限まで減らす」
- フェーズ: P1（ChatGPT 経路の自動化）
- 基準 SHA: `fe715608b131521c114446cab106cb41aa43b829`（origin/main、設計確認時点）
- 採用済み設計: `docs/implementation/issue-724-ai-settings-design.md`（本 PR に同梱。冒頭の「司令塔による修正」でフェーズ分けを定めている）
- 状態: **未実装・未検証**。本書と設計書だけの準備 PR であり、製品コードはまだ変更していない。

## 今回の範囲（P1）

1. `AiService` が所有する接続準備の状態機械（新規 `crates/ai/src/connection_preparation.rs` 案）。起点は「AI設定を開く／ログイン完了／選択の確定／適用／再試行」の文脈に限定し、描画起点では開始しない。二重起動は合流させ、世代・network が変わった段階結果は採用しない。
2. ChatGPT: 状態取得とモデル一覧取得を自動化し、「状態を更新」「モデル一覧を更新」ボタンを通常経路から削除する。ログイン完了時は既存の照合結果（account/read・モデル一覧）を引き継ぎ再送しない。
3. 推奨モデルの自動選択と自動保存: 優先順位は「利用者が確定した選択 ＞ 保存済みモデル（一覧に存在）＞ is_default ＞ 全ページ取得後の先頭」。一覧に無い保存済みモデルは自動置換せず停止して理由と変更操作を表示する。自動保存は既存の保存経路（revision・所有権・journal）を通し、追加の保存ボタンを押させない。
4. probe の自動実行: 接続確立・モデル決定に続いて自動で実行し、結果だけを表示する。「応答を確認」ボタンを通常経路から削除する。成功は世代キー付きで10分キャッシュし、失敗は再表示だけでは自動再試行しない。
5. 失敗表示: 段階ごとの理由と、原因に対応する主要操作を一つだけ表示する（再試行・再ログイン等）。進捗表示と取消は維持する。
6. 仕様文書の最小更新: `docs/ai-settings-oauth-implementation-spec.md` と `docs/ui/ai-settings/README.md` に、設計書の差分表に沿った自動化の契約差分を反映する。

## 対象外（P2 以降・変更禁止）

- runtime 初回導入の開始ボタン統合（現行の導入ボタンのまま）
- Custom 初回入力の改善（未登録キーの「登録」ボタン等は現行のまま）
- 仕様 HTML（`docs/ui/ai-settings/hane-ai-settings-spec.html`）と `docs/ai-runtime-distribution-implementation-spec.md` の改訂
- 通信プロトコル・認証方式、`probe.rs` の通信プロファイル、`settings.rs` の永続形式、`provider.rs` の接続先検証の変更
- エディタ側の AI 実行機能、provider の自動 fallback、複数接続の並列実行

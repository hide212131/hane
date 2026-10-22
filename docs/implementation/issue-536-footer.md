# Issue #536 実装準備

## 状態と基準

- Issue: https://github.com/hide212131/hane/issues/536
- 対象: フッタ整理・保存状態・文字数・倍率・AI入口の実装を一つの機能PRで完了する。
- 基準ブランチ: `main`
- 実装開始時の基準SHA: `97c279acc063ef51ad5d5ee12af51a0edee174d2`
- 詳細仕様: [footer-redesign.md](footer-redesign.md)。このブランチに設計書全文を含めている。
- この準備文書は製品実装・テスト・画面確認の完了を示さない。

## 今回の対象

F01–F12をすべて対象とする。32px一行フッタ、現在文書に紐づく保存状態と問題表示、Unicodeスカラー値による全体/選択文字数、既存ズームとAI入口の利用、一般設定へのテーマ/自動保存移設、ヘッダの最近のファイルメニュー、既存案内と確認手順の更新を含む。設計書第9節の確認条件を満たす。

Issue #516のAI利用可否はF11と同じ変更・受入証拠で扱い、重複する別実装を作らない。#308/#309等の広い保存整理や全域リファクタリングは先行条件にしない。変更と重なる既存作業がcurrent mainに現れた場合、その部分の衝突だけを解消する。

## 守る条件

- 既存の保存、下書き、文書識別、IME、入力先、ズーム、選択AIの経路を再利用する。新しい保存状態機械、文字数キャッシュ、背景全文走査、通知履歴、設定形式や既定値を加えない。
- 保存失敗は該当文書の有効な保存成功まで保持し、古い/別文書の処理結果で現在表示を上書きしない。
- 本文選択・編集履歴・dirty状態を倍率操作で変更しない。日本語変換中に新しいメニューや無効なAI入口へフォーカスを移さない。
- 設定の移設先を用意してから旧フッタの入口を除く。保存成功を推測で表示しない。
- 変更はIssueと設計書の範囲に限定する。一つの機能PRに直接関連する回帰テストと操作案内を含める。

## 変更を許可する範囲

設計書第8節にある次の製品・案内ファイル、および直接関連する既存テストのみ:

- `crates/ui/src/view/footer.rs`（追加）
- `crates/ui/src/view.rs`
- `crates/ui/src/view/session_save.rs`
- `crates/document/src/lib.rs`
- `crates/ui/src/view/viewport.rs`（必要な場合の薄い接続）
- `crates/ui/src/view/ai_selection.rs`
- `crates/ui/src/theme.rs`
- `scripts/hosted_file_tabs_gui.py`
- `README.md` と対象機能の既存テスト

`.github/`、workflow、権限制御、依存関係、広範な保存/入力経路の再設計は対象外。ユーザーのChatGPTプロジェクト内の`sources/`は参照専用。

## 受入条件

- F01–F02: 32px一行、幅ごとの集約、ライト/ダークと長い表示でも各操作へ到達。
- F03–F05: 無題/下書き/保存中/未保存/失敗/競合の正しい表示、文書別の問題保持、古い結果の拒否、復旧警告との同時確認。
- F06–F07: 空白・改行・Markdown記号を含む全体/選択文字数を同じUnicodeスカラー単位で更新し、大文書のための全文コピー・全文走査をしない。
- F08: 倍率表示と操作を連続ズームへ接続し、本文/選択/履歴/dirty状態を保つ。
- F09–F10: 一般設定でテーマ・自動保存を変更して保持し、設定保存失敗を画面で示す。最近のファイルから未保存内容を失わず開く。
- F11–F12: 日本語変換・入力先・AI選択を保ち、関連テストとREADME/既存確認手順を更新。

## 検証計画

- current head上で必須CIを通す。既定workflowによる`cargo test --workspace --all-features --locked`と`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`を含める。
- 設計書第9節の回帰シナリオを実装上の関連テストで確認する。
- macOSとWindowsで直接GUI確認する。幅740/640/639/440/439/280px付近と279px、ライト/ダーク、一般設定のテーマ変更→本文へ戻る→選択タブ背景・パスコピー、保存/警告、文字数/選択、倍率、ファイルメニュー、IME/入力先を確認する。Windowsは接続先`papa-win`上のChatGPTアプリから操作する。Hosted GUIは新導線の合格根拠にしない。
- 結果は実装PRのcurrent headと必要なbase contextに結びつけて記録する。

## 実装依頼の契約

- AADW v3の初回実装、通常指定モデル`claude-sonnet-5-5`。
- workerはPR head内のファイルだけを読み書きし、shell/git/gh/test/build/lint/pushや次工程の起動をしない。trusted finalizer後、current-head CIとCommanderによるGUI/reviewを確認する。
- Draft解除・merge・Issue close・再送はworkerに行わせない。
- environment/measurement/unknownの失敗を製品パッチへ流さない。利用上限以外の失敗をCodex実装fallbackへ切り替えない。

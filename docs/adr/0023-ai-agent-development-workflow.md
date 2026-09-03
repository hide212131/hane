# ADR-0023: AI agent 開発を実装・レビュー・進行判断に分離する

## ステータス

承認済み

## 日付

2026-09-03

## 背景

Hane の Issue から実装、レビュー、修正、マージまでを coding agent で自動化したい。

単一の agent に実装と自己レビューとマージ判断を任せると、同じ判断傾向が一連の工程に残りやすく、また agent の推論と GitHub 上の書き込み権限が一体化する。さらに、Claude Code、Codex、GitHub Copilot にはそれぞれ異なる実行環境と契約があり、既存のサブスクリプションを役割ごとに使いたい。

GitHub Agentic Workflows は自然言語でリポジトリの状態を読み、次の処理を判断できる一方、safe outputs と GitHub Actions を使って書き込み処理を分離できる。ただし公開プレビューであり、認証や safe output の制約は変更される可能性がある。

## 決定

AI agent を次の3役に分ける。

- **Claude Code**: 実装とレビュー指摘への修正を行う。
- **Codex**: Pull Request のコードレビューを行う。
- **GitHub Copilot**: Codex review、Pull Request、CI を読み、Claude に修正を戻すかマージ判定へ進めるかを判断する。

GitHub Actions は agent 間の状態遷移と権限制御を担当する。Copilot が `ready` と判断しても直接マージせず、最新 head SHA、CI、review thread、mergeability を deterministic workflow で再確認してからマージする。

Claude Code は Claude Code Action を直接使い、Claude Pro / Max の OAuth token を GitHub Actions Secret に置く。Codex review は ChatGPT の Codex と GitHub の接続を使う。Copilot judge は GitHub Agentic Workflows の Copilot engine を使う。

具体的なトリガー、状態管理、認証 Secret、反復回数、マージ条件は
[`docs/agentic-development-workflow.md`](../agentic-development-workflow.md) を正とする。

## 結果

- 実装した agent とレビューする agent を分離できる。
- レビュー結果を採用するかどうかの判断を、実装担当とは別の Copilot に任せられる。
- AI の推論と GitHub の書き込みを分け、最終マージ条件をコードで固定できる。
- Claude、Codex、Copilot の各契約と認証経路を混ぜずに運用できる。
- agent や GitHub Agentic Workflows の仕様変更があっても、役割分担を維持したまま個別の実装を交換できる。

## 棄却した案: Copilot coding agent をすべての司令塔にする

Copilot coding agent に「Claude に実装を依頼し、Codex にレビューを依頼する」と自然言語だけで委譲させる案は採用しない。agent 間の委譲そのものを暗黙の会話にすると、どのイベントで何が起動したか、どの認証枠を使ったか、同じレビューを再処理していないかを追いにくくなる。

委譲は GitHub Actions / Agentic Workflows の明示した状態遷移として扱う。

## 棄却した案: 単一 agent で実装からマージまで行う

構成は単純になるが、実装とレビューの独立性がなくなる。また、agent に広い書き込み権限を与える必要が生じるため採用しない。

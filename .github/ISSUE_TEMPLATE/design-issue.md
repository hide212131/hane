---
name: 設計 Issue（Work → Claude Code）
about: ChatGPT / Work で整理した要求・設計を Claude Code の実装に引き渡すための Issue テンプレート
title: ""
labels: ""
assignees: ""
---

<!--
このテンプレートは、要求整理・詳細設計を担当する Work（ChatGPT）が Issue を作成し、
実装担当の Claude Code へ引き渡すための構成です。
六つの主要項目（背景・解決したい問題・要求・設計上の決定・制約・受け入れ条件）に加えて、
実装裁量の範囲と設計完了 handoff を記述する項目を含みます。
詳細は docs/agentic-development-workflow.md の「Work（ChatGPT）」節、ADR-0023・ADR-0026 を参照してください。
このテンプレートに沿わない既存 Issue でも、既存の /implement 手順はそのまま利用できます。
-->

## 背景

<!-- なぜこの Issue が必要か。確認した基準（対象 commit、関連する既存文書）があれば書く。 -->

## 解決したい問題

<!-- 現状のどこに問題があるか。 -->

## 要求

<!-- 満たすべき要求を列挙する。 -->

## 設計上の決定

<!-- 決定済みの設計とその根拠を書く。未決事項がある場合は区別して明記する。 -->

### 決定済み

### 未決事項

## 制約

<!-- 変更してよい範囲・してはいけない範囲を明記する。 -->

## 受け入れ条件

- [ ] 

## 実装時の判断

<!--
上記の要求・決定・制約を守る範囲で、具体的な変更ファイル・関数・実装手順は
Claude Code が既存コードと関連文書を確認したうえで最終判断する旨を明記する。
-->

## 設計引き渡し

<!-- 設計完了 handoff の書式。書き方の例は docs/agentic-development-workflow.md の「Work（ChatGPT）」節を参照。 -->

設計状態: 設計中 / 設計完了（実装未起動）
成果物・制約・受け入れ条件: 本文参照
実装を阻む未決事項: なし / <内容>
実装担当: Claude Code。レビュー: Codex。進行判断: GitHub Copilot。
起動状況: 未起動 / `/implement` 投稿済み（コメント URL） / 実装中（Pull Request URL）

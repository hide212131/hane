---
name: 設計・実装 Issue（AADW v2）
about: 要求・設計判断・制約・受け入れ条件を整理し、AADW v2 で実装できる状態にするための Issue テンプレート
title: ""
labels: ""
assignees: ""
---

<!--
Hane の現行運用は docs/agentic-development-workflow-v2.md と docs/aadw-command-policy.md を正とします。
この Issue 自体は AADW の機械的な state を持ちません。ChatGPT Commander が GitHub 上の current facts と本文を読み、必要な次の一つの action を判断します。
-->

## 背景

<!-- なぜこの Issue が必要か。関連する既存文書、Issue、PR があれば書く。 -->

## 解決したい問題

<!-- 現状のどこに問題があるか。観測した事実と推測を分ける。 -->

## 要求

<!-- 満たすべき要求を書く。 -->

## 設計上の決定

### 決定済み

<!-- 実装が守るべき設計判断と、その根拠を書く。 -->

### 未決事項

<!-- 不明点がなければ「なし」。実装を阻む unknown は解消するまで pass 扱いしない。 -->

## 制約

<!-- 変更してよい範囲・してはいけない範囲を明記する。 -->

## 受け入れ条件

- [ ] （満たしたかを GitHub 上の evidence で確認できる条件を書く）

## 実装時の判断

<!--
要求・決定・制約を守る範囲で、具体的な変更ファイル、関数構成、実装手順は実装担当が既存コードを読んで判断する。
独立した root cause を一つの修正へ混ぜない。
-->

## 引き渡し情報

<!--
実装を阻む未決事項がないこと、必要な成果物・制約・受け入れ条件が本文に揃っていることを確認する。
特定の起動コマンドや AADW 独自 state は前提にしない。
-->

実装を阻む未決事項: なし / <内容>
製品コードを変更する場合の実装担当: Claude Code
docs / metadata のみの場合: Commander が current facts と Commander Policy から既存の action を選ぶ
GUI validation の要否: Commander が変更内容と受け入れ条件から判断

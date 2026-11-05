# Issue #684: PR担当への実装引き継ぎ

## 準備状態

- 対象: [Issue #684](https://github.com/hide212131/hane/issues/684)、親 [#683](https://github.com/hide212131/hane/issues/683)。
- 基準main: `041b3d539ff82924864b9c3ad996695d07297ad6`。
- 詳細設計: [`docs/design/issue-684-verification-feedback.md`](../design/issue-684-verification-feedback.md)。
- 文書だけの実装用Draft PR。運用基盤のコード・testsは未実装、新経路・速度効果は未検証。
- 利用者の選好は「シンプルで速いと予想される方」。同一会話維持を必須にしない。
- 共有worktreeの既存 `crates/ui/src/view.rs` 変更は対象外。専用checkoutから作業する。

## 最初の一action

current main、PR head/base、Issue本文、同じ責務のPR/run、default branchのAGENTS/Policy/Claude実行手順を読む。
通常の意味判断は既存Jevで行い、一actionずつ進める。旧headの証拠を継続利用しない。
これは運用基盤専用PRであり、通常の製品workerへの `@claude` 実装依頼は投稿しない。
現行finalizerは `.github/` とPolicyを拒否するため、その入口では実装を提出できない。
U2専用PR #526の例外を転用せず、通常workerの制限を緩めない。

権限を持つPR担当が詳細設計§7の運用基盤担当として、限定した基盤コード・契約test・文書を直接変更できる。
製品コード変更は禁止のまま。実行・差分確認・commit/pushは担当がexpected-headを確認して実施する。
Claudeの補助編集は認証済み専用経路を利用できる場合だけの任意とする。
利用する場合もSonnet 5.5と編集専用toolsに限定し、GitHub/Jev認証とshell/提出権限を渡さない。
local CLIは存在するが `loggedIn=false` を確認済みであり、ログインを着手条件にしない。
通常の製品実装のClaude接続やCodex fallbackの条件は変更しない。

## 今回実装するもの

1. 既存verifyの固定workspace tests/Clippyについて、実exit・outcome・診断・SHA/出所をreportにする。
   テスト失敗後もlintを一度実行し、最後の合否を保持する。
2. 既存handoffへoptionalな `AADW_FEEDBACK_RUN_ID` / `AADW_FEEDBACK_RUN_ATTEMPT` を接続する。
   出所・attempt・H0/H1・PR・model確認・finalizer・重複依頼guardを検査する。
3. 元依頼とreportを `pr-head/` 外のRead可能な `feedback/` に渡し、新Claude起動から修正を続ける。
   H1に含まれるpatchを二重適用せず、checkpoint/patch指定と排他にする。
4. 詳細設計§6のallowlistだけを変更し、既存Policyと実行手順へ利用方法を整合させる。
   new helperは結果の生成・検査だけで、判断・再依頼・GitHub mutationをしない。
5. §8の異常・接続回帰を実装し、本番利用がdefault branch採用後であることを明記する。

対象外: Rust製品コード、vendor、GUI基盤、外部review担当、一般provider routing、
新しい常駐接続/workflow/DB、自動retry・merge、任意shell解禁、CI全体最適化、release。

## 必須の実装検証

少なくとも次を専用checkoutで実行する。新testは作成後に実行し、未存在のまま成功扱いにしない。

```text
python3 .github/tests/test_aadw_verification_feedback.py
python3 .github/tests/test_aadw_action_contract.py
python3 .github/tests/test_aadw_prevention_wiring.py
python3 .github/tests/test_aadw_patch_resume.py
python3 .github/tests/test_claude_model_verification.py
python3 .github/tests/test_codex_usage_limit_fallback.py
python3 .github/tests/test_rust_ci_path_filter.py
```

固定runnerを使うproducer→report→consumer→stageの接続を、実終了0/非0のprocessとAPI応答fixtureで確認する。
文字列上のworkflow配線検査だけを接続成功と称さない。
詳細設計§8.1のケースをtest名と観測へ対応付ける。
構文/差分検査、current-head CI、最終reviewも必要。ci.yml変更に伴う両OS CIを維持する。

## 実run確認とIssue受入

PRコメント起動はdefault branchのworkflowを使うので、準備branchだけでは新fieldは動作しない。
コードPRの採用後、§8.2の専用fixture PRで実際のClaude実装→失敗→結果返却→
Jev/Commanderのfresh判断→新Claudeの修正→成功を確認する。
その際のH0/H1/H2、run/attempt、model、source/追加comment、artifact、test/lint結果と
異常拒否の証拠を本PRへ残す。fixture PRは製品へmergeせず閉じる。
採用前の隔離接続検証と、採用後の本番経路実証を混同しない。

代表的な実修正のbefore/afterは§8.3の指標で比較する。
基準の #671 はverify終了→再依頼224秒。ただしこれを短縮の保証値にしない。
push/依頼数が同じなら同じと書き、残る起動・CI待ちを示す。
実run確認と実修正の効果が揃うまでIssue #684をcloseしない。`Refs #684` を使い自動closeを設定しない。

## 提出と受入の境界

開始時とpush直前にPR exact headが一致することを再確認する。不一致なら提出せずcurrent factsへ戻る。
current baseが進んだ場合は重なる責務・evidenceへの影響だけを判断し、全域の完了を待たない。
report成功だけでreview/GUI/mergeを起動しない。required evidenceの正本はCommander Policy。
新fieldの運用を止めれば従来の依頼が使える。code revertは本PR全体を単位にする。

設計方針の選択にはJev（provider `typesafe`、要求 `jev-latest`、応答 `jev-1.14.0`）を使用し、
`existing_restart`、具体化の次actionとして `prepare_design` を取得した。
local Claude未認証の新事実に対し、既存Policy内の運用基盤担当による着手を選ぶ `infra_operator` を取得した。
判断は実装成功や速度測定の証拠ではない。

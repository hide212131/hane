# AADW v3: Claude Codeへの実行指示

この文書は、[Commander Policy](aadw-command-policy.md)に従ってClaude Codeへ実装を依頼する操作手順である。設計判断は[ADR-0031](adr/0031-aadw-v3-jev-bounded-execution.md)、全体構成は[AADW v3設計書](agentic-development-workflow-v3.md)を正とする。

実装担当はClaude Code、通常モデルは **`claude-sonnet-5-5`** とする。通常実装の入口は **実装用PRのConversationに投稿するコメント** である。Issueへの `/implement`、設計用PRへの無断の実装依頼、Codexへの無条件な切替は使わない。

## 1. 最初に現在の事実を読む

Commanderは、current main、対象Issueの本文と受入条件、採用済み設計、関連PR、同じ対象の実行中runを確認する。過去の会話やローカルskillの記載だけで入口を推測しない。設計用Draft PRと実装用Draft PRは目的で区別する。

意味判断と実行権限の確認はCommander Policyに従う。以下の操作手順だけを理由に、未確定の設計を実装したり、終了していないworkerと同時に別のwriterを開始したりしない。

## 2. 実装用PRを準備する

同じIssue・同じフェーズの実装用PRがある場合は、そのPRを再利用する。別の実装用PRを重複して作らない。

実装用PRがない場合は、Commanderが次の準備を行う。**これは実行先の準備であり、製品コードの直接実装ではない。利用者にCodexの製品実装許可を追加で求めない。**

1. current mainの40桁SHAから、同じrepository内に作業branchを作る。例: `work/issue-304-p2`。同名branchが既にある場合は内容と関連PRを確認し、上書きしない。
2. `docs/implementation/issue-<番号>-<フェーズ>.md` などに、対象Issue、対象フェーズ、基準SHA、採用済み設計の場所、今回の範囲、対象外、未実装・未検証であることを書く。この文書差分だけを準備commitにする。空commitだけではPR作成に必要な差分にならない。
3. 作業branchをhead、mainをbaseとして、実装用Draft PRを作る。本文は「Claude実装待ちの準備PR」と明示し、Issueと設計を参照する。`Refs #<番号>` を使い、設計保存や一部フェーズのmergeで親Issueを自動closeさせない。

ここでは製品コード・テストを実装しない。既存の設計用Draft PRを無断で転用しない。repository書込み権限がない場合は、不足する権限と未実行の操作を報告する。製品実装の許可を求めても解決しない。

GitHub connectorを使う場合は、branch作成→文書の作成・commit→Draft PR作成の既存操作を使う。ローカルGitを使う場合も同じ内容に限り、実装準備のための新しいworkflowや常駐実行器は作らない。

## 3. Claudeへ一回だけ依頼する

投稿直前にPRのcurrent headを取り直す。default branch上の `claude-fix.yml` と `aadw_action_contract.py` が以下の契約を受け付けることを確認する。branch上だけの設定変更は、コメント起動workflowの反映完了ではない。

次のテンプレートの値を実際の値に置き換え、**実装用PRのConversation**へ投稿する。行頭の機械用項目は各一行とし、コードフェンスで囲まない。

@claude
AADW_COMMANDER_HANDOFF_V3
AADW_ACTION: implement
AADW_TARGET_HEAD: &lt;PRの40桁current head SHA&gt;
AADW_FAILURE_CLASS: initial
AADW_ROOT_CAUSE: issue-&lt;番号&gt;-&lt;フェーズ&gt;-initial
AADW_EVIDENCE: https://github.com/&lt;owner&gt;/&lt;repo&gt;/issues/&lt;番号&gt;

&#35;&#35; 今回の依頼
&lt;目的と今回の対象範囲。新規実装なら、その旨を明記する。&gt;

&#35;&#35; 読むもの
&lt;pr-head/から読める、採用済みの設計書と関連コードのパス。&gt;

&#35;&#35; 受入条件
&lt;今回のフェーズで満たす条件。Issue全体の完了条件と区別する。&gt;

&#35;&#35; 対象外
&lt;変更してはいけない責務、別フェーズ、他PRの範囲。&gt;

依頼本文で指定した範囲だけを実装してください。製品変更ではコードと直接関連するテストを、文書のみの運用確認では指定した文書だけを変更してください。
このworkerではshell、git、gh、test/build/lint実行、commit、pushは行わないでください。
実行できない検証を実施済みとせず、必要な検証を結果に書いてください。
次工程の起動、モデル切替、merge、Issue closeは行わないでください。

workerの使用可能な道具は `Read,Edit,Write,Glob,Grep` であり、GitHubのIssueやPRのURLへ自由にアクセスできるとは仮定しない。**URLだけを渡さず、実装に必要な決定事項と受入条件をコメントに含める。設計書は対象branchから読めるようにする。** 設計の採用に別PRのmergeが必要なら、先にその状態を確認する。

既存コードの修正では `AADW_FAILURE_CLASS` を、観測に対応する `product` または `test` にする。同じ原因には同じ `AADW_ROOT_CAUSE` を使い、原因を区別できるGitHub上の観測を `AADW_EVIDENCE` に指定する。再送を通すために原因IDやURLだけを付け替えない。

## 4. 投稿後に実行結果を確認する

コメント投稿成功はClaude起動成功ではない。Commanderは、投稿コメント、対象PR、対象SHAに対応する `AADW Claude Fix` のrunを確認する。

`prepare`が成功して初めて依頼が受理されたと扱う。`skipped`なら未開始であり、Issueへ投稿していないか、必要な行が正しいか、default branchの契約が反映済みかを確認する。`prepare`の失敗なら権限、head移動、closed/paused、fork、重複依頼などの具体的な理由を読む。原因を確かめず同じコメントを再送しない。

workerが成功した後も、実行結果が示すsession modelとmodel usage、patchの生成、trusted finalizerのpush、変更後のexact head、`verify`とrequired CIを別々に確認する。Sonnet 5.5の実行確認に失敗した場合はpatchをpushせず、利用上限fallbackにも流さない。workerはshellやGit認証情報を持たず、pushと検証はworkflow側が行う。空patch、検証失敗、モデル不一致、取得できない結果を完了としない。

モデル指定は `claude-sonnet-5-5` を使用する。runで確認できた実行モデルと設定上の指定を区別し、実行モデルが取得できない場合は「設定確認のみ」と記録する。Sonnet 5.5が使えない場合に、旧モデルへ黙って切り替えない。

## 5. 診断、利用上限、完了を区別する

原因が `measurement`、`environment`、`unknown` のときは製品修正へ送らず、Commander Policyの診断用契約と `aadw-diagnose.yml` を使う。診断完了は製品の受入完了ではない。通常診断もSonnet 5.5を使う。

Claudeの失敗だけでCodexへ切り替えない。`usage_or_rate_limit` と確認され、既存のfallback契約・権限・source run/attempt・checkpoint・exact-head条件を満たす場合だけ、既存の代替実装経路を使う。認証・権限・環境・model/providerの失敗は、利用上限とは区別する。

実装と必要なCIが揃ってからDraftを解除し、CodeRabbitのレビューを確認する。最終候補ではcurrent-head CI成功後のmanual full reviewを確認する。必要なGUI検証、current head/base、mergeabilityもCommander Policyに従って確認し、expected headを指定してmergeする。フェーズPRのmergeだけで親Issueをcloseしない。

## 運用基盤そのものを変更する場合

`.github/`、`AGENTS.md`、`CLAUDE.md`、Commander Policyなどの保護パスを、通常の製品workerに変更させない。利用者が明示した運用基盤専用PRとして、権限を持つ担当が変更・検証する。通常workerの制限を緩めて通すことはしない。

ローカルの同名skillがこの手順と矛盾する場合、default branch上の正本を確認する。repository内のskillを更新しても、別PCのユーザー領域に置かれたskillまで更新したとは報告しない。

## モデル名の出典

Anthropic, Introducing Claude Sonnet 5.5（2026-09-28）:
https://www.anthropic.com/claude-sonnet-5-5

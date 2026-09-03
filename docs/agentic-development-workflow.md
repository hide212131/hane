# AI Agent Development Workflow

## 目的

Hane の Issue から実装、レビュー、実アプリ検証、修正、マージ判断までを、複数の coding agent と検証層に役割を分けて自動化する。

この文書を運用設計の正本とする。役割分担そのものを採用する理由は
[ADR-0023](adr/0023-ai-agent-development-workflow.md) に残す。

追跡 Issue: #44

## 基本方針

1つの agent に実装、レビュー、実アプリ検証、進行判断をすべて任せない。

- **Claude Code** は実装を担当する。
- **Codex** は Pull Request のコードレビューを担当する。
- **Local GUI validator** はローカル macOS 上で Hane を起動し、実アプリの GUI 挙動を検証する。
- **GitHub Copilot** は Codex の指摘、GUI validation、Pull Request、CI の状態を読み、次の処理を判断する。
- **GitHub Actions** はトリガー、権限、状態遷移、最終的な機械的チェックを担当する。

ADR-0023 の基本判断である **Claude = implementer / Codex = reviewer / Copilot = judge** は変更しない。Local GUI validator はこの3者を置き換えず、コードレビューや CI では確認しにくい実アプリの挙動を補完する独立した検証層とする。

AI の判断と、GitHub 上で実際に変更を加える処理を分ける。特にマージは Copilot の判断だけでは実行せず、CI、Codex review、GUI validation、対象 commit、未解決レビューなどを機械的に確認する。

## 全体フロー

```text
Issue
  |
  v
Claude Code
  |  実装・テスト・commit
  v
Pull Request
  |
  v
CI + Codex review
  |
  +-- CI fail -----------------> atomic transition + durable outbox(PR, SHA, ci-failure)
  |                               |
  |                               +--> retryable dispatcher --> Copilot pre-GUI routing
  |                                                       +-- fix --> Claude Code fix --> push --> 全検証やり直し
  |                                                       +-- blocked --> human
  |
  +-- CI pass + Codex に修正候補あり --> Copilot pre-GUI routing
  |                                      +-- fix --> Claude Code fix --> push --> 全検証やり直し
  |                                      +-- blocked --> human
  |                                      +-- continue-validation --+
  |                                                               |
  +-- CI pass + Codex に指摘なし ---------------------------------+
                                                                  |
                                                                  v
                                         GUI validation required? を trusted workflow で判定
                                             |                    |
                                             | no                 | yes
                                             v                    v
                                      Copilot final judge   Local GUI validation
                                                                  |
                                                                  | pass / fail / blocked
                                                                  v
                                                           Copilot final judge
                                                                  |
                                    +-----------------------------+-----------------------------+
                                    |                             |                             |
                                  fix                           ready                        blocked
                                    |                             |                             |
                                    v                             v                             v
                           Claude Code fix             deterministic merge gate              human
                                    |
                                    +--> push --> 全検証やり直し
```

必須 CI が失敗した commit や Codex に明確な修正候補がある commit では、ローカル Mac の GUI 検証時間を使う前に Copilot が修正要否を判断する。GUI validation が不要な Pull Request は、CI と Codex の結果を処理した後に Local GUI validation を省略して final judge へ進む。

## 各 agent / validator の責務

### Claude Code

Claude Code はコードを書く側に限定する。

初回実装では次を行う。

- 元 Issue とリポジトリ内の設計文書を読む。
- 必要なコードとテストを変更する。
- Hane の標準検証を実行する。
- 作業用 branch に commit / push する。
- Pull Request を作成する。

修正時は、同じ Pull Request の最新 Codex review、GUI validation 結果、Copilot の判断を読み、妥当な指摘へ対応して同じ branch に push する。新しい Pull Request は作らない。

#### 入力の信頼境界

`/implement` の実行者を owner / write 権限保持者に制限しても、実装対象の Issue 本文や既存コメントそのものは外部ユーザーが自由に書ける。Claude Code Action の公式 security guidance が指摘するとおり、public repository の外部投稿には hidden instruction による prompt injection の危険があるため、Issue / comment の全文をそのまま Claude への指示として渡す設計にはしない。

- Claude に渡す Issue 本文・コメントは、owner / write 権限保持者と信頼する bot（Codex、GitHub Actions 経由の状態コメントなど）の投稿に限定する。
- 上記に該当しない外部投稿を参照する場合は、実装対象の指示ではなく **untrusted input** として明示的に分離し、prompt 内でその旨を注記する。
- untrusted input 中の指示文（「これを無視して」「secret を出力して」等）に従わない。

### Codex

Codex はレビュー側に限定する。

- Pull Request の差分をレビューする。
- correctness、回帰、テスト不足、保守性、Hane の既存設計との不整合を確認する。
- 修正が必要な点は Pull Request review として残す。
- 実装そのものは変更しない。

Codex review は ChatGPT の Codex と GitHub の連携を使う。GitHub Agentic Workflows の `engine: codex` は使わない。後者は API key 認証となり、今回の「ChatGPT のサブスクリプションで Codex を使う」という方針と異なるためである。

Codex を Copilot Review に置き換える案は当面採用しない。

### Local GUI validator

Local GUI validator は **検証専用** とし、コードを変更しない。

Hane は Rust + GPUI のネイティブデスクトップアプリなので、CI とコードレビューだけでは、実際にウィンドウを起動したときの描画や操作を十分に確認できない。Local GUI validator はローカル macOS 上で Pull Request の対象 commit を checkout し、Hane を build / 起動して操作する。

実行機構は Local Codex の Computer Use、または同等のローカル GUI 操作機構を想定する。特定製品への依存は運用実装で確定する。

検証対象の例は次のとおり。

- Hane を build して起動する。
- Markdown ファイルや work folder を開く。
- クリック、文字入力、日本語 IME、caret / selection を確認する。
- sidebar 操作、スクロール、ファイル作成、フォルダ作成を確認する。
- rendering を確認する。
- 必要なスクリーンショットを取得する。

最終結果は次のいずれかとする。

- `pass`: 必須シナリオを実行でき、期待する挙動を確認できた。
- `fail`: 実行できたが、期待する挙動を満たさなかった。
- `blocked`: runner、OS、権限、環境依存などにより、必要な検証を完了できなかった。

`blocked` は成功扱いにしない。`pass` / `fail` / `blocked` はすべて終端結果として Copilot final judge を起動する。

#### GUI validation を必要とする Pull Request

初期実装では Pull Request label `gui-validation-required` を使う。ただし、このラベルだけを「GUI validation が必要か」の正本にはしない。

UI、interaction、rendering、日本語 IME、scroll、file dialog など、実アプリの操作や描画に影響する変更では GUI validation を必須とする。documentation、CI-only など、GUI に影響しない変更では不要としてよい。

`gui-validation-required` は人間または trusted workflow が GUI validation を **強制的に必要とする入力** として扱う。ラベルがなくても GUI validation を省略できるとは限らない。

trusted workflow は Pull Request の各 head SHA について、変更ファイルとラベルから `GUI validation required?` を機械的に判定し、その判定を head SHA とともに保存する。初期ポリシーは fail closed とし、次の順で判定する。

1. `gui-validation-required` が付いていれば `required = true`。
2. 変更ファイルがすべて明示的な no-GUI allowlist（例: documentation / CI-only）に入る場合だけ `required = false`。
3. それ以外、または分類できない場合は `required = true`。

このため、ラベルを付け忘れたり後から外したりするだけでは GUI validation を回避できない。head SHA が変わった場合は分類もやり直す。merge gate は保存済みの分類対象 SHA が現在の head SHA と一致することを確認し、必要に応じて同じ決定規則を再評価して不一致を拒否する。

#### GUI validation の実行順序

ローカル Mac の実行時間を無駄にしないため、GUI validation は次を満たした後に実行する。

1. 対象 head SHA の必須 CI が成功している。
2. 同じ head SHA に対する Codex review が完了している。
3. Codex に修正候補がある場合は、Copilot pre-GUI routing が `continue-validation` と判断している。

必須 CI が終端的に失敗した commit は Local GUI validation を待たず、Copilot pre-GUI routing に送る。ただし、並列 CI の複数ジョブが同じ head SHA で失敗しても同じ routing request は一度だけ作成し、配送失敗時は再試行できるようにする。CI が失敗している commit、または Codex の指摘に対して Copilot が `fix` / `blocked` と判断した commit では GUI validation を走らせない。

### GitHub Copilot

Copilot は進行判断を担当する。

GitHub Agentic Workflows の `engine: copilot` を使い、次を入力として判断する。

- 元 Issue
- Pull Request の差分
- 現在の head SHA
- Codex の最新 review と review comment
- Codex review が指摘なしか、修正候補ありか
- GUI validation が必要か、その分類対象 SHA
- GUI validation の対象 SHA と結果
- 未解決 review thread
- CI / status check の結果
- 過去の agent loop の状態

Copilot は同じ judge の責務を、必要に応じて2つの checkpoint で使う。

#### pre-GUI routing

次のいずれかの場合、GUI validation より先に起動する。

- 対象 head SHA の必須 CI が終端的に失敗した。
- Codex が submitted review として修正候補を残した。

CI が失敗した場合は次のいずれかを返す。

1. `fix`: Claude Code の修正 workflow を起動する。
2. `blocked`: 人間の確認が必要な状態として停止する。

CI が成功していて Codex に修正候補がある場合は次のいずれかを返す。

1. `fix`: Claude Code の修正 workflow を起動する。
2. `continue-validation`: Codex の指摘はこの commit の GUI validation を止める理由ではないと判断し、次の検証へ進める。
3. `blocked`: 人間の確認が必要な状態として停止する。

CI failure に対して `continue-validation` は返さない。pre-GUI routing から直接 `ready-to-merge` にも進まない。

#### final judge

GUI validation が不要な場合、または GUI validation が終端結果 `pass` / `fail` / `blocked` を返した場合に起動する。出力は次の3つを基本とする。

1. `fix`: Claude Code の修正 workflow を起動する。
2. `ready`: マージ条件を検査する deterministic workflow を起動する。
3. `blocked`: 人間の確認が必要な状態として停止する。

判断できない場合は `ready` にしない。人間が確認できるコメントを残して停止する。

GUI validation が必須な Pull Request では、GUI result が `pass` で、GUI-validated SHA が現在の head SHA と一致する場合だけ `ready` を許可する。`fail` / `blocked` でも final judge 自体は必ず起動し、`fix` または `blocked` へ進める。未実施または stale の場合も `ready` に進めない。

## GitHub Actions の責務

Agent は判断するが、GitHub Actions が制御する。

Actions 側では最低限、次を保証する。

- 同じ event を二重処理しない。
- event 単位の重複排除だけでなく、`Pull Request 番号 + head SHA + transition kind` 単位で routing request を原子的に一度だけ作成する。
- CI、Codex review、GUI requirement classification、GUI validation、Copilot judge の対象 head SHA と現在の head SHA が一致することを確認する。
- 必須 CI の終端失敗を検出した場合は、状態を `waiting-judge` にする変更と `(PR, head SHA, ci-failure)` routing request の durable outbox 登録を同じ原子的更新で保存する。
- outbox dispatcher は pending request を lease して配送する。dispatcher が cancel / crash した場合や dispatch が失敗した場合は、lease timeout 後に同じ request を別の実行が再取得して再試行できる。
- dispatch API が成功した直後に応答を失うケースでは同じ request が再配送され得るため、受信側の pre-GUI routing workflow も stable transition ID をキーに原子的に実行権を取得し、同じ transition ID の judge / Claude fix を二重実行しない。
- dispatcher は dispatch API 成功を確認した後だけ request を `dispatched` とし、受信側が処理完了を記録したら `processed` とする。一定回数の配送失敗や処理タイムアウト後は fail closed で `blocked` にして人間へ通知する。
- 同じ head SHA について既存の pending / leased / dispatched / processed request がある別の CI failure event は新規 request を作らない。ただし既存 request が pending、lease expired、または未処理なら recovery worker が配送・再配送を継続できる。
- 修正後は新しい head SHA に対して必要な検証と GUI requirement classification をすべてやり直す。
- 古い SHA に対する review / classification / validation / judge / routing request を再利用しない。
- 最大反復回数を設ける。
- 認証情報を prompt やログへ渡さない。
- merge 前に Hane の必須チェックを再確認する。

## head SHA を中心にした不変条件

すべての検証結果、GUI requirement classification、workflow routing request は Pull Request の head SHA に紐づける。

例として Pull Request head SHA が `A` のとき、次の結果だけを `A` の判定材料にできる。

```text
PR head SHA = A
  |
  +-- CI(A)
  +-- Codex review(A)
  +-- GUI requirement classification(A)
  +-- GUI validation(A)  # required の場合
  +-- Copilot judge(A)
  +-- routing requests(A)
```

途中で commit `B` が push された場合、`A` に対する Codex review、GUI requirement classification、GUI validation、Copilot judge、routing request は現在の判定には使わない。必要な処理を `B` に対してやり直す。

この SHA 一致確認は agent の文章判断に任せず、workflow 側でも機械的に確認する。

## 認証

### Claude Code

Claude Code Action を直接使う。Claude Pro / Max のサブスクリプションを使う場合は、ローカルで
`claude setup-token` を実行して OAuth token を生成し、GitHub Actions Secret
`CLAUDE_CODE_OAUTH_TOKEN` に保存する。

workflow からは `anthropics/claude-code-action` の `claude_code_oauth_token` に Secret を渡す。

GitHub Agentic Workflows の Claude engine は、この OAuth token を使う方式とは別物であり、今回の Claude 実装経路には使わない。

### Codex

ChatGPT アカウントで Codex にサインインし、Codex Web / GitHub 側で Hane リポジトリを接続する。
Pull Request review はこの接続を使う。

今回の Codex review では `OPENAI_API_KEY` を GitHub Actions に置かない。

### Local GUI validator

Local GUI validator はローカル macOS 上で動かす。GitHub から対象 Pull Request と head SHA を受け取り、その SHA を checkout して検証する。

ローカル runner には、GUI validation に必要な最小限の GitHub 読み取り／結果報告権限だけを与える。個人用の SSH agent、不要な cloud credential、個人データへアクセスできる前提にはしない。

### GitHub Copilot

Hane は個人所有リポジトリなので、GitHub Agentic Workflows の Copilot inference には
`COPILOT_GITHUB_TOKEN` を使う。これは Copilot Requests 権限を持つ fine-grained Personal Access Token とし、GitHub Actions Secret に保存する。

`GITHUB_TOKEN` に `copilot-requests: write` を与えて組織課金する方式は、組織所有リポジトリ向けなので今回の前提にはしない。

## 状態管理

無限ループと古いレビューの再処理を避けるため、状態は Pull Request ごとに管理する。

最低限、次を保持する。

- 対象 Pull Request 番号
- current head SHA
- Codex-reviewed SHA
- Codex outcome: `clean` / `findings`
- GUI-classified SHA
- GUI validation required?
- GUI requirement classification source / policy version
- GUI-validated SHA
- GUI result: `pass` / `fail` / `blocked`
- Copilot-judged SHA
- routing requests: stable transition ID、head SHA、kind、delivery state、lease owner / expiry、attempt count
- processed transition IDs: judge / worker の冪等化記録
- 現在の状態
- fix iteration count

状態候補は次のとおり。

```text
implementing
waiting-codex
waiting-gui
waiting-judge
fix-requested
ready-to-merge
blocked
merged
```

pre-GUI routing 中も `waiting-judge` を使う。CI failure または Codex findings に対して `fix` / `blocked` なら GUI へ進まない。Codex findings に対する判定結果が `continue-validation` の場合だけ、GUI requirement classification の結果に応じて `waiting-gui` または final judge へ進む。

状態遷移は読み取り後の無条件書き込みではなく、期待する current state / head SHA を条件にした compare-and-set 相当で行う。CI failure では、同じ `(PR, head SHA, ci-failure)` routing request が存在しない場合だけ、`waiting-judge` への遷移と outbox request 作成を原子的に行う。並列イベントが同時に到着した場合、request を作成できなかった側は新規 dispatch をせず、既存 request の配送状態を尊重する。

routing request の配送状態は少なくとも `pending` / `leased` / `dispatched` / `processed` / `delivery-failed` を持つ。`leased` は期限付きとし、dispatcher が停止した場合は lease expiry 後に再取得可能にする。受信側は stable transition ID を処理済みか原子的に確認してから judge や worker を起動し、重複配送を no-op にする。

実装時には、原子的更新または排他が可能な永続領域を状態と durable outbox の正本として使う。機械可読な Pull Request comment を表示用に併用してよいが、競合制御ができない comment の単純な read-modify-write だけを状態や routing request の正本にはしない。ラベルは人間向けの表示や GUI validation required の強制指定に使ってよいが、状態や GUI requirement classification の正本にはしない。

## トリガー

初期案では次のイベントを使う。

### 実装開始

Issue 上の明示的なコマンドで始める。

```text
/implement
```

Issue 作成だけでは自動実装を始めない。誤作動と意図しないコスト消費を防ぐためである。

Hane は公開リポジトリであり、`/implement` コメントの文字列だけを起動条件にすると、任意の第三者が Claude の実行枠と書き込み権限を起動できてしまう。dispatch 前に、コメント投稿者の実効権限を検証し、owner / write 権限保持者以外からの `/implement` は無視する。これを不変条件とする。

`author_association` の `MEMBER` は組織所属を示すだけで、そのリポジトリへの write 権限を保証しない。collaborator に read／triage のみを与えることもできるため、`author_association` を write 権限の代用にはしない。代わりに `GET /repos/{owner}/{repo}/collaborators/{username}/permission` などで現在の実効権限を取得し、`write` / `maintain` / `admin` の場合のみ許可する。Hane は個人所有リポジトリのため、`author_association` が `OWNER` の場合を明示的に許可する最適化は行ってよい。

### CI

必須 CI は現在の head SHA に対して評価する。

- 必須 CI が成功した場合だけ GUI validation へ進める。
- 必須 CI が `failure` などの終端失敗になった場合は GUI validation を起動せず、Copilot pre-GUI routing 用の durable outbox request を作成する。
- matrix の複数ジョブが同じ head SHA で失敗する可能性があるため、各失敗イベントはまず `(PR, head SHA, ci-failure)` request の作成を原子的に試みる。
- request を最初に作成した処理だけが `waiting-judge` への状態遷移も同時に保存する。後続イベントは同じ request を再作成しない。
- dispatcher は pending request を期限付き lease で取得し、pre-GUI routing を dispatch する。dispatch に失敗した場合や dispatcher が停止した場合は再試行する。
- dispatch 成功後の応答喪失による重複配送に備え、受信 workflow は stable transition ID で冪等化する。
- pre-GUI routing は CI failure に対して `fix` または `blocked` を返し、`continue-validation` / `ready` は許可しない。
- CI がまだ実行中の場合は次へ進まない。
- head SHA が変わった場合は古い CI 結果や routing request を使わない。

### Codex review

Claude Code が Pull Request を作成したとき、または修正 commit を push したときに、その最新 head SHA を対象としてレビューする。

Codex の GitHub integration による自動 review は新規 Pull Request 作成時のみ保証される公式仕様であり、修正 push 後の再レビューは保証されない。また指摘がない場合は review 本文なしの 👍 reaction のみになることがある。そのため、修正 push 後の再レビューは Actions から `@codex review` コメントを明示的に投稿する方式で行い、次の契約を満たす。

- Actions が `@codex review` を投稿する際、投稿コメント（またはそれに紐づく Pull Request comment）に対象 head SHA を機械可読な形で記録する。
- Codex の review／reaction を、その記録した head SHA に対応付けて状態管理に保存する。
- Codex は手動レビューの実行中にリクエストコメントへ 👀 reaction を付け、その後にレビューを投稿する。この 👀 は実行中（running）を示すだけであり、完了とはみなさない。
- Codex review の完了は、記録した head SHA を対象とする submitted review、または指摘なしを示す終端 reaction（👍）のいずれかが観測できた場合のみ true とする。👀 のみの状態では次へ進まない。
- 👍 の場合は Codex outcome を `clean` とする。submitted review があり、同じ review に inline comment / suggestion がある場合は `findings` とする。
- `findings` の場合は Local GUI validation を直接起動せず、まず Copilot pre-GUI routing を起動する。
- 一定時間内に上記の完了イベントを観測できない、または head SHA の対応付けが判定できない場合は「未完了」として扱い、fail closed で `ready` に進めない。

### GUI requirement classification

Codex review の結果を処理した後、trusted workflow が現在の head SHA に対して GUI validation の要否を判定する。

- `gui-validation-required` は `required = true` を強制する入力とする。
- ラベルがない場合でも、変更ファイルが no-GUI allowlist だけであることを確認できない限り `required = true` とする。
- 判定結果、対象 head SHA、判定に使った policy version を状態管理に保存する。
- head SHA が変わったら判定を無効化してやり直す。

### Local GUI validation

GUI requirement classification が `required = true` で、対象 head SHA の CI が成功し、Codex outcome が `clean` または Copilot pre-GUI routing が `continue-validation` の場合、状態を `waiting-gui` にして Local GUI runner に検証を要求する。

runner は要求に含まれる Pull Request 番号と head SHA を使い、その SHA を checkout して Hane を build / 起動する。検証結果には少なくとも次を含める。

- Pull Request 番号
- validated head SHA
- result: `pass` / `fail` / `blocked`
- 実行したシナリオの識別子または概要
- 必要に応じてスクリーンショット等の証跡

結果受領時に現在の Pull Request head SHA と一致しなければ、その GUI validation は stale として破棄する。

`pass` / `fail` / `blocked` のいずれを受け取っても final judge を起動する。`fail` / `blocked` は `ready` の条件を満たさないが、`fix` または人間への `blocked` に進むため judge まで処理する。

### Copilot judge

対象 head SHA の必須 CI が失敗した場合は、GUI validation より前に pre-GUI routing を起動する。起動要求は durable outbox から配送され、dispatcher の失敗時には再試行する。受信側は stable transition ID で冪等化する。

CI が成功し、Codex outcome が `findings` の場合も、GUI validation より前に pre-GUI routing を起動する。

GUI validation が不要な Pull Request は、Codex review、成功した CI、GUI requirement classification が同じ head SHA で揃い、必要な pre-GUI routing が済んだ後に final judge を起動する。

GUI validation が必要な Pull Request は、同じ head SHA に対する GUI validation が `pass` / `fail` / `blocked` のいずれかの終端結果になった後に final judge を起動する。

GitHub Agentic Workflows から許可する書き込みは必要最小限にする。Claude 修正 workflow を起動する場合は allowlist した `dispatch-workflow` を使う。

## 修正ループ

Copilot が `fix` と判断した場合は、Claude Code に次の情報を渡す。

- Pull Request 番号
- 修正対象の head SHA
- Codex review / comment
- GUI validation 結果がある場合はその結果
- Copilot が修正必要と判断した理由

Claude は修正、検証、push まで行う。push 後は以前の Codex review、GUI requirement classification、GUI validation、Copilot judge、routing request を再利用せず、新しい head SHA に対して必要な検証をすべてやり直す。

最大反復回数の初期値は **3回** とする。3回で収束しない場合は `blocked` とし、人間へ引き継ぐ。

## マージ条件

Copilot の `ready` は「マージしてよい」という最終権限ではなく、機械的なマージ判定へ進める合図とする。

少なくとも次をすべて満たした場合だけマージする。

- Copilot final judge が最新 head SHA に対して `ready` と判断している。
- Codex review の対象 SHA が最新 head SHA と一致する。
- 必須 CI が最新 head SHA で成功している。
- GUI-classified SHA が最新 head SHA と一致する。
- merge gate が GUI requirement classification を同じ決定規則で再確認し、保存済みの `GUI validation required?` と矛盾しない。
- GUI validation required の場合、GUI result が `pass` である。
- GUI validation required の場合、GUI-validated SHA が最新 head SHA と一致する。
- Pull Request が draft ではない。
- conflict がない。
- 未解決の blocking review thread がない。
- 判定後に head SHA が変わっていない。

GUI validation が必須なのに未実施、`fail`、`blocked`、stale、または GUI requirement classification 自体が missing / stale / inconsistent の場合は fail closed とし、`ready-to-merge` に進めない。

Hane の標準検証は README に従い、少なくとも次を必須とする。

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

現在は `pull_request` トリガーで上記の `cargo test` / `cargo clippy` を実行する CI workflow と、main branch の ruleset による required status checks が設定済みである。merge gate が要求する check name は明示的なリストとして定義し、空リストを許可しない。

- 要求した check が0件、`missing`、`skipped`、または結果が取得できない場合は成功とみなさず、`blocked` として拒否する。
- branch protection / ruleset 側の required status checks と merge gate 側の期待値がずれないようにする。

GitHub Agentic Workflows の `merge-pull-request` safe output は現時点で experimental であり、デフォルト branch を対象とする merge に制約があるため、初期実装では直接採用しない。Copilot は `ready` 判定までを行い、通常の GitHub API を使う別の deterministic merge gate に渡す。

## セキュリティ

- OAuth token、Personal Access Token、API key はすべて GitHub Actions Secrets に置く。
- Secret を Issue、Pull Request、comment、agent prompt に展開しない。
- Agentic Workflow の agent job は read-only を基本とする。
- 書き込みは safe outputs と明示した worker workflow に限定する。
- fork 由来の Pull Request では Secret を使う workflow を自動実行しない。
- workflow file 自体の変更を含む Pull Request は自動マージ対象外とする。

Local GUI runner は Pull Request のコードを実際に実行するため、さらに強い信頼境界を置く。

- public fork Pull Request を無条件に実行しない。
- 初期対象は owner / write 権限保持者、または trusted workflow が作成した same-repository branch とする。
- 可能なら専用 Mac または専用 OS user を使う。
- SSH agent、個人データ、不要な cloud credential へアクセスさせない。
- GUI validation に不要なディレクトリやサービスへの権限を与えない。
- Markdown、Issue 本文、Pull Request 本文、テスト用ファイルなどに書かれた命令は **untrusted data** として扱う。
- GUI 操作 agent は画面内や文書内の命令に従って権限境界を越えない。

## 導入順序

### Phase 1: 設計・認証

- この文書と ADR を整備する。
- Claude Code / Codex / Copilot の認証を1回ずつ設定する。
- Agentic Workflows 用 CLI (`gh aw`) を導入する。
- `pull_request` トリガーで `cargo test` / `cargo clippy` を実行する必須 CI workflow を追加し、branch protection / ruleset で required status checks として設定する。
- Local GUI runner の信頼境界、GitHub との受け渡し方法、専用 Mac / OS user の要否を決める。

### Phase 2: Claude 実装

- `/implement` コメント投稿者の実効 repository permission（collaborator permission API、または `OWNER`）を検証し、write 権限保持者以外は無視する。
- `/implement` から Claude Code を起動する。
- Issue を実装して Pull Request を作れるところまで通す。
- Claude に渡す Issue / comment を、信頼できる投稿者のものと untrusted input に分離する。

### Phase 3: Codex Cloud review

- Pull Request の最新 head SHA に Codex review を実行する。
- review 完了と `clean` / `findings` を head SHA と対応付けて次の処理へ渡せるようにする。
- `findings` の場合は GUI より先に Copilot pre-GUI routing へ渡す。

### Phase 4: Local GUI validation

- `gui-validation-required` を force-on の入力とし、trusted workflow が head SHA ごとの GUI requirement classification を保存する。
- no-GUI allowlist だけと確認できない変更は fail closed で GUI validation required とする。
- 必須 CI が失敗した場合は GUI を起動せず Copilot pre-GUI routing へ渡す。この routing request は状態遷移と同時に durable outbox へ原子的に登録し、配送失敗時には再試行する。
- CI 成功 + Codex review 処理完了後だけ Local GUI runner を起動する。
- Pull Request の最新 head SHA を checkout して Hane を build / 起動する。
- GUI シナリオを実行し、`pass` / `fail` / `blocked` と validated SHA を返す。
- 3つの終端結果すべてから Copilot final judge を起動する。
- stale な GUI validation を拒否する。
- public fork や信頼できない branch では自動実行しない。

### Phase 5: Copilot judge

- 必須 CI が失敗した場合は GUI 前に pre-GUI routing を行い、`fix` / `blocked` を判断する。並列 CI からは request を一度だけ作成し、dispatcher failure は lease + retry で回復する。重複配送は stable transition ID で受信側が no-op にする。
- Codex に修正候補がある場合も GUI 前に pre-GUI routing を行い、`fix` / `continue-validation` / `blocked` を判断する。
- final judge では Codex review、GUI validation、CI、Pull Request を読み、`fix` / `ready` / `blocked` を判断する。
- `fix` なら Claude 修正 workflow を dispatch する。
- GUI validation が必要な Pull Request では、同じ head SHA の `pass` がなければ `ready` にしない。

### Phase 6: deterministic merge gate

- SHA、CI、Codex review、GUI requirement classification、GUI validation、review thread、mergeability を機械的に確認する。
- GUI requirement classification を再確認し、ラベル操作だけで GUI validation を回避できないようにする。
- 条件を満たした Pull Request だけを squash merge する。
- 失敗時は自動マージせず `blocked` にする。

## 参考

- GitHub Agentic Workflows: https://docs.github.com/en/copilot/concepts/agents/about-github-agentic-workflows
- Agentic Workflows authentication: https://github.github.com/gh-aw/reference/auth/
- Agentic Workflows safe outputs: https://github.github.com/gh-aw/reference/safe-outputs/
- Claude Code Action setup: https://github.com/anthropics/claude-code-action/blob/main/docs/setup.md
- Codex with a ChatGPT plan: https://help.openai.com/en/articles/11369540-using-codex-with-your-chatgpt-plan

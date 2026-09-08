# GUI validation 設計

2026-09-09 更新。GitHub-hosted macOS を優先し、不足が実証された操作だけをローカルで補う。[ADR-0024](adr/0024-local-gui-validation.md) が採用理由、[全体の開発ワークフロー](agentic-development-workflow.md) が役割・進行条件の正本である。文書名は既存リンクとの互換のため維持する。

GUI受領処理と [final judge / merge gate](final-judgement.md) の実装を追加した。実装の追加と、実際の自動マージまでの動作実証は区別する。実証と実装の進捗は [Issue #44](https://github.com/hide212131/hane/issues/44) と [#55](https://github.com/hide212131/hane/issues/55) で追跡する。

## 実証済みの範囲

[run 34254547035](https://github.com/hide212131/hane/actions/runs/34254547035) は標準 `macos-15`（macOS 15.7.9 / ARM64）で、Hane SHA `0f5e2b1f0ffb41a248b9a806009de273e8e8d793` の clean checkout、build、launch、対象 PID の window 取得、撮影、終了に成功した。見出しと日本語本文の描画も画像で確認した。[保存した結果・環境・画像](history/gui-validation/2026-09-09-hosted-launch/README.md)を参照する。

続く [run 34256439460](https://github.com/hide212131/hane/actions/runs/34256439460) では同じ Hane SHA に対する System Events 経由の ASCII 入力・保存内容照合・redo が成功した。undo のテストは選択置換と後続入力を単一履歴と誤って想定しており、日本語 IME は親入力方式が無効な状態での選択が `-50` となった。全体結果は `fail`、IME シナリオは `blocked` であり、成功結果に書き換えない。修正した手順による再実行を別の世代として記録する。

[run 34257026535](https://github.com/hide212131/hane/actions/runs/34257026535) は Hane SHA `d85af7f1d86f4dc05ec44e790d23ba63df05af72`、手順 SHA `c264aac84fab0374ec48f33eb099ab849ad4253f` で ASCII 入力・保存・独立した編集のundo/redo・再オープン、および日本語IMEの `nihongo` → `日本語` 変換・確定・保存がすべてpassとなった。再オープンと日本語保存後の画像も確認した。親入力方式の有効化は使い捨てhosted環境だけで行い、元の入力方式への復元もpass。続く [run 34257563560](https://github.com/hide212131/hane/actions/runs/34257563560)（同じHane SHA、手順 `96178aaff00eefeb43e85f3e109634c5fe51969c`）ではOSホイール操作によって表示が LINE 1–11 から LINE 13–23 に移り、文書が変わらないことも確認した。再オープン後の保存文字列は画面OCRでも確認した。

ローカルの通常ログインユーザーでも editor / cursor-boundary / cursor-scroll の起動・撮影は先行実証済みだが、cursor 系は instrument による内部状態の設定であり、OSのホイール操作やIME入力の証拠ではない。standalone Computer Use の過去の承認問題を、現在のHane専用コマンドの必須条件にはしない。

## 実行経路

```text
exact head SHA の CI / review / GUI要否分類
    -> findings があれば既存の Copilot pre-GUI routing
    -> 必要な GUI を GitHub-hosted macOS で実行
    -> SHA・手順の版・世代・実行元・証拠を照合
    -> pass / fail / blocked の全結果を Copilot final judge へ
    -> ready の場合だけ deterministic merge gate
```

GUI が不要な場合も、同じ SHA の CI・review・要否分類・必要な pre-GUI routing を確認した後で final judge へ進む。GUI不要というラベルやユーザー本文の自己申告で省略しない。head が変われば旧結果を現在判定に使わない。

役割は **Claude = implementer / Codex = reviewer / Copilot = judge** を維持する。GUI validator はアプリを検証して証拠を返し、コード修正やマージは行わない。Codex usage-limit 時だけの既存 Copilot review fallback も変更しない。

## 実行対象と権限

- GitHub API で open・non-draft・same-repository の PR と最新 head SHA を取得する。既存 controller が許可する投稿者・作成経路だけを対象にする。
- public fork、未知の作成者、古い SHA、未知の手順、期限切れの依頼は実行しない。Issue / PR / review 本文を実行命令や許可の正本にはしない。
- 実行対象のコードには、書き込みトークン、GitHub App の秘密鍵、Copilot / Claude の認証情報を渡さない。checkout は `persist-credentials: false` とする。
- 検証手順は信頼した control SHA、Hane は対象 SHA として別々に取得する。対象 PR が変更した手順を、その PR 自身の合格を決める手順として自動採用しない。
- 結果受領・status投稿・judge・merge は、それぞれに必要な権限を持つ別の信頼する処理で行う。

hosted は使い捨て環境と個人データの分離に役立つが、悪意ある対象コードが同じジョブ内の証拠を改変できないという保証ではない。信頼する作成者への限定は維持する。TCC DB、安全設定、既存セッションの承認を緩めて成功させることを運用前提にしない。

## シナリオと合否

起動・画像生成だけでは入力や保存を合格にしない。まず限定した基本操作を再現可能なシナリオとして固定し、対象機能と確認方法を結果に明記する。

| 機能 | 確認方法 |
| --- | --- |
| 起動・終了 | clean checkout / binary digest / ready / 対象PIDのwindow / 対象プロセスの終了 |
| 描画 | 対象windowの画像と期待する文書の表示。画像生成だけを表示内容の自動判定と呼ばない |
| ASCII入力・保存 | OS経由で既知の文字列を入力し、保存ファイルのbyte列を照合 |
| undo / redo | 明確に分離した編集を戻す・やり直す操作と、各時点の保存内容を照合 |
| 再オープン | プロセスを終了し、別の設定領域で同じ保存文書を開き、内容と画像を確認 |
| 日本語IME | 入力方式と親methodを確認し、OS経由のromajiから変換・確定し、保存された日本語を照合 |
| スクロール・フォーカス等 | OSの操作と、その操作による表示・状態の変化を確認。内部instrumentの設定と区別 |

既存の起動・撮影部品は `scripts/gui_validate.py` と `scripts/window_id.swift`、入力部品は `scripts/phase0_input.swift` にある。実験手順 `scripts/hosted_gui_interaction.py` / `.swift` は基本操作の調査用であり、productionの受領契約や網羅的GUI検証の完成を意味しない。

`pass` は要求されたシナリオの確認がすべて成功した場合だけとする。保存内容の不一致など観測した期待違反は `fail`、実行環境・入力方式・権限・タイムアウトなどで確認できない場合は `blocked` とする。複数シナリオは個別に記録し、一部成功で全体の `fail` / `blocked` を隠さない。プロセス消失・ジョブ中断・証拠不足も成功にしない。

## 依頼と結果の契約

依頼には repository、PR番号、full head SHA、control SHA / procedure version、scenario policy version、request ID、generation、作成時刻・期限を含める。手動依頼であれば実行者と許可の対象も記録する。依頼を受け取っただけで実行済みにしない。

結果には上記の対応情報に加え、実際のcheckout SHA・clean状態、binary digestとtoolchain/features、runner image/version、開始・終了時刻、工程・シナリオごとの `pass` / `fail` / `blocked`、理由、証拠の一覧・digestを含める。GitHub Actionsの run ID / attempt は artifact と結果に一致させる。

受領処理は本文中の自己申告だけを信頼せず、Actions APIでworkflow、repository、実行元、run/attempt、artifactを照合する。現在のPR headと世代を再取得し、不一致の結果はstaleとして現在判定から除く。同じ世代の終端結果は再処理しない。再実行は新しい世代を発行し、旧結果を消さずに分ける。

ビルド不能・ジョブ失敗・取消・期限切れで通常の結果が作れない場合も、受領側が実行状態を確認して `blocked` として終端処理する。`pass` / `fail` / `blocked` のいずれでも final judge を起動する。GUIが必要な場合、同じ対象SHA・現世代の `pass` がなければjudgeの `ready` を採用しない。

状態更新と外部副作用はPRごとに直列化し、headと世代を開始時および副作用直前に再確認する。配送・処理途中の失敗は期限付きの実行状態とreconciliationで回復させる。古い実行が新しい世代を上書きしないよう、完了・再試行・期限切れを区別する。

## 設定・文書・証拠

テスト文書、`HANE_STATE_DIR`、ログ、画像は実行ごとの一時領域へ分ける。入力・保存する文書には、ユーザーが普段使う文書を流用しない。起動・撮影だけのカスタム文書は、相対リソースの参照元を維持して扱い、入力シナリオへ転用しない。

artifact は結果JSON、summary、対象ログ、対象window画像、専用fixtureの保存内容、必要な環境情報だけを明示的に収集する。stateディレクトリ、認証設定、個人画面、デスクトップ全体をまとめて公開しない。hostedの結果でも公開可能なデータであることを確認する。

重要な実証は `docs/history/gui-validation/` に対象SHAと手順、成功・失敗・未検証の範囲を残す。artifactの保持期限後も、判断根拠を確認できるようにする。

## ローカル補完が必要になった場合

専用OSユーザー、private制御repo、常駐runnerは必須にしない。ユーザーが確認したコードと操作は通常のログインアカウントでも検証できる。未確認の外部PRを無人実行する許可とは扱わない。

まず対話的な専用コマンドで不足する操作だけを確認し、普段の入力と競合させない。対象PIDのみを操作・終了し、変更した入力方式等を復元する。個人パスが映った画像はそのまま公開しない。常駐化が必要な場合だけ、専用アカウント・private repo・runnerの配置を運用上の選択肢として再検討する。

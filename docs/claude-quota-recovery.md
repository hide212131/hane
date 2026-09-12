# Claude session quota recovery

Claude automatic fix が明示的な session 利用上限で実作業を開始できなかった場合だけ、通常の `Claude fix failed` から分離して待機・再開する。

## 契約

- `You've hit your session limit` と UTC の reset 時刻を同じ実行ログから確認できる場合だけ quota wait に分類する。一般的な exit code 1、認証失敗、timeout、原因不明の失敗は従来どおり terminal failure とする。
- quota wait は `hane/claude-fix` に `pending` として `Claude fix quota wait until <UTC> for <short-sha>` を記録する。`target_url` は分類用の後続 run ではなく、利用上限が実際に発生した Claude worker run を保持する。
- advertised reset 時刻の2分後を `retry_after` とする。時刻前は再配送しない。
- 再配送直前に open / non-draft / same-repository / exact head、最新 routing が `fix` であることを再確認する。
- 同一 head では quota wait を最大2回まで再開できる。3回目の quota 到達は `Claude fix blocked: quota retry budget ...` として停止し、owner に引き継ぐ。
- quota retry は既存の manual retry authorization を作成・更新・消費しない。`docs/claude-retry-state.md` の「手動承認1件につき paid invocation は高々1回」という契約を変更しない。
- repository 全体で quota retry の dispatcher を直列化し、1回の reconcile で再開する PR は1件だけにする。同じ利用枠が戻った瞬間に複数 PR を同時起動しない。
- dispatch 前に `hane/claude-quota-retry` lease を記録する。同一 retry の重複配送を30分抑止し、配送が失われた場合だけ再回復できる。

## 状態遷移

```text
Claude fix failed
  |
  | explicit session-limit log + reset time
  v
quota wait (pending, retry_after)
  |
  | retry_after reached + exact-head/routing revalidation
  v
Claude automatic fix worker
  |-- success/new head --> normal validation cycle
  |-- ordinary failure --> terminal failed
  `-- quota again --> quota wait (up to 2 retries) --> blocked
```

この回復は quota だけを対象にする。通常失敗を自動で有料再実行する仕組みにはしない。

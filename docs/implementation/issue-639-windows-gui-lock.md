# Issue #639: Windows GUI lock launcher — implementation record

- 状態: PR #641を含むmainを同期済み。同期後current-headのWindows/macOS専用CI、最終レビュー、papa-win実機確認は未完了。
- Issue: https://github.com/hide212131/hane/issues/639
- Parent work to unblock: PR #638 / Issue #567 P6 Windows focused GUI.
- 準備時点のbase: `a50c44669d6a116dfbe13439bb258db39d9bf92b`。
- 同期したmain / PR #641のmerge commit: `c4986bece1e67ffb18f4f2238a3b11f8f3769281`。
- 同期直前のPR head: `5d666aa807c76d76f06adef89da0883177f54ed2`。
- main同期commit（この記録の更新前）: `7f0f085fca08258210769b73ead479b9a487522e`。競合はなく、launcherと直接テストの内容は同期前と同一。
- launcher / 直接テストの実装commit: `fcdbd3ef47d0c0bf00d6395c98412d89f4099a27`。文書更新を含む最終headと実行結果は[PR #640](https://github.com/hide212131/hane/pull/640)に記録する。
- The PR contains the Windows launcher, its process tests, this record, and the recovery-guide update. The current implementation is not yet proof that Windows GUI was run.
- AADW run [40443432550](https://github.com/hide212131/hane/actions/runs/40443432550) completed prepare, Claude Sonnet 5.5 verification, finalizer, workspace regression tests, and lint successfully.
- CI run [40443655916](https://github.com/hide212131/hane/actions/runs/40443655916)と[40489256945](https://github.com/hide212131/hane/actions/runs/40489256945)は専用ジョブ追加前の履歴であり、今回のOSロック受入証拠には使わない。同期後current headで`Windows GUI lock lifecycle tests`と`macOS GUI validator lock tests`が実際に実行・成功することを確認する。
- 同期時の静的確認で、mainのmacOSジョブ条件はmacOS専用入力だけを対象とし、本PRのWindows launcher / 直接テストの変更では起動しないことが分かった。親担当へ配線の修正調整を依頼済み。macOSジョブのskipを成功扱いせず、両OSの実行証拠が揃うまでWindows実機確認へ進まない。
- papa-winの実canonical lock確認は未実施。上記専用CI成功前はWindows GUI・lock・候補へ触れない。成功後に既存Windows担当と共有作業を調整して取得・保持・解放を記録する。
- 最終候補のcurrent-head CI成功後にCodeRabbit full reviewを依頼し、対象head・範囲・完了状態を確認する。このチャットはPRのmergeとIssue #639のcloseを行わず、受入証拠を親担当へ引き渡す。

## 準備時点の既存経路と確認した不足（履歴）

`scripts/gui_validate.py` uses `pwd.getpwuid(...).pw_dir/.cache/hane/gui-validation.lock`, `fcntl.flock(LOCK_EX | LOCK_NB)`, and `subprocess.Popen(pass_fds=(lock_fd,))`. The same Python CLI also uses macOS-only build/window/screenshot tools and its operation guide explicitly limits actual launch/capture to macOS.

Windows GUI acceptance is operated directly in the connected Windows ChatGPT/Codex environment. The Windows task needs to acquire the same canonical lock before it starts an isolated Hane candidate, then keep the lock alive for the candidate's whole lifetime. 準備時点ではWindows用の正規lock-and-launch入口がなく、本PRで以下の入口を実装した。

## Accepted implementation direction

Add a small Windows-only launcher at `scripts/windows_gui_lock_launcher.py` with a command interface of the form:

```text
python scripts/windows_gui_lock_launcher.py -- <candidate-executable> [candidate arguments...]
```

The launcher is only a safe lock-and-launch boundary. It does not build Hane, create scenario fixtures/state, inspect GUI windows, capture screenshots, or automate input. The Windows GUI task remains responsible for those steps through the existing direct CUA workflow.

The launcher must:

1. Resolve the user's canonical profile location through a trusted Windows account/profile API and use `.cache/hane/gui-validation.lock` beneath it. Do not accept an environment override or per-run lock path in the production CLI.
2. Acquire the canonical path with a nonblocking OS-level exclusive lock before creating any candidate-specific state or launching the command. If another task holds the lock, report `blocked` and exit without building, creating a run directory/fixture, or launching the candidate.
3. Pass only the acquired OS handle to the exact child process using restricted handle inheritance. Do not enable global inheritance of ambient handles.
4. Wait for the child and return its exit status. If the launcher is interrupted or exits unexpectedly while the child remains alive, the child's inherited handle must keep the OS lock held. When the child exits, the OS must release the lock.
5. Never discover a presumed owner by looking at file timestamps, delete/truncate/replace the canonical lock file, or fall back to an independent temporary lock.
6. Log the canonical lock path, candidate executable/arguments, launcher PID, child PID, acquisition result, and cleanup/exit result without persisting a second owner registry.

The existing macOS `gui_validate.py` behavior, path, POSIX `flock`, and `pass_fds` semantics remain unchanged. Only extract a shared pure path/contract primitive if needed; do not turn this PR into a Windows screenshot/capture validator or a general CI/workflow redesign.

## Required verification

- Windows unit/integration tests use a test-only temporary lock path injected below the production CLI boundary and distinct helper processes. They prove: first acquisition succeeds; a concurrent attempt blocks without launching its child; the child inherits the handle; terminating the launcher while the child remains does not free the lock; child exit releases it; a subsequent process can acquire it.
- Test that missing parent/profile path or OS lock errors fail closed and do not invoke the candidate command. Test that arguments containing spaces are passed as argv elements without `shell=True`.
- Windows CI runs the real OS-lock lifecycle tests. macOS CI reruns existing `scripts/tests/test_gui_validate.py`, including contention and surviving-child lock inheritance, to prove the existing flow remains intact.
- Documentation explains the Windows helper invocation and its boundary with the macOS-only capture CLI and direct Windows CUA workflow.
- On papa-win, after the helper passes CI, use it for one isolated candidate to confirm the actual canonical lock is acquired before launch, remains held through the GUI run, and is released after the candidate exits. Record exact head, binary hash/path, launcher/candidate PID, fixture/state, evidence, and cleanup. This is not a request to run GUI during the implementation worker turn.

## Not included

No Hane product source changes, no new GUI input/capture automation, no lock-owner discovery API, no persistent status DB, no workflow/Gate/AADW policy edits, no modifications to the protected worker limits, no cleanup of other PRs' candidate/state, and no merge/Issue close. PR #638 remains Draft until its required GUI checks are completed.

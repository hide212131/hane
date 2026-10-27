# Issue #639: Windows GUI lock launcher — implementation preparation

- Status: preparation only. Product/tool implementation and tests have not started.
- Issue: https://github.com/hide212131/hane/issues/639
- Parent work to unblock: PR #638 / Issue #567 P6 Windows focused GUI.
- Base/current main at preparation: `a50c44669d6a116dfbe13439bb258db39d9bf92b`.
- The current PR contains this preparation document only. Do not treat it as completed support or as proof that Windows GUI was run.

## Current source and confirmed gap

`scripts/gui_validate.py` uses `pwd.getpwuid(...).pw_dir/.cache/hane/gui-validation.lock`, `fcntl.flock(LOCK_EX | LOCK_NB)`, and `subprocess.Popen(pass_fds=(lock_fd,))`. The same Python CLI also uses macOS-only build/window/screenshot tools and its operation guide explicitly limits actual launch/capture to macOS.

Windows GUI acceptance is operated directly in the connected Windows ChatGPT/Codex environment. The Windows task needs to acquire the same canonical lock before it starts an isolated Hane candidate, then keep the lock alive for the candidate's whole lifetime. No supported Windows lock-and-launch entrypoint exists today.

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

#!/usr/bin/env python3
"""Hosted GUI interaction spike (Issue 44, follow-up to the launch probe).

Runs independent interactive smoke scenarios against the pinned target
checkout's Hane binary on a GitHub-hosted macOS runner:

  1. ascii_edit_save_undo_redo_reopen — OS-level select-all, type a known
     ASCII string, Cmd-S, undo+save, redo+save, then terminate and reopen
     the same fixture and check its content again.
  2. japanese_ime_input — select a built-in Japanese input source (if one
     is available), type known romaji via real key events so the OS IME
     performs the conversion, confirm/commit, Cmd-S, and check the saved
     content. Blocked (not failed) if no Japanese source is available.

This is still an interactive *smoke* check: it proves keyboard input, save,
undo/redo and reopen paths exist and produce the expected file bytes, plus a
best-effort screenshot per session. It does not prove rendering pixel-by-
pixel, and it does not cover all focus changes or dialogs. OS wheel scrolling is
checked independently using visible line numbers recognized by Vision OCR.

This script loads scripts/gui_validate.py from the trusted control
checkout via importlib and calls its preflight/build/launch/
window_discovery/capture/cleanup functions directly, so the exact same PID
lifecycle logic is reused rather than reimplemented.
"""

from __future__ import annotations

import importlib.util
import hashlib
import json
import os
import math
import platform
import re
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Optional

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "hosted-gui-interaction/7"
VERIFICATION_KIND = "interactive_input_smoke"
SCOPE_NOTE = (
    "この結果はキーボード入力・保存・undo/redo・再オープン・日本語 IME 入力・"
    "OSスクロール、および太字/斜体複合・複数行 inline code・quote・list における"
    "marker/本文境界へのクリック・caret移動・ドラッグ選択・区切り記号の追加削除の基本スモーク確認に"
    "限定される。区切り記号の未閉鎖/再閉鎖、および既存の複数行 code span 自体の閉じ backtick の"
    "削除・再入力では、caretを中立位置へ移して撮影した描画ピクセルのdigestが変化し、"
    "再閉鎖後に初期閉鎖状態へ戻ることも確認する。OCR は座標特定にのみ使用し、"
    "bold/italic等の意味的なstyle種別そのものはOCRでは判定しない。全フォーカス移動・ダイアログ表示等の"
    "網羅的な GUI 検証はまだ証明されていない。"
)

EXIT_PASS = 0
EXIT_NONPASS = 1

ASCII_FIXTURE_ORIGINAL = "# hosted gui interaction spike\n\noriginal content\n"
ASCII_KNOWN_TEXT = "hane hosted gui ascii check"
ASCII_AFTER_APPEND = ASCII_KNOWN_TEXT + "x"
IME_FIXTURE_ORIGINAL = "# hosted gui interaction spike (ime)\n\n"
IME_ROMAJI = "nihongo"
IME_EXPECTED_TEXT = "日本語"

INLINE_FIXTURE_ORIGINAL = (
    "# hosted gui interaction inline syntax spike\n"
    "\n"
    "**bold *italic* combo** boundary line.\n"
    "\n"
    "this inline `code\n"
    "span` crosses a line.\n"
    "\n"
    "> quote with **bold** inside.\n"
    "\n"
    "- list item with *italic* inside.\n"
)

BOLD_ITALIC_OPEN_RE = r"\*\*(?=bold \*italic)"
BOLD_ITALIC_CLOSE_RE = r"(?<=combo)\*\*"
CODE_SPAN_OPEN_RE = r"`(?=code)"
CODE_SPAN_CLOSE_RE = r"(?<=span)`"
QUOTE_BOLD_OPEN_RE = r"(?<=quote with )\*\*(?=bold)"
LIST_ITALIC_OPEN_RE = r"(?<=item with )\*(?=italic)"

BOLD_ITALIC_OCR_RE = r"bold italic combo"
CODE_SPAN_OPEN_OCR_RE = r"code"
CODE_SPAN_CLOSE_OCR_RE = r"span"
QUOTE_BOLD_OPEN_OCR_RE = r"bold(?= inside)"
LIST_ITALIC_OPEN_OCR_RE = r"italic(?= inside)"
DRAG_SELECT_START_OCR_RE = r"(?<=b)old(?= italic combo)"
DRAG_SELECT_END_OCR_RE = r"com(?=bo boundary)"
BOUNDARY_MARK = "Z"
NAVIGATION_MARK = "N"
JAPANESE_SOURCE = "com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese"

# Issue #137 review (Codex, PR #139): クリックの着地点が OCR/helper 誤差ではなく
# 製品側 source-mapping の不整合であることを、OCR を一切経由しない実 EditorView・
# 実 glyph shaping・実 mouse event で独立に確認する probe。crates/** には恒久追加
# せず、`run_coordinate_independent_probe` がこの一時テスト本体を使い捨ての
# SHA検証済みクローン (`env.snapshot_checkout`) の `crates/ui/src/view.rs` へ注入し、
# 実行後に original bytes と clean tree を復元・証明したうえで破棄する。
COORDINATE_PROBE_TEST_NAME = "boundary_click_lands_on_source_offset_independent_of_ocr"
# libtest 上のテスト名は crate ルートからの module path 付きになる(注入先は
# crates/ui/src/view.rs の `#[cfg(test)] mod tests`)。関数名だけを `--exact` で
# 渡すと一致するテストが無く `running 0 tests` を終了コード0で返し、独立
# hit-test を一度も実行せずに `pass` を返してしまう(Issue #137 review, PR #139)。
COORDINATE_PROBE_TEST_QUALIFIED_NAME = f"view::tests::{COORDINATE_PROBE_TEST_NAME}"
# probe の assertion failure を示す panic メッセージの一部。cargo test の非0終了が
# コンパイルエラー・linker/toolchain 障害・Cargo.lock 問題など検証環境側の失敗なのか、
# probe 自身が製品 source-mapping 不整合を検出した assertion failure なのかを区別する
# ために使う(Issue #137 review, PR #139)。
COORDINATE_PROBE_FAILURE_MARKER = "product source-mapping mismatch, not OCR/helper noise"
# 対象テストの成功行そのもの。stdout+stderr を末尾で切り詰めた `cargo_test_output`
# とは別に、切り詰めの影響を受けない全文検索でこの行を確定的に抽出・保持する
# (Codex review, PR #139: 通常の hosted 実行では stderr のコンパイル出力だけで
# 80行を超え、成功時でも stdout 側のこの行が tail から脱落して fail-closed に
# 拒否されていた)。
COORDINATE_PROBE_TARGET_LINE_RE = re.compile(
    r"^test " + re.escape(COORDINATE_PROBE_TEST_QUALIFIED_NAME) + r" \.\.\. (ok|FAILED)$",
    re.MULTILINE,
)
# 各ケースの構造化 evidence 行(case identity・意図した visual boundary/side・
# canonical offset・実 GPUI caret/source offset・classification)。libtest は
# 既定で成功したテストの stdout を握りつぶすため、`--nocapture` と組み合わせて
# 使う(Codex review, PR #139)。
COORDINATE_PROBE_CASE_LINE_RE = re.compile(r"^COORDINATE_PROBE_CASE (\{.*\})$", re.MULTILINE)
COORDINATE_PROBE_EXPECTED_CASES = (
    "bold_open", "bold_close", "code_open", "code_close",
    "quote_open", "quote_close", "list_open", "list_close",
)

COORDINATE_PROBE_RUST_SOURCE = r'''

    /// The row's own hidden-markup rendering (no caret touching it), as the
    /// macOS hosted GUI validator's `click-text` OCR sees it — used only to
    /// locate the visual x-offset to click at, never as the offset oracle
    /// below.
    fn row_visual_text(
        view: &gpui::Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
        line: usize,
    ) -> String {
        cx.update(|_, app| {
            view.read_with(app, |editor_view, _| {
                editor_view
                    .rendered_line(line)
                    .expect("line rendered")
                    .visual_text
            })
        })
    }

    // Issue #137 review (Codex, PR #139): injected temporarily by
    // scripts/hosted_gui_interaction.py's `run_coordinate_independent_probe`
    // into a disposable, SHA-verified clone of the exact target commit —
    // never committed permanently under crates/**. It drives the real
    // `EditorView` render tree (`debug_bounds`, glyph shaping, `row_click`,
    // `simulate_mouse_down`/`simulate_mouse_up`) with the caret parked away
    // from the target constructs, so their `**`/`*`/`` ` ``/`>`/`-` markers
    // are hidden exactly as the OCR'd screenshot would see them — a
    // coordinate-specific event with no OCR bounding box in the loop, unlike
    // `boundary_edit_check` in the validator.
    //
    // Every expected offset here is the canonical position derived purely
    // by searching the source text — never through `source_map`/
    // `source_offset_for_visual_position` — so a mismatch against the
    // actual post-click selection is unambiguous, independent evidence of a
    // product source-mapping defect, not OCR/helper noise. Every case runs
    // before any assertion, so one mismatch never hides the others, and each
    // mismatch is collected as `fail` evidence instead of being folded into
    // the expected value.
    #[gpui::test]
    fn boundary_click_lands_on_source_offset_independent_of_ocr(cx: &mut gpui::TestAppContext) {
        let text = "x\n\n**bold *italic* combo** boundary line.\n\nthis inline `code\nspan` crosses a line.\n\n> quote with **bold**\n\n- list item with *italic*\n";
        let (view, cx, root) = open_view_for_mouse_tests(cx, text, false);
        assert!(root.is_none());

        let bold_source_line = "**bold *italic* combo** boundary line.";
        let bold_line_start = text.find(bold_source_line).unwrap();
        let bold_open_expected = SourceOffset(bold_line_start + 2);
        // Canonical closing position is right before the "**" marker, not
        // after it (Issue #137 review: folding the marker length into the
        // expected value absorbed the very mismatch this probe exists to
        // surface).
        let bold_close_expected = SourceOffset(
            bold_line_start + bold_source_line.find("combo**").unwrap() + "combo".len(),
        );

        let code_open_source_line = "this inline `code";
        let code_open_line_start = text.find(code_open_source_line).unwrap();
        let code_open_expected =
            SourceOffset(code_open_line_start + code_open_source_line.find('`').unwrap() + 1);

        let code_close_source_line = "span` crosses a line.";
        let code_close_line_start = text.find(code_close_source_line).unwrap();
        let code_close_expected = SourceOffset(
            code_close_line_start + code_close_source_line.find("span`").unwrap() + "span".len(),
        );

        let quote_source_line = "> quote with **bold**";
        let quote_line_start = text.find(quote_source_line).unwrap();
        let quote_open_expected =
            SourceOffset(quote_line_start + quote_source_line.find("**bold").unwrap() + 2);
        let quote_close_expected = SourceOffset(
            quote_line_start + quote_source_line.find("bold**").unwrap() + "bold".len(),
        );

        let list_source_line = "- list item with *italic*";
        let list_line_start = text.find(list_source_line).unwrap();
        let list_open_expected =
            SourceOffset(list_line_start + list_source_line.find("*italic").unwrap() + 1);
        let list_close_expected = SourceOffset(
            list_line_start + list_source_line.find("italic*").unwrap() + "italic".len(),
        );

        // Clicking a construct discloses its markers on every following
        // frame (the same "caret inside reveals `**`" behavior the disclosure
        // tests at line 4568+ cover), so each case below re-parks the caret
        // on the neutral first line and re-reads the row's hidden-markup
        // visual text immediately before computing where to click — mirroring
        // `_move_to_neutral` before every capture in the Python validator.
        // Issue #137 review (Codex, PR #139): a bare pass/fail on the whole
        // libtest run only proves *some* GPUI click landed correctly, not
        // which of the 8 boundary constructs did. Each case below prints a
        // single-line, machine-parseable `COORDINATE_PROBE_CASE {...}` JSON
        // record — case identity, intended visual boundary/side, the
        // canonical source offset computed above (independent of
        // `source_map`), and the actual post-click GPUI selection — *before*
        // the mismatch check, so the validator (`.github/scripts/gui_policy.py`)
        // can verify per-case evidence for a `pass` receipt instead of
        // trusting a single test name/count. `--nocapture` (see
        // `run_coordinate_independent_probe`) is required for these lines to
        // reach `cargo test`'s stdout on a passing run.
        let mut mismatches: Vec<String> = Vec::new();
        for (case, edge, selector, line, row_index, needle, edge_offset, canonical) in [
            ("bold_open", "start", "row-2-0", 2, 0, "bold", 0usize, bold_open_expected),
            ("bold_close", "end", "row-2-0", 2, 0, "combo", "combo".len(), bold_close_expected),
            // "this inline `code" / "span` crosses a line." are one soft-wrapped
            // paragraph block, so the second source line is the block's row 1,
            // not its own row 0.
            ("code_open", "start", "row-4-0", 4, 0, "code", 0, code_open_expected),
            ("code_close", "end", "row-5-1", 5, 1, "span", "span".len(), code_close_expected),
            ("quote_open", "start", "row-7-0", 7, 0, "bold", 0, quote_open_expected),
            ("quote_close", "end", "row-7-0", 7, 0, "bold", "bold".len(), quote_close_expected),
            ("list_open", "start", "row-9-0", 9, 0, "italic", 0, list_open_expected),
            ("list_close", "end", "row-9-0", 9, 0, "italic", "italic".len(), list_close_expected),
        ] {
            view.update(cx, |view, cx| {
                view.editor_mut()
                    .set_selection(Selection::caret(SourceOffset(0)))
                    .unwrap();
                cx.notify();
            });
            cx.run_until_parked();

            let visual = row_visual_text(&view, cx, line);
            let visual_offset = visual.find(needle).unwrap() + edge_offset;

            let (point, _row_click_predicted) =
                row_click(&view, cx, selector, line, row_index, visual_offset);
            cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
            cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());
            let actual = view.read_with(cx, |view, _| view.editor().selection());
            let at_canonical = actual == Selection::caret(canonical);
            println!(
                "COORDINATE_PROBE_CASE {{\"case\":\"{case}\",\"edge\":\"{edge}\",\"canonical_source_offset\":{},\"actual_source_offset\":{},\"classification\":\"{}\"}}",
                canonical.0,
                actual.active.0,
                if at_canonical { "at_canonical" } else { "mismatch" },
            );
            if !at_canonical {
                mismatches.push(format!(
                    "click on {selector} at visual offset {visual_offset} landed on {actual:?}, \
                     independent of OCR, instead of the canonical source offset {canonical:?}"
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "independent GPUI click(s) landed on a non-canonical source offset — product \
             source-mapping mismatch, not OCR/helper noise:\n{}",
            mismatches.join("\n")
        );
    }
'''

# 境界クリック挿入が期待 canonical position からこの文字数を超えて離れて着地した場合の
# 分類の閾値(診断用途のみ)。click_point は OCR bounding box から算出されており、
# OS click が意図した visual boundary を指したという独立証拠にはならない。そのため
# delta の大小によらず、実 EditorView への coordinate-specific event など独立した座標検証
# なしでは製品 source-mapping 疑いの fail と断定できず、helper/OCR miss の可能性を
# 区別できないものとして procedure の blocked として扱う(Issue #137)。値は対象境界の
# marker(最大2文字の `**`)+ 直後の空白1文字を含む近傍かどうかを分類記録するためだけに使う。
BOUNDARY_LANDING_FAR_MISS_CHARS = 4

# 上記の分類で procedure blocked となった境界クリックのうち、この集合に該当する
# ものは baseline へ復元したうえでもう一度 screenshot・OCR・クリックをやり直し、
# 同じ非 canonical offset が再現するかどうかを追加の証拠として記録する(Issue #137
# review: OCR 座標誤差と製品 source mapping 不具合を区別してほしいという指摘への
# 対応)。ただし再試行も同じ OCR bounding box→座標算出の helper 経路を使うため、
# systematic な OCR/クリック誤差があれば製品が正しくても毎回同じ offset へ再現
# しうる。したがって再現しても実 EditorView への coordinate-specific event など
# OCR と独立した座標検証にはならず、fail へは昇格しない(PR #139 review)。再現
# の有無にかかわらず procedure blocked のまま #101 側での切り分けに委ねる。
AMBIGUOUS_LANDING_CLASSIFICATIONS = frozenset({"far_miss", "boundary_ambiguous_near_canonical"})

# inline_syntax_boundary の各 mutating subtest は、成功経路であっても最大で数回の
# undo-save までしか積まない(delimiter_toggle の 3 段編集が最長)。fail/blocked 後の
# baseline 復元はこの上限に余裕を持たせた回数だけ undo-save を試み、それでも一致
# しなければ復元失敗として fail-closed にする。
BASELINE_RESTORE_MAX_UNDOS = 8

# Hane の save_session は書き込みを background executor へ投入して非同期に完了する
# ため、force-save/undo-save のキー送信が返った直後に読んだ fixture バイト列は、
# 保存中の古い(baseline と偶然一致する)内容である場合がある。save_session の完了を
# 明示的に通知する手段がないため、固定の短い settle 秒数では、非同期書き込みが
# その秒数より遅れて到着する環境で古い baseline を「一致」と誤確定してしまう
# (Codex review on PR #138)。baseline 復元は fail/blocked 後にしか実行されない
# 低頻度経路なので、一致を確定する前に呼び出し側から渡された poll_timeout の
# 残り時間いっぱいまで無変化を確認し、締切までに到着するどんな遅延書き込みも
# 見逃さないようにする。


def match_insertion_offset(text: str, pattern: str, *, edge: str) -> int:
    match = re.search(pattern, text)
    if not match:
        raise ValueError(f"pattern not found in expected fixture text: {pattern}")
    return match.end() if edge == "end" else match.start()


def insert_at_match(text: str, pattern: str, insertion: str, *, edge: str) -> str:
    position = match_insertion_offset(text, pattern, edge=edge)
    return text[:position] + insertion + text[position:]


def locate_single_insertion_offset(baseline: str, actual: str, insertion: str) -> Optional[int]:
    """baseline に insertion を 1 箇所だけ挿入すると actual になる、その挿入位置を返す。

    そのような位置が一意に定まらない(挿入以外の破損・複数候補がある等)場合は
    None を返す。これにより「単一文字が期待と違う場所へ着地した」という
    helper 自身の到達失敗と、単純な不一致では説明できない破損とを区別できる。
    """
    if len(actual) != len(baseline) + len(insertion):
        return None
    candidates = [
        offset
        for offset in range(len(baseline) + 1)
        if baseline[:offset] + insertion + baseline[offset:] == actual
    ]
    return candidates[0] if len(candidates) == 1 else None


def delimiter_states(delimiter: str) -> tuple[str, str]:
    if delimiter not in ("*", "**", "`"):
        raise ValueError("unsupported delimiter")
    closed = INLINE_FIXTURE_ORIGINAL + f" {delimiter}loose{delimiter} tail"
    unclosed = INLINE_FIXTURE_ORIGINAL + f" {delimiter}loose tail"
    return unclosed, closed


def load_pinned_gui_validate(control_dir: Path):
    sys.dont_write_bytecode = True
    module_path = control_dir / "scripts" / "gui_validate.py"
    if not module_path.is_file():
        raise SystemExit(f"pinned gui_validate.py not found at {module_path}")
    spec = importlib.util.spec_from_file_location("pinned_gui_validate", module_path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"could not load module spec for {module_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules["pinned_gui_validate"] = module
    spec.loader.exec_module(module)
    return module


def make_step(name: str, result: str, reason: Optional[str] = None, **detail) -> dict:
    return {"name": name, "result": result, "reason": reason, **detail}


def skipped_step(name: str, reason: str) -> dict:
    return {"name": name, "result": "skipped", "reason": reason}


def worst_result(steps: list[dict], priority: dict[str, int]) -> str:
    considered = [s["result"] for s in steps if s["result"] in priority]
    return min(considered, key=lambda r: priority[r]) if considered else "blocked"


def reason_for(steps: list[dict]) -> str:
    reasons = [s["reason"] for s in steps if s.get("reason") and s["result"] != "pass"]
    return "; ".join(reasons) if reasons else "すべての工程が成功した"


@dataclass(frozen=True)
class PreparedHelper:
    binary: Path
    digest: str


def prepare_helper(source: Path, directory: Path) -> PreparedHelper:
    binary = directory / "interaction-helper"
    subprocess.run(
        ["/usr/bin/swiftc", str(source), "-o", str(binary)],
        capture_output=True,
        text=True,
        check=True,
        timeout=120,
    )
    binary.chmod(0o500)
    return PreparedHelper(binary, hashlib.sha256(binary.read_bytes()).hexdigest())


def run_helper(swift_helper: PreparedHelper, args: list[str], timeout: float) -> tuple[bool, str, str]:
    try:
        if hashlib.sha256(swift_helper.binary.read_bytes()).hexdigest() != swift_helper.digest:
            return False, "", "compiled interaction helper integrity mismatch"
        proc = subprocess.run(
            [str(swift_helper.binary), *args], capture_output=True, text=True, timeout=timeout
        )
        if hashlib.sha256(swift_helper.binary.read_bytes()).hexdigest() != swift_helper.digest:
            return False, "", "compiled interaction helper integrity mismatch"
    except subprocess.TimeoutExpired:
        return False, "", f"swift helper timed out after {timeout}s: {' '.join(args)}"
    except OSError as exc:
        return False, "", f"swift helper failed to start: {exc}"
    if proc.returncode != 0:
        return False, proc.stdout, proc.stderr.strip() or f"swift helper exited {proc.returncode}"
    return True, proc.stdout.strip(), ""


def wait_for_fixture_bytes(
    fixture_path: Path, expected: bytes, timeout: float, interval: float = 0.2
) -> tuple[bool, bytes]:
    deadline = time.monotonic() + timeout
    last = b""
    while True:
        try:
            last = fixture_path.read_bytes()
        except OSError:
            last = b""
        if last == expected:
            return True, last
        if time.monotonic() >= deadline:
            return False, last
        time.sleep(interval)


def wait_for_fixture_settled_bytes(
    fixture_path: Path, expected: bytes, timeout: float, interval: float = 0.2,
) -> tuple[bool, bytes]:
    """Like wait_for_fixture_bytes, but a match must survive re-reads all the
    way to `timeout` before being accepted. save_session writes asynchronously
    in the background executor with no completion signal this harness can
    observe, so a read right after the save keystroke can observe stale bytes
    that coincidentally equal `expected` while the real write is still in
    flight and lands arbitrarily later. A fixed short settle window would
    still miss writes that land after it elapses, so this holds the match
    open for the caller's entire remaining budget instead of a fixed
    constant, catching any write landing before the deadline. This is only
    called to restore state after a fail/blocked subtest, so spending the
    full timeout on the success path is acceptable."""
    deadline = time.monotonic() + timeout
    last = b""
    matched_since: Optional[float] = None
    while True:
        try:
            last = fixture_path.read_bytes()
        except OSError:
            last = b""
        now = time.monotonic()
        if last == expected:
            if matched_since is None:
                matched_since = now
        else:
            matched_since = None
        if now >= deadline:
            return matched_since is not None, last
        time.sleep(interval)


def make_config(module, *, workspace_dir, scenario, expected_sha, request_id, generation, run_dir,
                 fixture_path, features, extra_env, startup_timeout, window_timeout):
    state_dir = run_dir / "state"
    run_dir.mkdir(parents=True, exist_ok=True)
    state_dir.mkdir(parents=True, exist_ok=True)
    return module.Config(
        workspace_dir=workspace_dir,
        scenario=scenario,
        expected_sha=expected_sha,
        request_id=request_id,
        generation=generation,
        run_dir=run_dir,
        state_dir=state_dir,
        log_path=run_dir / "hane.log",
        image_path=run_dir / f"{scenario}.png",
        fixture_path=fixture_path,
        features=features,
        extra_env=extra_env,
        startup_timeout_seconds=startup_timeout,
        window_timeout_seconds=window_timeout,
    )


def capture_named(module, env, config, window_id: str, run_dir: Path, label: str) -> dict:
    capture_cfg = replace(config, image_path=run_dir / f"{label}.png")
    capture_step = module.do_capture(env, capture_cfg, window_id)
    return {**capture_step, "name": f"capture_{label}"}


def open_session(module, env, config, binary_path, process_holder, capture_label: str) -> tuple[list[dict], Optional[str]]:
    steps = []
    launch_step = module.do_launch(env, config, binary_path, process_holder)
    steps.append(launch_step)
    if launch_step["result"] != "pass":
        steps.append(skipped_step("window_discovery", "launch が pass しなかった"))
        steps.append(skipped_step(f"capture_{capture_label}", "launch が pass しなかった"))
        return steps, None
    window_step, window_id = module.do_window_discovery(env, config, process_holder["process"])
    steps.append(window_step)
    if window_step["result"] != "pass":
        steps.append(skipped_step(f"capture_{capture_label}", "window_discovery が pass しなかった"))
        return steps, None
    steps.append(capture_named(module, env, config, window_id, config.run_dir, capture_label))
    return steps, window_id


def close_session(module, env, process_holder) -> dict:
    return module.do_cleanup(env, process_holder["process"])


def current_pid(process_holder) -> Optional[int]:
    process = process_holder.get("process")
    return process.pid if process is not None else None


def verify_visible_text(helper, image_path, expected, timeout):
    ok, text, error = run_helper(helper, ["ocr", str(image_path)], timeout)
    if not ok:
        return make_step("visible_saved_text", "blocked", reason=error)
    normalized = re.sub(r"\s+", "", text).casefold()
    matched = re.sub(r"\s+", "", expected).casefold() in normalized
    return make_step(
        "visible_saved_text", "pass" if matched else "fail",
        reason=None if matched else "保存した文書の表示をOCRで確認できない",
        expected=expected, recognized_text=text,
    )


def image_pixel_digest(helper, image_path: Path, timeout: float) -> tuple[Optional[str], Optional[str]]:
    ok, value, error = run_helper(helper, ["image-digest", str(image_path)], timeout)
    if not ok:
        return None, error
    if not re.fullmatch(r"[0-9a-f]{64}", value):
        return None, f"invalid image pixel digest: {value!r}"
    return value, None


def run_ascii_scenario(module, env, target_dir, swift_helper, base_run_dir, binary_path,
                        expected_sha, request_id, startup_timeout, window_timeout,
                        helper_timeout, poll_timeout, priority) -> dict:
    run_dir = base_run_dir / "ascii_edit_save_undo_redo_reopen"
    fixture_path = run_dir / "ascii-fixture.md"
    run_dir.mkdir(parents=True, exist_ok=True)
    fixture_path.write_text(ASCII_FIXTURE_ORIGINAL, encoding="utf-8")
    steps: list[dict] = []
    process_holder: dict = {"process": None}
    config = make_config(
        module, workspace_dir=target_dir, scenario="ascii-edit-save-undo-redo",
        expected_sha=expected_sha, request_id=request_id, generation="1", run_dir=run_dir,
        fixture_path=fixture_path, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
    )
    try:
        session_steps, window_id = open_session(module, env, config, binary_path, process_holder, "before")
        steps += session_steps
        pid = current_pid(process_holder)
        if pid is not None and window_id is not None:
            ok, _out, err = run_helper(swift_helper, ["select-all-type-save", str(pid), ASCII_KNOWN_TEXT], helper_timeout)
            if not ok:
                steps.append(make_step("edit_save", "blocked", reason=err))
            else:
                matched, actual = wait_for_fixture_bytes(fixture_path, ASCII_KNOWN_TEXT.encode("utf-8"), poll_timeout)
                steps.append(make_step("edit_save", "pass" if matched else "fail",
                                       reason=None if matched else "保存後のフィクスチャ内容が期待した ASCII 文字列と一致しない",
                                       actual=actual.decode("utf-8", errors="replace")))
            ok, _out, err = run_helper(swift_helper, ["append-save", str(pid), "x"], helper_timeout)
            if not ok:
                steps.append(make_step("append_save", "blocked", reason=err))
            else:
                matched, actual = wait_for_fixture_bytes(fixture_path, ASCII_AFTER_APPEND.encode(), poll_timeout)
                steps.append(make_step("append_save", "pass" if matched else "fail",
                                       reason=None if matched else "追加入力が保存されていない",
                                       actual=actual.decode("utf-8", errors="replace")))
            ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
            if not ok:
                steps.append(make_step("undo_save", "blocked", reason=err))
            else:
                matched, actual = wait_for_fixture_bytes(fixture_path, ASCII_KNOWN_TEXT.encode("utf-8"), poll_timeout)
                steps.append(make_step("undo_save", "pass" if matched else "fail",
                                       reason=None if matched else "undo 後に保存された内容が追加入力前の文書と一致しない",
                                       actual=actual.decode("utf-8", errors="replace")))
            ok, _out, err = run_helper(swift_helper, ["redo-save", str(pid)], helper_timeout)
            if not ok:
                steps.append(make_step("redo_save", "blocked", reason=err))
            else:
                matched, actual = wait_for_fixture_bytes(fixture_path, ASCII_AFTER_APPEND.encode("utf-8"), poll_timeout)
                steps.append(make_step("redo_save", "pass" if matched else "fail",
                                       reason=None if matched else "redo 後に保存された内容が期待した ASCII 文字列と一致しない",
                                       actual=actual.decode("utf-8", errors="replace")))
            steps.append(capture_named(module, env, config, window_id, run_dir, "after"))
        else:
            for name in ("edit_save", "append_save", "undo_save", "redo_save"):
                steps.append(skipped_step(name, "対象プロセスの PID を取得できなかった"))
    finally:
        steps.append(close_session(module, env, process_holder))

    if worst_result(steps, priority) != "pass":
        steps.append(skipped_step("reopen", "再オープン前の工程が pass しなかった"))
        return {"name": "ascii_edit_save_undo_redo_reopen", "steps": steps,
                "result": worst_result(steps, priority), "reason": reason_for(steps),
                "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}

    reopen_dir = run_dir / "reopen"
    reopen_process_holder: dict = {"process": None}
    reopen_config = make_config(
        module, workspace_dir=target_dir, scenario="ascii-reopen",
        expected_sha=expected_sha, request_id=request_id, generation="2", run_dir=reopen_dir,
        fixture_path=fixture_path, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
    )
    try:
        session_steps, _window_id = open_session(module, env, reopen_config, binary_path, reopen_process_holder, "reopen")
        steps += session_steps
        steps.append(verify_visible_text(swift_helper, reopen_dir / "reopen.png", ASCII_AFTER_APPEND, helper_timeout))
        matched, actual = wait_for_fixture_bytes(fixture_path, ASCII_AFTER_APPEND.encode("utf-8"), 1.0)
        steps.append(make_step(
            "reopen_content_check", "pass" if matched else "fail",
            reason=None if matched else "再オープン後もフィクスチャ内容が期待通りであることを確認できない",
            note="ファイル内容の一致は再オープンの証跡の一部に過ぎず、描画結果そのものの証明ではない",
            actual=actual.decode("utf-8", errors="replace"),
        ))
    finally:
        steps.append(close_session(module, env, reopen_process_holder))
    return {"name": "ascii_edit_save_undo_redo_reopen", "steps": steps,
            "result": worst_result(steps, priority), "reason": reason_for(steps),
            "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}


def run_ime_scenario(module, env, target_dir, swift_helper, base_run_dir, binary_path,
                      expected_sha, request_id, startup_timeout, window_timeout,
                      helper_timeout, poll_timeout, priority) -> dict:
    run_dir = base_run_dir / "japanese_ime_input"
    run_dir.mkdir(parents=True, exist_ok=True)
    fixture_path = run_dir / "ime-fixture.md"
    fixture_path.write_text(IME_FIXTURE_ORIGINAL, encoding="utf-8")
    steps: list[dict] = []
    original_source: Optional[str] = None
    process_holder: dict = {"process": None}
    try:
        ok, out, err = run_helper(swift_helper, ["current-source"], helper_timeout)
        if not ok:
            steps.append(make_step("query_current_source", "blocked", reason=err))
        else:
            original_source = out
            steps.append(make_step("query_current_source", "pass", source_id=original_source))
        ok, out, err = run_helper(swift_helper, ["list-sources"], helper_timeout)
        if not ok:
            steps.append(make_step("list_input_sources", "blocked", reason=err))
            return {"name": "japanese_ime_input", "steps": steps, "result": worst_result(steps, priority),
                    "reason": reason_for(steps), "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}
        available = [line for line in out.splitlines() if line.strip()]
        steps.append(make_step("list_input_sources", "pass", available_sources=available))
        japanese_source = next((source_id for source_id in available if source_id == JAPANESE_SOURCE), None)
        if japanese_source is None:
            steps.append(make_step("select_japanese_source", "blocked", reason="このランナーに組み込みの日本語入力ソースが見つからない"))
            steps.append(skipped_step("ime_input_save", "日本語入力ソースを選択できなかった"))
            return {"name": "japanese_ime_input", "steps": steps, "result": worst_result(steps, priority),
                    "reason": reason_for(steps), "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}
        ok, _out, err = run_helper(swift_helper, ["select-source", japanese_source], helper_timeout)
        steps.append(make_step("select_japanese_source", "pass" if ok else "blocked", reason=None if ok else err,
                               selected_source=japanese_source))
        if not ok:
            steps.append(skipped_step("ime_input_save", "日本語入力ソースを選択できなかった"))
            return {"name": "japanese_ime_input", "steps": steps, "result": worst_result(steps, priority),
                    "reason": reason_for(steps), "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}
        config = make_config(
            module, workspace_dir=target_dir, scenario="japanese-ime-input",
            expected_sha=expected_sha, request_id=request_id, generation="1", run_dir=run_dir,
            fixture_path=fixture_path, features=["timing-probe"], extra_env={},
            startup_timeout=startup_timeout, window_timeout=window_timeout,
        )
        session_steps, window_id = open_session(module, env, config, binary_path, process_holder, "before")
        steps += session_steps
        pid = current_pid(process_holder)
        if pid is not None and window_id is not None:
            ok, _out, err = run_helper(swift_helper, ["type-romaji-commit-save", str(pid), IME_ROMAJI, japanese_source], helper_timeout)
            if not ok:
                steps.append(make_step("ime_input_save", "blocked", reason=err))
            else:
                matched, actual = wait_for_fixture_bytes(fixture_path, IME_EXPECTED_TEXT.encode("utf-8"), poll_timeout)
                steps.append(make_step("ime_input_save", "pass" if matched else "fail",
                                       reason=None if matched else "保存された内容が期待した日本語文字列と一致しない",
                                       romaji_input=IME_ROMAJI, expected_text=IME_EXPECTED_TEXT,
                                       actual_text=actual.decode("utf-8", errors="replace")))
            steps.append(capture_named(module, env, config, window_id, run_dir, "after"))
        else:
            steps.append(skipped_step("ime_input_save", "対象プロセスの PID を取得できなかった"))
    finally:
        steps.append(close_session(module, env, process_holder))
        if original_source:
            ok, _out, err = run_helper(swift_helper, ["select-source", original_source], helper_timeout)
            steps.append(make_step("restore_input_source", "pass" if ok else "blocked",
                                   reason=None if ok else err, restored_source=original_source))
    return {"name": "japanese_ime_input", "steps": steps,
            "result": worst_result(steps, priority), "reason": reason_for(steps),
            "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}


def run_scroll_scenario(module, env, target_dir, swift_helper, base_run_dir, binary_path,
                        expected_sha, request_id, startup_timeout, window_timeout,
                        helper_timeout, poll_timeout, priority):
    run_dir = base_run_dir / "os_scroll"
    run_dir.mkdir(parents=True, exist_ok=True)
    fixture = run_dir / "scroll-fixture.md"
    contents = "\n\n".join(f"LINE {n}" for n in range(1, 101)) + "\n"
    fixture.write_text(contents, encoding="utf-8")
    config = make_config(module, workspace_dir=target_dir, scenario="os-scroll",
                         expected_sha=expected_sha, request_id=request_id, generation="1",
                         run_dir=run_dir, fixture_path=fixture, features=["timing-probe"],
                         extra_env={}, startup_timeout=startup_timeout, window_timeout=window_timeout)
    holder = {"process": None}
    steps = []
    try:
        initial, window_id = open_session(module, env, config, binary_path, holder, "before")
        steps.extend(initial)
        if window_id is not None:
            before_ok, before_text, before_error = run_helper(swift_helper, ["ocr", str(run_dir / "before.png")], helper_timeout)
            ok, detail, error = run_helper(swift_helper, ["wheel", str(current_pid(holder)), "-600"], helper_timeout)
            steps.append(make_step("os_wheel", "pass" if ok else "blocked", reason=error or None, event=detail))
            steps.append(capture_named(module, env, config, window_id, run_dir, "after"))
            after_ok, after_text, after_error = run_helper(swift_helper, ["ocr", str(run_dir / "after.png")], helper_timeout)
            if not before_ok or not after_ok:
                steps.append(make_step("visible_scroll", "blocked", reason=before_error or after_error))
            else:
                before = [int(n) for n in re.findall(r"LINE\s+(\d+)", before_text, re.I)]
                after = [int(n) for n in re.findall(r"LINE\s+(\d+)", after_text, re.I)]
                moved = bool(before and after and 1 in before and min(after) > 1 and max(after) > max(before))
                steps.append(make_step("visible_scroll", "pass" if moved else "fail",
                                       reason=None if moved else "OSスクロール後に表示行の移動を確認できない",
                                       before_lines=before, after_lines=after, before_text=before_text, after_text=after_text))
            unchanged = fixture.read_text(encoding="utf-8") == contents
            steps.append(make_step("scroll_preserves_document", "pass" if unchanged else "fail",
                                   reason=None if unchanged else "スクロールで文書内容が変わった"))
    finally:
        steps.append(close_session(module, env, holder))
    return {"name": "os_scroll", "steps": steps, "result": worst_result(steps, priority),
            "reason": reason_for(steps), "evidence": {"fixture_path": str(fixture)}}


def _decode(data: bytes) -> str:
    return data.decode("utf-8", errors="replace")


def _move_to_neutral(swift_helper, pid: int, helper_timeout: float, name: str) -> dict:
    ok, _out, err = run_helper(swift_helper, ["move-doc-start", str(pid)], helper_timeout)
    return make_step(name, "pass" if ok else "blocked", reason=None if ok else err)


def restore_scenario_baseline(swift_helper, pid, fixture_path, baseline,
                               helper_timeout, poll_timeout, name):
    baseline_bytes = baseline.encode("utf-8")
    # 失敗/タイムアウトした mutating subtest は、編集キー送信後・保存キー送信前に
    # 止まっている可能性がある。その場合ディスク上の fixture は baseline のまま
    # (未保存)で「一致」に見えてしまい、undo を一度も実行しないまま次の
    # subtest が汚染された文書状態から始まる(Issue #136 の再発)。比較の前に
    # 必ず保存させ、in-memory の編集をディスクへ反映させてから判定する。
    ok, _out, err = run_helper(swift_helper, ["force-save", str(pid)], helper_timeout)
    if not ok:
        return make_step(name, "blocked",
                          reason=f"baseline 復元前の force-save に失敗した: {err}", attempts=0)
    # force-save/undo-save のキー送信は save_session の非同期な書き込み完了を待たない
    # ため、直後の読み取りは書き込み中の古い baseline 相当のバイト列を「一致」と誤認
    # しうる。settled 版で一致が一定時間保持されることまで確認してから受理する。
    matched, actual = wait_for_fixture_settled_bytes(fixture_path, baseline_bytes, poll_timeout)
    attempts = 0
    while not matched and attempts < BASELINE_RESTORE_MAX_UNDOS:
        ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
        if not ok:
            return make_step(name, "blocked",
                              reason=f"baseline 復元の undo に失敗した: {err}", attempts=attempts)
        attempts += 1
        matched, actual = wait_for_fixture_settled_bytes(fixture_path, baseline_bytes, poll_timeout)
    if not matched:
        return make_step(name, "blocked",
                          reason=f"{attempts} 回の undo でも fixture が baseline へ復元できない",
                          expected=baseline, actual=_decode(actual), attempts=attempts)
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_caret")
    if reset["result"] != "pass":
        return make_step(name, "blocked",
                          reason=f"baseline 復元後の caret 初期化に失敗した: {reset.get('reason')}")
    return make_step(name, "pass", attempts=attempts)


def run_mutating_subtest(steps, subtest_steps, name, process_holder, swift_helper,
                          fixture_path, baseline, helper_timeout, poll_timeout):
    steps.extend(subtest_steps)
    outcome = next((s["result"] for s in reversed(subtest_steps) if s["name"] == name), "blocked")
    if outcome == "pass":
        return True
    pid = current_pid(process_holder)
    if pid is None:
        steps.append(make_step(f"{name}_state_restore", "blocked",
                                reason="対象プロセスの PID を取得できず baseline へ復元できない"))
        return False
    restore_step = restore_scenario_baseline(swift_helper, pid, fixture_path, baseline,
                                              helper_timeout, poll_timeout, f"{name}_state_restore")
    steps.append(restore_step)
    return restore_step["result"] == "pass"


def parse_click_evidence(stdout: str) -> dict:
    """click-text の stdout(JSON 1行)から OCR/クリック evidence を取り出す。

    OCR の bounding box そのものを source 境界の真値として扱うのではなく、
    「OCR が認識した文字列と bounding box」「そこから helper が選んだ実 OS click 座標」を
    証跡として残すためだけに使う(Issue #137)。フィールド欠落・非 JSON は helper 自身の
    契約違反として ValueError にし、呼び出し側で製品 fail と区別できる procedure blocked
    として扱わせる。
    """
    try:
        payload = json.loads(stdout)
    except (ValueError, TypeError) as exc:
        raise ValueError(f"click-text の stdout を JSON として解釈できない: {stdout!r}") from exc
    required = {"matched_text", "bounding_box", "window_bounds", "click_point", "edge"}
    missing = required - payload.keys()
    if missing:
        raise ValueError(f"click-text evidence に必須フィールドが不足している({sorted(missing)}): {stdout!r}")
    return payload


def boundary_edit_check(swift_helper, screenshot_path, pid, fixture_path, baseline,
                        ocr_pattern, ocr_edge, source_pattern, source_edge, insertion,
                        helper_timeout, poll_timeout):
    detail = {"screenshot": str(screenshot_path)}
    expected_offset = match_insertion_offset(baseline, source_pattern, edge=source_edge)
    detail["expected_canonical_source_offset"] = expected_offset
    ok, out, err = run_helper(swift_helper, ["click-text", str(pid), str(screenshot_path), ocr_pattern, ocr_edge], helper_timeout)
    if not ok:
        return "blocked", (
            f"境界へのクリックに失敗した(OCR が対象文字列を認識できない、window bounds 取得失敗、"
            f"helper の timeout/integrity mismatch などの helper/OCR 側要因の可能性があり、"
            f"製品の source mapping を観測する前の procedure blocked とする): {err}"
        ), detail
    try:
        detail["click_evidence"] = parse_click_evidence(out)
    except ValueError as exc:
        return "blocked", (
            f"click-text から OCR bounding box / 実クリック座標の evidence を取得できず、"
            f"境界クリックの着地点を検証できない: {exc}"
        ), detail
    ok, _out, err = run_helper(swift_helper, ["type-save", str(pid), insertion], helper_timeout)
    if not ok:
        return "blocked", (
            f"境界への入力に失敗した(timeout、起動失敗、integrity mismatch、AppleScript の"
            f"実行失敗などの helper/実行環境側要因の可能性があり、probe の着地点を観測できて"
            f"いないため製品 fail と区別して procedure blocked とする): {err}"
        ), detail
    expected = insert_at_match(baseline, source_pattern, insertion, edge=source_edge)
    matched, actual_bytes = wait_for_fixture_bytes(fixture_path, expected.encode("utf-8"), poll_timeout)
    actual = _decode(actual_bytes)
    detail.update(expected_after_insert=expected, actual_after_insert=actual)
    landing_offset = locate_single_insertion_offset(baseline, actual, insertion)
    detail["actual_landing_source_offset"] = landing_offset
    if not matched:
        if actual == baseline:
            detail["landing_classification"] = "not_inserted"
            return "blocked", (
                "境界クリック挿入後も fixture が baseline のままで、probe が一切挿入されていない。"
                "OS click が editor 外へ外れた可能性があり、独立した座標証拠なしには"
                "helper/OCR miss と製品不具合を区別できないため procedure blocked とする"
            ), detail
        if landing_offset is None:
            detail["landing_classification"] = "corrupted"
            return "fail", (
                "境界クリック挿入後の内容が期待値と一致せず、単一文字挿入として"
                "着地点を特定できない(破損の疑い)"
            ), detail
        delta = abs(landing_offset - expected_offset)
        detail["landing_offset_delta"] = delta
        if delta > BOUNDARY_LANDING_FAR_MISS_CHARS:
            detail["landing_classification"] = "far_miss"
            return "blocked", (
                f"境界クリックの着地点が期待 canonical position から {delta} 文字離れており、"
                "OCR bounding box の誤差や helper 自身のクリック精度が意図した visual boundary へ"
                "到達できなかった疑いが強いため、製品 fail とは区別して procedure blocked とする"
            ), detail
        detail["landing_classification"] = "boundary_ambiguous_near_canonical"
        return "blocked", (
            f"境界クリックの着地点が期待 canonical position から {delta} 文字という近傍で一致しない。"
            "closing marker と後続 visible text が共有する境界での製品 source mapping の疑いはあるが、"
            "click_point も同じ OCR bounding box から算出されているため OS click が意図した visual "
            "boundary を指した独立証拠がなく、helper/OCR miss と区別できない。独立した座標検証"
            "(実 EditorView への coordinate-specific event 等)なしに製品 fail と断定せず、procedure "
            "blocked として #101 側での root-cause 切り分けを待つ"
        ), detail
    if landing_offset != expected_offset:
        detail["landing_classification"] = "corrupted"
        return "blocked", (
            "境界クリック挿入後の内容は期待バイト列と一致したが、baseline との差分から"
            "再算出した実着地点オフセットが期待 canonical position と一致しない"
            "(evidence 内部矛盾の疑い)"
        ), detail
    detail["landing_classification"] = "at_canonical"
    ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
    if not ok:
        return "blocked", (
            f"境界クリック挿入後の undo に失敗した(timeout、integrity mismatch、AppleScript の"
            f"実行失敗などの helper/実行環境側要因の可能性があり、undo の製品挙動を観測できて"
            f"いないため製品 fail と区別して procedure blocked とする): {err}"
        ), detail
    matched, actual = wait_for_fixture_bytes(fixture_path, baseline.encode("utf-8"), poll_timeout)
    detail.update(expected_after_undo=baseline, actual_after_undo=_decode(actual))
    if not matched:
        return "fail", "undo 後に元の内容へ戻らない", detail
    return "pass", None, detail


def confirm_boundary_edit_reproducibility(
    module, env, config, swift_helper, pid, window_id, run_dir, label,
    fixture_path, baseline, ocr_pattern, ocr_edge, source_pattern, source_edge, insertion,
    helper_timeout, poll_timeout, status, reason, detail,
) -> tuple[str, str, dict, list[dict]]:
    """far_miss / boundary_ambiguous_near_canonical で procedure blocked とした
    境界クリックについて、baseline へ復元したうえでもう一度 screenshot・OCR・クリックを
    やり直し、同じ非 canonical offset が再現するかどうかを確認する(Issue #137
    review: click_point が OCR bounding box の再計算値に過ぎず、正しい visual
    boundary をクリックした場合の製品 source mapping 不具合と OCR/クリック誤差を
    区別できないという指摘への対応)。ただし再試行も同じ OCR→座標算出の helper 経路を
    使うため、systematic な OCR bounding box のずれや helper 自身のクリック誤差が
    あれば、製品が正しくても毎回同じ間違った offset へ再現しうる(PR #139 review:
    fail への自動昇格は禁止)。したがって二回の着地点が一致しても、それは実
    EditorView への coordinate-specific event など OCR と独立した座標検証には
    ならないため、`reproducible_mismatch` として証拠に残すだけで procedure blocked
    のまま #101 側での root-cause 切り分けに委ね、fail へは昇格しない。

    OCR と独立した座標証拠は `run_coordinate_independent_probe` が別途提供する
    (Issue #137 review, PR #139: crates/** への恒久追加は禁止のため、使い捨ての
    SHA検証済みクローンへ `COORDINATE_PROBE_RUST_SOURCE` を一時注入して実行し、
    original bytes と clean tree を復元・証明したうえで破棄する)。実 EditorView・
    実 glyph shaping・実 mouse event で、閉じ marker の直後により可視テキストが
    続く境界(**combo** の後続、code span の閉じ backtick の後続、quote/list内の
    閉じ marker の後続など)が `Bias::After` の tie-break(後続の可視 segment を
    優先)で marker の手前ではなく直後に着地するなら、その probe 自身の `fail`
    として証拠に残る(`boundary_ambiguous_near_canonical` を OCR/helper 誤差と
    断定しないための独立確認であり、製品側の root cause 断定・恒久修正は #101 に
    委ねる)。"""
    steps: list[dict] = []
    restore_step = restore_scenario_baseline(
        swift_helper, pid, fixture_path, baseline, helper_timeout, poll_timeout,
        f"{label}_confirm_restore",
    )
    steps.append(restore_step)
    if restore_step["result"] != "pass":
        return status, (
            f"{reason} 再現確認のための baseline 復元に失敗したため、独立した再試行は"
            "行わず procedure blocked のままとする"
        ), detail, steps
    confirm_label = f"{label}_confirm"
    capture = capture_named(module, env, config, window_id, run_dir, confirm_label)
    steps.append(capture)
    if capture["result"] != "pass":
        return status, (
            f"{reason} 再現確認用の撮影に失敗したため、独立した再試行は行わず"
            "procedure blocked のままとする"
        ), detail, steps
    screenshot = run_dir / f"{confirm_label}.png"
    status2, reason2, detail2 = boundary_edit_check(
        swift_helper, screenshot, pid, fixture_path, baseline,
        ocr_pattern, ocr_edge, source_pattern, source_edge, insertion,
        helper_timeout, poll_timeout,
    )
    steps.append(make_step(f"{confirm_label}_check", status2, reason=reason2, **detail2))
    landing1 = detail.get("actual_landing_source_offset")
    landing2 = detail2.get("actual_landing_source_offset")
    if status2 == "pass":
        return status, (
            f"{reason} 再試行では canonical position に着地して再現しなかった"
            "ため、procedure blocked のままとする"
        ), detail, steps
    if landing1 is not None and landing2 is not None and landing1 == landing2:
        merged_detail = {
            **detail,
            "landing_classification": "reproducible_mismatch",
            "confirmation_landing_source_offset": landing2,
        }
        merged_reason = (
            f"2回の OCR/クリック試行がいずれも同じ source offset {landing1} へ着地し、"
            f"期待 canonical position {detail.get('expected_canonical_source_offset')} と一致しない。"
            "ただし両試行とも同じ OCR bounding box→座標算出の helper 経路を使っており、"
            "systematic な OCR/クリック誤差があれば製品が正しくても同じ間違った offset へ"
            "再現しうるため、この再現性だけでは実 EditorView への coordinate-specific event"
            "などの OCR と独立した座標証拠にならない。fail へは昇格せず、reproducible_mismatch"
            "として記録したうえで procedure blocked のまま #101 側での root-cause 切り分けに委ねる"
        )
        return status, merged_reason, merged_detail, steps
    return status, (
        f"{reason} 再試行でも別の非 canonical な着地点となり再現しなかったため、"
        "procedure blocked のままとする"
    ), detail, steps


def run_coordinate_independent_probe(env, module, snapshot: Path, timeout: float) -> dict:
    """OCR を一切経由しない独立した座標証拠を得るための probe(Issue #137
    review, PR #139)。`boundary_click_lands_on_source_offset_independent_of_ocr`
    という `#[gpui::test]` を、`env.snapshot_checkout` が用意した使い捨ての
    SHA検証済みクローンの `crates/ui/src/view.rs` へ一時的に注入して実行し、
    直後に original bytes と clean tree(`git status --porcelain`)を復元・証明
    したうえで破棄する。crates/** へ恒久追加すると、現在の(既知のバグかもしれない)
    挙動を "expected" として固定してしまい、この probe が独立に確認すべき製品側
    mismatch をテスト成功として吸収してしまう。

    復元・clean tree の証明ができなければ、cargo test の結果がどうであれ採用せず
    `blocked` として fail-closed にする。証明できた場合、cargo test の失敗
    (= 境界クリックの着地点が canonical position と一致しない)は OCR/helper 誤差
    ではあり得ない独立証拠として `fail` を返す。
    """
    name = "coordinate_independent_probe"
    view_rs = snapshot / "crates" / "ui" / "src" / "view.rs"
    try:
        original_bytes = view_rs.read_bytes()
    except OSError as exc:
        return make_step(name, "blocked", reason=f"probe 注入対象ファイルを読み込めない: {exc}")

    stripped = original_bytes.rstrip(b"\n")
    if not stripped.endswith(b"}"):
        return make_step(name, "blocked", reason="probe 注入位置(tests モジュール終端)を特定できない")
    insert_at = len(stripped) - 1
    injected = (
        original_bytes[:insert_at]
        + COORDINATE_PROBE_RUST_SOURCE.encode("utf-8")
        + original_bytes[insert_at:]
    )

    proc = None
    test_error = None
    try:
        view_rs.write_bytes(injected)
        args = [
            "cargo", "test", "--locked",
            "--manifest-path", str(snapshot / "Cargo.toml"),
            "-p", "hane-ui", "--lib", COORDINATE_PROBE_TEST_QUALIFIED_NAME,
            # `--nocapture`: libtest hides a test's stdout unless it fails, which
            # would silently drop every `COORDINATE_PROBE_CASE` evidence line on
            # the success path this probe exists to prove (Codex review, PR #139).
            "--", "--exact", "--nocapture",
        ]
        try:
            proc = subprocess.run(args, cwd=snapshot, capture_output=True, text=True, timeout=timeout)
        except subprocess.TimeoutExpired as exc:
            test_error = str(exc)
    finally:
        restored = False
        restored_bytes = None
        try:
            view_rs.write_bytes(original_bytes)
            restored_bytes = view_rs.read_bytes()
            restored = restored_bytes == original_bytes
        except OSError:
            restored = False
        dirty_paths = None
        clean = False
        if restored:
            try:
                dirty_paths = env.git_dirty_paths(snapshot)
                clean = dirty_paths == []
            except module.EnvError:
                clean = False

    # SHA-256 (rather than the in-process boolean comparison above alone) so the
    # validator can independently re-check restoration from the receipt without
    # trusting a self-reported `restored=True`, and the exact `git status
    # --porcelain` lines instead of a self-reported `clean_tree=True` boolean
    # (Codex review, PR #139: missing/tampered/dirty receipts must fail closed).
    original_sha256 = hashlib.sha256(original_bytes).hexdigest()
    restored_sha256 = hashlib.sha256(restored_bytes).hexdigest() if restored_bytes is not None else None
    restore_evidence = dict(
        restored=restored, clean_tree=clean,
        original_view_rs_sha256=original_sha256, restored_view_rs_sha256=restored_sha256,
        clean_tree_paths=dirty_paths,
    )

    if not restored or not clean:
        return make_step(
            name, "blocked",
            reason=(
                "probe 注入後に crates/ui/src/view.rs の original bytes と clean tree を"
                "復元・証明できなかったため、cargo test の結果を採用せず fail-closed とする"
            ),
            **restore_evidence,
        )
    if proc is None:
        return make_step(name, "blocked", reason=f"独立 probe の cargo test が完了しなかった: {test_error}")

    full_output = proc.stdout + proc.stderr
    output_tail = "\n".join(full_output.splitlines()[-80:])
    executed_match = re.search(r"^running (\d+) tests?$", full_output, re.MULTILINE)
    executed = int(executed_match.group(1)) if executed_match else 0
    if executed != 1:
        return make_step(
            name, "blocked",
            reason=(
                f"独立 probe が想定した1件の hit-test を実行しなかった(実行数: {executed})。"
                "test filter が対象テストに一致しなかった可能性があり、cargo test の"
                "結果を採用せず fail-closed とする"
            ),
            cargo_test_output=output_tail, tests_executed=executed,
        )
    # `output_tail` keeps only the log's last 80 lines for diagnostics, so a
    # normal hosted run's compiler/stderr noise can push the target test's own
    # success line out of it even when the test passed. Extract that line, and
    # each case's structured evidence, from the untruncated `full_output`
    # instead (Codex review, PR #139).
    target_line_match = COORDINATE_PROBE_TARGET_LINE_RE.search(full_output)
    target_test_line = target_line_match.group(0) if target_line_match else None
    probe_cases = None
    try:
        parsed_cases = [json.loads(raw) for raw in COORDINATE_PROBE_CASE_LINE_RE.findall(full_output)]
    except (ValueError, TypeError):
        parsed_cases = []
    if len(parsed_cases) == len(COORDINATE_PROBE_EXPECTED_CASES) and all(
        isinstance(case, dict) for case in parsed_cases
    ):
        probe_cases = parsed_cases
    if proc.returncode == 0:
        if target_test_line != f"test {COORDINATE_PROBE_TEST_QUALIFIED_NAME} ... ok" or probe_cases is None:
            return make_step(
                name, "blocked",
                reason=(
                    "独立 probe の cargo test が0終了したが、対象テストの成功行または8件の"
                    "ケース別 evidence 行を出力から確定的に抽出できなかったため、fail-closed とする"
                ),
                cargo_test_output=output_tail, tests_executed=executed, **restore_evidence,
            )
        return make_step(
            name, "pass", cargo_test_output=output_tail, tests_executed=executed,
            test_name=COORDINATE_PROBE_TEST_QUALIFIED_NAME, target_test_line=target_test_line,
            probe_cases=probe_cases, **restore_evidence,
        )
    if (COORDINATE_PROBE_FAILURE_MARKER in full_output
            and f"test {COORDINATE_PROBE_TEST_QUALIFIED_NAME} ... FAILED" in full_output):
        if target_test_line != f"test {COORDINATE_PROBE_TEST_QUALIFIED_NAME} ... FAILED" or probe_cases is None:
            return make_step(
                name, "blocked",
                reason=(
                    "独立 probe の cargo test が製品 source-mapping 不整合を示唆したが、対象テストの"
                    "失敗行または8件のケース別 evidence 行を出力から確定的に抽出できなかったため、"
                    "fail-closed とする"
                ),
                cargo_test_output=output_tail, tests_executed=executed, **restore_evidence,
            )
        return make_step(
            name, "fail",
            reason=(
                "OCR を経由しない独立 GPUI probe が、境界クリックの着地点が期待 canonical "
                "position と一致しない製品側 source mapping 不整合を確認した(Issue #101)"
            ),
            cargo_test_output=output_tail, tests_executed=executed,
            test_name=COORDINATE_PROBE_TEST_QUALIFIED_NAME, target_test_line=target_test_line,
            probe_cases=probe_cases, **restore_evidence,
        )
    return make_step(
        name, "blocked",
        reason=(
            "独立 probe の cargo test が非0終了したが、対象 probe の assertion failure を"
            "確認できなかった(コンパイルエラー・linker/toolchain 障害・Cargo.lock 問題など"
            "検証環境側の失敗の可能性があり、製品側 source-mapping 不整合と断定できない)"
        ),
        cargo_test_output=output_tail, tests_executed=executed,
    )


def run_boundary_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                      name, checks, helper_timeout, poll_timeout) -> list[dict]:
    pid = current_pid(process_holder)
    if pid is None or window_id is None:
        return [skipped_step(name, "対象プロセスの PID またはウィンドウを取得できなかった")]
    result: list[dict] = []
    for index, (ocr_pattern, ocr_edge, source_pattern, source_edge) in enumerate(checks):
        label = f"{name}_{index}"
        capture = capture_named(module, env, config, window_id, run_dir, label)
        result.append(capture)
        if capture["result"] != "pass":
            result.append(make_step(name, "blocked", reason=f"境界確認用の撮影に失敗した: {capture.get('reason')}"))
            return result
        screenshot = run_dir / f"{label}.png"
        status, reason, detail = boundary_edit_check(
            swift_helper, screenshot, pid, config.fixture_path, INLINE_FIXTURE_ORIGINAL,
            ocr_pattern, ocr_edge, source_pattern, source_edge, BOUNDARY_MARK,
            helper_timeout, poll_timeout,
        )
        if status == "blocked" and detail.get("landing_classification") in AMBIGUOUS_LANDING_CLASSIFICATIONS:
            status, reason, detail, confirm_steps = confirm_boundary_edit_reproducibility(
                module, env, config, swift_helper, pid, window_id, run_dir, label,
                config.fixture_path, INLINE_FIXTURE_ORIGINAL,
                ocr_pattern, ocr_edge, source_pattern, source_edge, BOUNDARY_MARK,
                helper_timeout, poll_timeout, status, reason, detail,
            )
        else:
            confirm_steps = []
        result.append(make_step(f"{name}_check_{index}", status, reason=reason, **detail))
        result.extend(confirm_steps)
        if status != "pass":
            result.append(make_step(name, status, reason=reason))
            return result
        reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_reset_{index}")
        result.append(reset)
        if reset["result"] != "pass":
            result.append(make_step(name, "blocked", reason=reset.get("reason")))
            return result
    result.append(make_step(name, "pass"))
    return result


def run_boundary_navigation_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                                 helper_timeout, poll_timeout) -> list[dict]:
    name = "boundary_caret_navigation"
    pid = current_pid(process_holder)
    if pid is None or window_id is None:
        return [skipped_step(name, "対象プロセスの PID またはウィンドウを取得できなかった")]
    expected_left = INLINE_FIXTURE_ORIGINAL.replace("**bold", f"*{NAVIGATION_MARK}*bold", 1)
    expected_right = INLINE_FIXTURE_ORIGINAL.replace("**bold", f"**b{NAVIGATION_MARK}old", 1)
    expected_up = INLINE_FIXTURE_ORIGINAL.replace("this inline `code", f"this{NAVIGATION_MARK} inline `code", 1)
    cases = (
        ("left_across_open_marker", BOLD_ITALIC_OCR_RE, "start", "left", expected_left),
        ("right_into_visible_text", BOLD_ITALIC_OCR_RE, "start", "right", expected_right),
        ("up_from_multiline_code_close", CODE_SPAN_CLOSE_OCR_RE, "end", "up", expected_up),
    )
    steps: list[dict] = []
    for index, (case_name, ocr_pattern, ocr_edge, direction, expected) in enumerate(cases):
        reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_reset_before_{index}")
        steps.append(reset)
        if reset["result"] != "pass":
            steps.append(make_step(name, "blocked", reason=reset.get("reason")))
            return steps
        label = f"{name}_{index}"
        capture = capture_named(module, env, config, window_id, run_dir, label)
        steps.append(capture)
        if capture["result"] != "pass":
            steps.append(make_step(name, "blocked", reason=capture.get("reason")))
            return steps
        screenshot = run_dir / f"{label}.png"
        ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(screenshot), ocr_pattern, ocr_edge], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"caret navigation の境界クリックに失敗した: {err}"))
            return steps
        ok, _out, err = run_helper(swift_helper, ["move-caret", str(pid), direction, "1"], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"caret {direction} 移動に失敗した: {err}"))
            return steps
        ok, _out, err = run_helper(swift_helper, ["type-save", str(pid), NAVIGATION_MARK], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"caret 移動後の着地点確認入力に失敗した: {err}"))
            return steps
        matched, actual = wait_for_fixture_bytes(config.fixture_path, expected.encode("utf-8"), poll_timeout)
        detail = {
            "case": case_name,
            "screenshot": str(screenshot),
            "direction": direction,
            "count": 1,
            "expected_after_move_insert": expected,
            "actual_after_move_insert": _decode(actual),
        }
        if not matched:
            steps.append(make_step(f"{name}_check_{index}", "fail", reason="caret 移動後の source 着地点が期待値と一致しない", **detail))
            steps.append(make_step(name, "fail", reason="caret 移動後の source 着地点が期待値と一致しない"))
            return steps
        ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"caret navigation undo に失敗した: {err}"))
            return steps
        restored, actual = wait_for_fixture_bytes(config.fixture_path, INLINE_FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout)
        detail.update(expected_after_undo=INLINE_FIXTURE_ORIGINAL, actual_after_undo=_decode(actual))
        steps.append(make_step(f"{name}_check_{index}", "pass" if restored else "fail",
                               reason=None if restored else "caret navigation undo 後に元の内容へ戻らない", **detail))
        if not restored:
            steps.append(make_step(name, "fail", reason="caret navigation undo 後に元の内容へ戻らない"))
            return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_reset_after")
    steps.append(reset)
    steps.append(make_step(name, "pass" if reset["result"] == "pass" else "blocked", reason=reset.get("reason")))
    return steps


def run_boundary_ime_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                          helper_timeout, poll_timeout) -> list[dict]:
    name = "boundary_ime_input"
    pid = current_pid(process_holder)
    if pid is None or window_id is None:
        return [skipped_step(name, "対象プロセスの PID またはウィンドウを取得できなかった")]
    steps: list[dict] = []
    original_source: Optional[str] = None
    try:
        ok, out, err = run_helper(swift_helper, ["current-source"], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=err))
            return steps
        original_source = out
        ok, out, err = run_helper(swift_helper, ["list-sources"], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=err))
            return steps
        available = [line for line in out.splitlines() if line.strip()]
        if JAPANESE_SOURCE not in available:
            steps.append(make_step(name, "blocked", reason="このランナーに組み込みの日本語入力ソースが見つからない"))
            return steps
        reset = _move_to_neutral(swift_helper, pid, helper_timeout, "boundary_ime_input_reset_before")
        steps.append(reset)
        if reset["result"] != "pass":
            steps.append(make_step(name, "blocked", reason=reset.get("reason")))
            return steps
        capture = capture_named(module, env, config, window_id, run_dir, "boundary_ime_input")
        steps.append(capture)
        if capture["result"] != "pass":
            steps.append(make_step(name, "blocked", reason=capture.get("reason")))
            return steps
        screenshot = run_dir / "boundary_ime_input.png"
        ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(screenshot), BOLD_ITALIC_OCR_RE, "start"], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"IME 境界クリックに失敗した: {err}"))
            return steps
        ok, _out, err = run_helper(swift_helper, ["type-romaji-at-caret-commit-save", str(pid), IME_ROMAJI, JAPANESE_SOURCE], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"境界 IME 入力に失敗した: {err}"))
            return steps
        expected = insert_at_match(INLINE_FIXTURE_ORIGINAL, BOLD_ITALIC_OPEN_RE, IME_EXPECTED_TEXT, edge="end")
        matched, actual = wait_for_fixture_bytes(config.fixture_path, expected.encode("utf-8"), poll_timeout)
        detail = {"screenshot": str(screenshot), "expected_after_insert": expected, "actual_after_insert": _decode(actual)}
        if not matched:
            steps.append(make_step("boundary_ime_input_check", "fail", reason="境界 IME 入力後の内容が期待値と一致しない", **detail))
            steps.append(make_step(name, "fail", reason="境界 IME 入力後の内容が期待値と一致しない"))
            return steps
        ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"境界 IME undo に失敗した: {err}"))
            return steps
        matched, actual = wait_for_fixture_bytes(config.fixture_path, INLINE_FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout)
        detail.update(expected_after_undo=INLINE_FIXTURE_ORIGINAL, actual_after_undo=_decode(actual))
        steps.append(make_step("boundary_ime_input_check", "pass" if matched else "fail",
                               reason=None if matched else "境界 IME undo 後に元の内容へ戻らない", **detail))
        if not matched:
            steps.append(make_step(name, "fail", reason="境界 IME undo 後に元の内容へ戻らない"))
            return steps
        reset = _move_to_neutral(swift_helper, pid, helper_timeout, "boundary_ime_input_reset_after")
        steps.append(reset)
        steps.append(make_step(name, "pass" if reset["result"] == "pass" else "blocked", reason=reset.get("reason")))
        return steps
    finally:
        if original_source:
            ok, _out, err = run_helper(swift_helper, ["select-source", original_source], helper_timeout)
            steps.append(make_step("restore_boundary_ime_input_source", "pass" if ok else "blocked",
                                   reason=None if ok else err, restored_source=original_source))
        else:
            steps.append(make_step("restore_boundary_ime_input_source", "blocked", reason="元の入力ソースを取得できなかった"))


def run_drag_select_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                         helper_timeout, poll_timeout) -> list[dict]:
    name = "drag_select_delete_undo_redo"
    pid = current_pid(process_holder)
    if pid is None or window_id is None:
        return [skipped_step(name, "対象プロセスの PID またはウィンドウを取得できなかった")]
    steps: list[dict] = []
    capture = capture_named(module, env, config, window_id, run_dir, "drag_select_state0")
    steps.append(capture)
    if capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=capture.get("reason")))
        return steps
    screenshot = run_dir / "drag_select_state0.png"
    ok, _out, err = run_helper(swift_helper, ["drag-select-text", str(pid), str(screenshot),
                                                DRAG_SELECT_START_OCR_RE, "start", DRAG_SELECT_END_OCR_RE, "end"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"ドラッグ選択に失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["delete-selection-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"選択範囲の削除に失敗した: {err}"))
        return steps
    deleted_expected = INLINE_FIXTURE_ORIGINAL.replace("old *italic* com", "", 1)
    matched, actual = wait_for_fixture_bytes(config.fixture_path, deleted_expected.encode("utf-8"), poll_timeout)
    detail = {"screenshot": str(screenshot), "deleted_expected": deleted_expected, "deleted_actual": _decode(actual)}
    if not matched:
        steps.append(make_step("drag_select_delete_undo_redo_check", "fail", reason="ドラッグ選択範囲の削除結果が一致しない", **detail))
        steps.append(make_step(name, "fail", reason="ドラッグ選択範囲の削除結果が一致しない"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"undo に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, INLINE_FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout)
    detail["undo_actual"] = _decode(actual)
    if not matched:
        steps.append(make_step("drag_select_delete_undo_redo_check", "fail", reason="undo 後に元の内容へ戻らない", **detail))
        steps.append(make_step(name, "fail", reason="undo 後に元の内容へ戻らない"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["redo-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"redo に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, deleted_expected.encode("utf-8"), poll_timeout)
    detail["redo_actual"] = _decode(actual)
    if not matched:
        steps.append(make_step("drag_select_delete_undo_redo_check", "fail", reason="redo 後に削除結果へ戻らない", **detail))
        steps.append(make_step(name, "fail", reason="redo 後に削除結果へ戻らない"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"最終 undo に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, INLINE_FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout)
    detail["restored_actual"] = _decode(actual)
    steps.append(make_step("drag_select_delete_undo_redo_check", "pass" if matched else "fail",
                           reason=None if matched else "最終 undo 後に元の内容へ戻らない", **detail))
    if not matched:
        steps.append(make_step(name, "fail", reason="最終 undo 後に元の内容へ戻らない"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, "drag_select_reset")
    steps.append(reset)
    steps.append(make_step(name, "pass" if reset["result"] == "pass" else "blocked", reason=reset.get("reason")))
    return steps


def run_delimiter_toggle_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                              kind: str, delimiter: str, helper_timeout, poll_timeout) -> list[dict]:
    name = f"delimiter_toggle_{kind}"
    pid = current_pid(process_holder)
    if pid is None or window_id is None:
        return [skipped_step(name, "対象プロセスの PID またはウィンドウを取得できなかった")]
    steps: list[dict] = []
    unclosed_expected, closed_expected = delimiter_states(delimiter)
    ok, _out, err = run_helper(swift_helper, ["end-doc-type-save", str(pid), f" {delimiter}loose{delimiter} tail"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"区切り記号の追加に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, closed_expected.encode("utf-8"), poll_timeout)
    if not matched:
        steps.append(make_step(name, "fail", reason=f"{delimiter!r} を追加した内容が期待値と一致しない: actual={_decode(actual)!r}"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_initial_reset")
    steps.append(reset)
    if reset["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=reset.get("reason")))
        return steps
    initial_capture = capture_named(module, env, config, window_id, run_dir, f"{name}_initial_closed")
    steps.append(initial_capture)
    if initial_capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=initial_capture.get("reason")))
        return steps
    initial_image = run_dir / f"{name}_initial_closed.png"
    initial_digest, digest_error = image_pixel_digest(swift_helper, initial_image, helper_timeout)
    if digest_error:
        steps.append(make_step(name, "blocked", reason=f"初期閉鎖状態の描画 digest 取得に失敗した: {digest_error}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(initial_image), r"loose", "end"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ区切り記号の位置決めに失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["shift-select", str(pid), "right", str(len(delimiter))], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ区切り記号の選択に失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["delete-selection-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ区切り記号の削除に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, unclosed_expected.encode("utf-8"), poll_timeout)
    unclosed_actual = _decode(actual)
    if not matched:
        steps.append(make_step(name, "fail", reason=f"{delimiter!r} 削除後の未閉鎖内容が期待値と一致しない"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_unclosed_reset")
    steps.append(reset)
    if reset["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=reset.get("reason")))
        return steps
    unclosed_capture = capture_named(module, env, config, window_id, run_dir, f"{name}_unclosed")
    steps.append(unclosed_capture)
    if unclosed_capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=f"未閉鎖状態の撮影に失敗した: {unclosed_capture.get('reason')}"))
        return steps
    unclosed_image = run_dir / f"{name}_unclosed.png"
    unclosed_digest, digest_error = image_pixel_digest(swift_helper, unclosed_image, helper_timeout)
    if digest_error:
        steps.append(make_step(name, "blocked", reason=f"未閉鎖状態の描画 digest 取得に失敗した: {digest_error}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(unclosed_image), r"loose", "end"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"再閉鎖位置の位置決めに失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["type-save", str(pid), delimiter], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ区切り記号の再入力に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, closed_expected.encode("utf-8"), poll_timeout)
    closed_actual = _decode(actual)
    if not matched:
        steps.append(make_step(name, "fail", reason=f"{delimiter!r} 再入力後の内容が期待値と一致しない"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_closed_reset")
    steps.append(reset)
    if reset["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=reset.get("reason")))
        return steps
    closed_capture = capture_named(module, env, config, window_id, run_dir, f"{name}_closed")
    steps.append(closed_capture)
    if closed_capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=f"閉鎖状態の撮影に失敗した: {closed_capture.get('reason')}"))
        return steps
    closed_image = run_dir / f"{name}_closed.png"
    closed_digest, digest_error = image_pixel_digest(swift_helper, closed_image, helper_timeout)
    if digest_error:
        steps.append(make_step(name, "blocked", reason=f"再閉鎖状態の描画 digest 取得に失敗した: {digest_error}"))
        return steps
    visual_transition = unclosed_digest != closed_digest
    visual_restored = initial_digest == closed_digest
    if not visual_transition or not visual_restored:
        reason = (
            "未閉鎖/再閉鎖で描画ピクセルが変化しない"
            if not visual_transition else "再閉鎖後の描画が初期閉鎖状態へ戻らない"
        )
        steps.append(make_step(
            f"{name}_check", "fail", reason=reason, delimiter=delimiter,
            initial_screenshot=str(initial_image), unclosed_screenshot=str(unclosed_image), closed_screenshot=str(closed_image),
            initial_pixel_digest=initial_digest, unclosed_pixel_digest=unclosed_digest, closed_pixel_digest=closed_digest,
            visual_transition_observed=visual_transition, closed_visual_restored=visual_restored,
            unclosed_expected=unclosed_expected, unclosed_actual=unclosed_actual,
            closed_expected=closed_expected, closed_actual=closed_actual,
        ))
        steps.append(make_step(name, "fail", reason=reason))
        return steps
    steps.append(make_step(
        f"{name}_check", "pass", delimiter=delimiter,
        initial_screenshot=str(initial_image), unclosed_screenshot=str(unclosed_image), closed_screenshot=str(closed_image),
        initial_pixel_digest=initial_digest, unclosed_pixel_digest=unclosed_digest, closed_pixel_digest=closed_digest,
        visual_transition_observed=True, closed_visual_restored=True,
        unclosed_expected=unclosed_expected, unclosed_actual=unclosed_actual,
        closed_expected=closed_expected, closed_actual=closed_actual,
    ))
    for index in range(3):
        ok, _out, err = run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
        if not ok:
            steps.append(make_step(name, "blocked", reason=f"区切り記号テストの復元 undo {index + 1} に失敗した: {err}"))
            return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, INLINE_FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout)
    if not matched:
        steps.append(make_step(name, "fail", reason=f"{delimiter!r} テスト後に元の内容へ戻らない: actual={_decode(actual)!r}"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_reset")
    steps.append(reset)
    steps.append(make_step(name, "pass" if reset["result"] == "pass" else "blocked", reason=reset.get("reason")))
    return steps


def run_multiline_code_span_toggle_step(module, env, config, swift_helper, process_holder, window_id, run_dir,
                                        helper_timeout, poll_timeout) -> list[dict]:
    name = "multiline_code_span_close_toggle"
    pid = current_pid(process_holder)
    if pid is None or window_id is None:
        return [skipped_step(name, "対象プロセスの PID またはウィンドウを取得できなかった")]
    steps: list[dict] = []
    unclosed_expected = INLINE_FIXTURE_ORIGINAL.replace("span`", "span", 1)
    closed_expected = INLINE_FIXTURE_ORIGINAL
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_initial_reset")
    steps.append(reset)
    if reset["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=reset.get("reason")))
        return steps
    initial_capture = capture_named(module, env, config, window_id, run_dir, f"{name}_initial_closed")
    steps.append(initial_capture)
    if initial_capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=initial_capture.get("reason")))
        return steps
    initial_image = run_dir / f"{name}_initial_closed.png"
    initial_digest, digest_error = image_pixel_digest(swift_helper, initial_image, helper_timeout)
    if digest_error:
        steps.append(make_step(name, "blocked", reason=f"初期閉鎖状態の描画 digest 取得に失敗した: {digest_error}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(initial_image), CODE_SPAN_CLOSE_OCR_RE, "end"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"複数行 code span の閉じ backtick の位置決めに失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["shift-select", str(pid), "right", "1"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ backtick の選択に失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["delete-selection-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ backtick の削除に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, unclosed_expected.encode("utf-8"), poll_timeout)
    unclosed_actual = _decode(actual)
    if not matched:
        steps.append(make_step(name, "fail", reason="複数行 code span の閉じ backtick 削除後の内容が期待値と一致しない"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_unclosed_reset")
    steps.append(reset)
    if reset["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=reset.get("reason")))
        return steps
    unclosed_capture = capture_named(module, env, config, window_id, run_dir, f"{name}_unclosed")
    steps.append(unclosed_capture)
    if unclosed_capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=f"未閉鎖状態の撮影に失敗した: {unclosed_capture.get('reason')}"))
        return steps
    unclosed_image = run_dir / f"{name}_unclosed.png"
    unclosed_digest, digest_error = image_pixel_digest(swift_helper, unclosed_image, helper_timeout)
    if digest_error:
        steps.append(make_step(name, "blocked", reason=f"未閉鎖状態の描画 digest 取得に失敗した: {digest_error}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["click-text", str(pid), str(unclosed_image), CODE_SPAN_CLOSE_OCR_RE, "end"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"再閉鎖位置の位置決めに失敗した: {err}"))
        return steps
    ok, _out, err = run_helper(swift_helper, ["type-save", str(pid), "`"], helper_timeout)
    if not ok:
        steps.append(make_step(name, "blocked", reason=f"閉じ backtick の再入力に失敗した: {err}"))
        return steps
    matched, actual = wait_for_fixture_bytes(config.fixture_path, closed_expected.encode("utf-8"), poll_timeout)
    closed_actual = _decode(actual)
    if not matched:
        steps.append(make_step(name, "fail", reason="閉じ backtick 再入力後の内容が期待値と一致しない"))
        return steps
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_closed_reset")
    steps.append(reset)
    if reset["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=reset.get("reason")))
        return steps
    closed_capture = capture_named(module, env, config, window_id, run_dir, f"{name}_closed")
    steps.append(closed_capture)
    if closed_capture["result"] != "pass":
        steps.append(make_step(name, "blocked", reason=f"再閉鎖状態の撮影に失敗した: {closed_capture.get('reason')}"))
        return steps
    closed_image = run_dir / f"{name}_closed.png"
    closed_digest, digest_error = image_pixel_digest(swift_helper, closed_image, helper_timeout)
    if digest_error:
        steps.append(make_step(name, "blocked", reason=f"再閉鎖状態の描画 digest 取得に失敗した: {digest_error}"))
        return steps
    visual_transition = unclosed_digest != closed_digest
    visual_restored = initial_digest == closed_digest
    if not visual_transition or not visual_restored:
        reason = (
            "複数行 code span の未閉鎖/再閉鎖で描画ピクセルが変化しない"
            if not visual_transition else "複数行 code span の再閉鎖後の描画が初期閉鎖状態へ戻らない"
        )
        steps.append(make_step(
            f"{name}_check", "fail", reason=reason,
            initial_screenshot=str(initial_image), unclosed_screenshot=str(unclosed_image), closed_screenshot=str(closed_image),
            initial_pixel_digest=initial_digest, unclosed_pixel_digest=unclosed_digest, closed_pixel_digest=closed_digest,
            visual_transition_observed=visual_transition, closed_visual_restored=visual_restored,
            unclosed_expected=unclosed_expected, unclosed_actual=unclosed_actual,
            closed_expected=closed_expected, closed_actual=closed_actual,
        ))
        steps.append(make_step(name, "fail", reason=reason))
        return steps
    steps.append(make_step(
        f"{name}_check", "pass",
        initial_screenshot=str(initial_image), unclosed_screenshot=str(unclosed_image), closed_screenshot=str(closed_image),
        initial_pixel_digest=initial_digest, unclosed_pixel_digest=unclosed_digest, closed_pixel_digest=closed_digest,
        visual_transition_observed=True, closed_visual_restored=True,
        unclosed_expected=unclosed_expected, unclosed_actual=unclosed_actual,
        closed_expected=closed_expected, closed_actual=closed_actual,
    ))
    reset = _move_to_neutral(swift_helper, pid, helper_timeout, f"{name}_reset")
    steps.append(reset)
    steps.append(make_step(name, "pass" if reset["result"] == "pass" else "blocked", reason=reset.get("reason")))
    return steps


def run_inline_syntax_scenario(module, env, target_dir, swift_helper, base_run_dir, binary_path,
                               expected_sha, request_id, startup_timeout, window_timeout,
                               helper_timeout, poll_timeout, priority) -> dict:
    run_dir = base_run_dir / "inline_syntax_boundary"
    fixture_path = run_dir / "inline-fixture.md"
    run_dir.mkdir(parents=True, exist_ok=True)
    fixture_path.write_text(INLINE_FIXTURE_ORIGINAL, encoding="utf-8")
    steps: list[dict] = []
    process_holder: dict = {"process": None}
    config = make_config(
        module, workspace_dir=target_dir, scenario="inline-syntax-boundary",
        expected_sha=expected_sha, request_id=request_id, generation="1", run_dir=run_dir,
        fixture_path=fixture_path, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
    )
    try:
        session_steps, window_id = open_session(module, env, config, binary_path, process_holder, "before")
        steps += session_steps

        boundary_click_checks = (
            ("boundary_click_edit_bold_italic", [
                (BOLD_ITALIC_OCR_RE, "start", BOLD_ITALIC_OPEN_RE, "end"),
                (BOLD_ITALIC_OCR_RE, "end", BOLD_ITALIC_CLOSE_RE, "start"),
            ]),
            ("boundary_click_edit_code_span", [
                (CODE_SPAN_OPEN_OCR_RE, "start", CODE_SPAN_OPEN_RE, "end"),
                (CODE_SPAN_CLOSE_OCR_RE, "end", CODE_SPAN_CLOSE_RE, "start"),
            ]),
            ("boundary_click_edit_quote", [(QUOTE_BOLD_OPEN_OCR_RE, "start", QUOTE_BOLD_OPEN_RE, "end")]),
            ("boundary_click_edit_list", [(LIST_ITALIC_OPEN_OCR_RE, "start", LIST_ITALIC_OPEN_RE, "end")]),
        )
        subtests = [
            (bname, (lambda bname=bname, bchecks=bchecks: run_boundary_step(
                module, env, config, swift_helper, process_holder, window_id, run_dir,
                bname, bchecks, helper_timeout, poll_timeout)))
            for bname, bchecks in boundary_click_checks
        ]
        subtests.append(("boundary_caret_navigation", lambda: run_boundary_navigation_step(
            module, env, config, swift_helper, process_holder, window_id, run_dir,
            helper_timeout, poll_timeout)))
        subtests.append(("boundary_ime_input", lambda: run_boundary_ime_step(
            module, env, config, swift_helper, process_holder, window_id, run_dir,
            helper_timeout, poll_timeout)))
        subtests.append(("drag_select_delete_undo_redo", lambda: run_drag_select_step(
            module, env, config, swift_helper, process_holder, window_id, run_dir,
            helper_timeout, poll_timeout)))
        for kind, delimiter in (("star", "*"), ("bold", "**"), ("code", "`")):
            subtests.append((f"delimiter_toggle_{kind}", (lambda kind=kind, delimiter=delimiter: run_delimiter_toggle_step(
                module, env, config, swift_helper, process_holder, window_id, run_dir,
                kind, delimiter, helper_timeout, poll_timeout))))
        subtests.append(("multiline_code_span_close_toggle", lambda: run_multiline_code_span_toggle_step(
            module, env, config, swift_helper, process_holder, window_id, run_dir,
            helper_timeout, poll_timeout)))

        # 独立シナリオの fail/blocked が次のシナリオへ漏れないよう、各 subtest 終了後に
        # baseline 復元を挟む(Issue #136)。復元自体が失敗したら以降を実行せず、
        # 元の fail/blocked step はそのまま残す(fail-closed)。
        continue_ok = True
        for sub_name, thunk in subtests:
            if not continue_ok:
                steps.append(skipped_step(sub_name, "直前の subtest の baseline 復元が失敗したため実行しない"))
                continue
            continue_ok = run_mutating_subtest(
                steps, thunk(), sub_name, process_holder, swift_helper,
                config.fixture_path, INLINE_FIXTURE_ORIGINAL, helper_timeout, poll_timeout,
            )

        if window_id is not None:
            steps.append(capture_named(module, env, config, window_id, run_dir, "after"))
    finally:
        steps.append(close_session(module, env, process_holder))

    if worst_result(steps, priority) != "pass":
        for name in ("launch_reopen", "window_discovery_reopen", "capture_reopen",
                     "visible_saved_text", "reopen_content_check", "cleanup_reopen"):
            steps.append(skipped_step(name, "再オープン前の工程が pass しなかった"))
        return {"name": "inline_syntax_boundary", "steps": steps,
                "result": worst_result(steps, priority), "reason": reason_for(steps),
                "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}

    expected_final = INLINE_FIXTURE_ORIGINAL
    reopen_dir = run_dir / "reopen"
    reopen_process_holder: dict = {"process": None}
    reopen_config = make_config(
        module, workspace_dir=target_dir, scenario="inline-syntax-boundary-reopen",
        expected_sha=expected_sha, request_id=request_id, generation="2", run_dir=reopen_dir,
        fixture_path=fixture_path, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
    )
    try:
        launch_step = module.do_launch(env, reopen_config, binary_path, reopen_process_holder)
        steps.append({**launch_step, "name": "launch_reopen"})
        reopen_window_id = None
        if launch_step["result"] != "pass":
            steps.append(skipped_step("window_discovery_reopen", "launch_reopen が pass しなかった"))
            steps.append(skipped_step("capture_reopen", "launch_reopen が pass しなかった"))
        else:
            window_step, reopen_window_id = module.do_window_discovery(env, reopen_config, reopen_process_holder["process"])
            steps.append({**window_step, "name": "window_discovery_reopen"})
            if window_step["result"] != "pass":
                steps.append(skipped_step("capture_reopen", "window_discovery_reopen が pass しなかった"))
            else:
                steps.append(capture_named(module, env, reopen_config, reopen_window_id, reopen_dir, "reopen"))
        steps.append(verify_visible_text(swift_helper, reopen_dir / "reopen.png", "bold italic combo", helper_timeout))
        matched, actual = wait_for_fixture_bytes(fixture_path, expected_final.encode("utf-8"), 1.0)
        steps.append(make_step(
            "reopen_content_check", "pass" if matched else "fail",
            reason=None if matched else "再オープン後もフィクスチャ内容が期待通りであることを確認できない",
            note="ファイル内容の一致は再オープンの証跡の一部に過ぎず、描画結果そのものの証明ではない",
            actual=_decode(actual),
        ))
    finally:
        cleanup_step = close_session(module, env, reopen_process_holder)
        steps.append({**cleanup_step, "name": "cleanup_reopen"})
    return {"name": "inline_syntax_boundary", "steps": steps,
            "result": worst_result(steps, priority), "reason": reason_for(steps),
            "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}


def env_float(name: str, default: float) -> float:
    value = os.environ.get(name)
    if not value:
        return default
    parsed = float(value)
    if not math.isfinite(parsed) or parsed <= 0:
        raise ValueError(f"{name} must be positive and finite")
    return parsed


def main() -> int:
    target_dir_value = os.environ.get("HANE_GUI_INTERACTION_TARGET_DIR")
    if not target_dir_value:
        print("HANE_GUI_INTERACTION_TARGET_DIR is required", file=sys.stderr)
        return 3
    target_dir = Path(target_dir_value).resolve()
    control_dir_value = os.environ.get("HANE_GUI_INTERACTION_CONTROL_DIR")
    control_dir = Path(control_dir_value).resolve() if control_dir_value else Path(__file__).resolve().parent.parent
    swift_helper_value = os.environ.get("HANE_GUI_INTERACTION_SWIFT_HELPER")
    swift_helper = Path(swift_helper_value).resolve() if swift_helper_value else control_dir / "scripts" / "hosted_gui_interaction.swift"
    if not swift_helper.is_file():
        print(f"swift input helper not found at {swift_helper}", file=sys.stderr)
        return 3
    base_run_dir = Path(os.environ.get("HANE_GUI_INTERACTION_RUN_DIR") or (Path(tempfile.gettempdir()) / "hane-gui-interaction"))
    base_run_dir.mkdir(parents=True, exist_ok=True)
    request_id = os.environ.get("HANE_GUI_INTERACTION_REQUEST_ID") or (time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-{os.getpid()}")
    expected_sha = os.environ.get("HANE_GUI_INTERACTION_EXPECTED_SHA", "")
    if not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        raise ValueError("full expected target SHA is required")
    startup_timeout = env_float("HANE_GUI_INTERACTION_STARTUP_TIMEOUT_SECS", 15.0)
    window_timeout = env_float("HANE_GUI_INTERACTION_WINDOW_TIMEOUT_SECS", 30.0)
    helper_timeout = env_float("HANE_GUI_INTERACTION_HELPER_TIMEOUT_SECS", 20.0)
    poll_timeout = env_float("HANE_GUI_INTERACTION_POLL_TIMEOUT_SECS", 10.0)
    coordinate_probe_timeout = env_float("HANE_GUI_INTERACTION_COORDINATE_PROBE_TIMEOUT_SECS", 600.0)
    os.chdir(target_dir)
    module = load_pinned_gui_validate(control_dir)
    env = module.RealEnvironment()
    priority = module.RESULT_PRIORITY
    lock_error = None
    helper_directory = tempfile.TemporaryDirectory(prefix="hane-trusted-helper-")
    try:
        env.acquire_execution()
    except module.EnvError as exc:
        lock_error = str(exc)
    try:
        started_at = env.clock.now_iso()
        preflight_config = make_config(
            module, workspace_dir=target_dir, scenario="hosted-gui-interaction-preflight",
            expected_sha=expected_sha, request_id=request_id, generation="0", run_dir=base_run_dir / "_preflight",
            fixture_path=None, features=["timing-probe"], extra_env={},
            startup_timeout=startup_timeout, window_timeout=window_timeout,
        )
        top_steps: list[dict] = []
        scenarios: list[dict] = []
        build_info: dict = {}
        target_info: dict = {}
        if lock_error:
            preflight_step = make_step("preflight", "blocked", reason=lock_error)
        else:
            preflight_step, target_info = module.do_preflight(env, preflight_config)
        top_steps.append(preflight_step)
        control_sha = "unknown"
        try:
            control_sha = env.git_head(control_dir)
        except module.EnvError as exc:
            top_steps.append(make_step("control_git_head", "blocked", reason=str(exc)))
        if preflight_step["result"] != "pass":
            top_steps.append(skipped_step("build", "preflight が pass しなかった"))
        else:
            try:
                swift_helper = prepare_helper(swift_helper, Path(helper_directory.name))
                top_steps.append(make_step("prepare_helper", "pass", sha256=swift_helper.digest))
            except (OSError, subprocess.SubprocessError) as exc:
                top_steps.append(make_step("prepare_helper", "blocked", reason=str(exc)))
            if isinstance(swift_helper, PreparedHelper):
                build_step, binary_path, build_info = module.do_build(env, preflight_config)
            else:
                build_step, binary_path = skipped_step("build", "trusted helper unavailable"), None
            top_steps.append(build_step)
            if build_step["result"] != "pass":
                binary_path = None
            if binary_path is not None:
                def handle_signal(signum, _frame):
                    raise module.Aborted(f"signal {signum}")
                previous = {sig: signal.signal(sig, handle_signal) for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT)}
                try:
                    for scenario_name, run_scenario in (
                        ("ascii_edit_save_undo_redo_reopen", run_ascii_scenario),
                        ("japanese_ime_input", run_ime_scenario),
                        ("os_scroll", run_scroll_scenario),
                        ("inline_syntax_boundary", run_inline_syntax_scenario),
                    ):
                        try:
                            scenarios.append(run_scenario(
                                module, env, target_dir, swift_helper, base_run_dir, binary_path,
                                expected_sha, request_id, startup_timeout, window_timeout,
                                helper_timeout, poll_timeout, priority,
                            ))
                        except (module.Aborted, Exception) as exc:
                            scenarios.append({"name": scenario_name, "result": "blocked", "reason": str(exc), "steps": []})
                            if isinstance(exc, module.Aborted):
                                break
                finally:
                    for sig, handler in previous.items():
                        signal.signal(sig, handler)
                # 使い捨てクローンへの一時注入は、直前の app scenario が使っていた
                # do_build のバイナリ用 snapshot を env が上書きしても安全な、
                # 全 scenario 終了後にだけ行う(Issue #137 review, PR #139)。
                try:
                    probe_snapshot = env.snapshot_checkout(target_dir, expected_sha)
                except module.EnvError as exc:
                    scenarios.append({
                        "name": "coordinate_independent_probe", "steps": [],
                        "result": "blocked", "reason": str(exc), "evidence": {},
                    })
                else:
                    probe_step = run_coordinate_independent_probe(
                        env, module, probe_snapshot, coordinate_probe_timeout,
                    )
                    scenarios.append({
                        "name": "coordinate_independent_probe", "steps": [probe_step],
                        "result": probe_step["result"], "reason": probe_step.get("reason"),
                        "evidence": {k: v for k, v in probe_step.items()
                                     if k not in ("name", "result", "reason")},
                    })
        scenario_results = [s["result"] for s in scenarios] or ["blocked"]
        all_results = [s["result"] for s in top_steps if s["result"] in priority] + [r for r in scenario_results if r in priority]
        overall_result = min(all_results, key=lambda r: priority[r]) if all_results else "blocked"
        reasons = [s.get("reason") for s in top_steps if s.get("reason") and s["result"] != "pass"]
        reasons += [s.get("reason") for s in scenarios if s.get("reason") and s["result"] != "pass"]
        overall_reason = "; ".join(r for r in reasons if r) or "すべての工程が成功した"
        result_doc = {
            "schema_version": SCHEMA_VERSION, "procedure_version": PROCEDURE_VERSION,
            "verification_kind": VERIFICATION_KIND, "request_id": request_id,
            "run_id": os.environ.get("GITHUB_RUN_ID", ""), "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT", ""),
            "started_at": started_at, "finished_at": env.clock.now_iso(), "target": target_info,
            "control": {"sha": control_sha},
            "runner": {"os": os.environ.get("RUNNER_OS", ""), "arch": os.environ.get("RUNNER_ARCH", ""),
                       "image_os": os.environ.get("ImageOS", ""), "image_version": os.environ.get("ImageVersion", ""),
                       "macos_version": platform.mac_ver()[0], "machine": platform.machine()},
            "build": build_info, "top_level_steps": top_steps, "scenarios": scenarios,
            "overall_result": overall_result, "overall_reason": overall_reason, "scope_note": SCOPE_NOTE,
        }
        label = {"pass": "PASS", "fail": "FAIL", "blocked": "BLOCKED"}[overall_result]
        result_doc["summary"] = f"[{label}] hosted-gui-interaction request={request_id} — {overall_reason} (対話スモーク確認。網羅的な GUI 検証ではない)"
        result_path = base_run_dir / "result.json"
        result_path.write_text(json.dumps(result_doc, indent=2, ensure_ascii=False) + "\n")
        (base_run_dir / "summary.md").write_text(result_doc["summary"] + "\n")
        print(result_doc["summary"])
        print(f"result: {result_path}")
        return EXIT_PASS if overall_result == "pass" else EXIT_NONPASS
    finally:
        env.release_execution()
        helper_directory.cleanup()


if __name__ == "__main__":
    sys.exit(main())

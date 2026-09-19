#!/usr/bin/env python3
"""Trusted focused GUI validation for normal list display (Issue #126).

This is deliberately a separate verification kind from hosted-gui-interaction/7
(scripts/hosted_gui_interaction.py) and from hosted-date-badge/N
(scripts/hosted_date_badge_gui.py). It proves only Issue #126's normal-list
display acceptance criteria:

  B. a mixed nested list (unordered item containing an ordered sublist, and
     an ordered item containing an unordered sublist) renders every item's
     content and preserves nesting order,
  C. a list item with multiple paragraphs and a nested child list still
     returns to the parent list's own numbering afterward,
  D. an ordered list whose source markers are not `1.` and are not
     sequential (e.g. `3.`, `41.`, `100.`) still displays sequential normal
     numbers starting at the first marker's value (3, 4, 5).

It also proves (E) normal-list-specific editing against real fixture file
bytes: direct edit of a source marker, direct edit of item body text, ASCII
input, Japanese IME commit at caret, ASCII selection replace, undo/redo,
save, and reopen after restart. Regardless of whether the IME step passes,
fails, or is blocked, the original input source is restored before the
following ASCII selection-replace step runs. Helper failure, an unavailable
input source, and OCR failure are never reported as pass.

It reuses the existing launcher/input stack (scripts/gui_validate.py and
scripts/hosted_gui_interaction.py, including its compiled
scripts/hosted_gui_interaction.swift OS-input helper) rather than inventing a
second one. It does not weaken or replace the comprehensive interactive-input
procedure. All visual assertions go through OCR text (`ocr`, `click-text`)
rather than internal rendering geometry, because product PR #189 is still
changing the internal list layout; this keeps the assertions modular against
that in-flight change. The source-marker direct edit, the selection-replace
edit, and every Japanese IME commit attempt do not rely on `click-text` at
all for caret positioning: a `click-text` bounding box's start/end is
evidence of where OCR read a glyph, not proof of the canonical source caret
offset, so landing a caret via a click and then a further fixed-count
keyboard move from that click can mis-attribute a one-character OCR landing
error as a product failure (PR #208 review PRRT_kwDOUETGuM6j8L1G). Those
three edits instead move the caret from `move-doc-start` by a canonical
source-text character offset computed with a pure function over the known
pristine fixture text (`EDITING_FIXTURE_ORIGINAL`), then act with
`move-caret`/`shift-select` only; `click-text`/OCR is used, if at all, only
to capture disclosure evidence, never to compute a caret position.
"""

from __future__ import annotations

import importlib.util
import json
import os
import platform
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "hosted-normal-list/4"
VERIFICATION_KIND = "normal_list_focused"
SCOPE_NOTE = (
    "Issue #126 の normal list 表示(B: ネスト混在、C: 複数段落+子リスト+親リストへの復帰、"
    "D: 非1・非連番 ordered ソースの連番表示)だけを検証する focused GUI evidence。"
    "hosted-gui-interaction/7 や hosted-date-badge の包括的/専用 GUI 検証合格を意味しない。"
    "product PR #189 でリスト内部レイアウトが変更され続けている間、固定ピクセル座標ではなく "
    "OCR テキストと、同一 screenshot 内での click-text bounding box の相対位置に限定し、"
    "内部レイアウト型には依存しない。B の各項目は OCR 行順序が期待した top-to-bottom の"
    "項目順と一致することを確認し、nested ordered items・nested bullet items がそれぞれ"
    "実際に右へインデントされていることを、同一 screenshot 内で同種 marker のトップレベル行"
    "(baseline)と比較した本文 x 位置から確認する。marker 種別が異なる親の本文 x をそのまま"
    "基準にすると、bullet と ordered の marker 幅の差だけで child_x が親より右になり得るため、"
    "child と同じ marker 種別を持つトップレベル行を baseline として使い、marker 幅の差を"
    "打ち消したうえで実際の構造上の横オフセットだけを判定する。Alpha 側(bullet 親 → ordered"
    "子)・Gamma 側(ordered 親 → bullet 子)は独立に検証し、いずれか一方だけが正しくても"
    "pass にしない。C は段落順の"
    "OCR 行順序に加え、nested child list の後に親 ordered list の番号(2)が復帰することを"
    "normal display 自体の番号表示で確認する。marker disclosure/reclosure 検証に加えて、"
    "通常リスト固有の source marker/本文の直接編集、ASCII 入力、日本語 IME 確定、選択置換、"
    "Undo/Redo、保存、再起動後の再オープンを実ファイルの byte 列で検証する(E)。source marker の"
    "直接編集・選択置換・IME 確定の caret 位置決めは、いずれも click-text の bounding box に"
    "依存しない(click-text の start/end は canonical source caret 位置の証拠ではなく、本文"
    "クリック後の固定文字数移動は1文字の OCR 着地誤差でも正常製品を fail にし得るため、PR #208"
    "review PRRT_kwDOUETGuM6j8L1G)。既知の pristine fixture テキストから純粋関数で求めた"
    "canonical source offset まで、document start から move-caret/shift-select だけで"
    "決定的に移動し、click-text/OCR は使ってもマーカー disclosure の evidence capture に"
    "限る。IME 工程の各 attempt も同じ canonical offset までの keyboard 移動を、pristine"
    "baseline 復元後に毎回やり直す。IME 工程は成功・失敗にかかわらず、後続の ASCII 選択置換の"
    "前に元の入力ソースへ復元する。helper failure・入力ソース未提供・OCR/click-text failure は"
    "いずれも pass として扱わない。テキストの存在だけでは順序・位置の証跡として扱わない。"
)
EXIT_PASS = 0
EXIT_NONPASS = 1
ASCII_INPUT_SOURCE = "com.apple.keylayout.ABC"

def load_module(control_dir: Path, relative: str, name: str):
    sys.dont_write_bytecode = True
    module_path = control_dir / relative
    spec = importlib.util.spec_from_file_location(name, module_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load trusted module: {module_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def step(name: str, result: str, reason: Optional[str] = None, **detail) -> dict:
    return {"name": name, "result": result, "reason": reason, **detail}


def skipped_step(name: str, reason: str) -> dict:
    return {"name": name, "result": "skipped", "reason": reason}


def worst(steps: list, priority: dict) -> str:
    values = [item["result"] for item in steps if item.get("result") in priority]
    return min(values, key=lambda value: priority[value]) if values else "blocked"


def reason_for(steps: list) -> str:
    reasons = [item.get("reason") for item in steps if item.get("result") != "pass" and item.get("reason")]
    return "; ".join(reasons) if reasons else "すべての工程が成功した"


def normalized_ocr_text(value: str) -> str:
    return re.sub(r"\s+", " ", value).strip()


# ---------------------------------------------------------------------------
# Pure source-byte transforms and OCR/step decision logic. No subprocess, no
# filesystem, no GUI: these are exercised directly by scripts/tests/.
# ---------------------------------------------------------------------------


def expected_sequential_numbers(seed: int, count: int) -> list:
    """The CommonMark ordered-list numbering rule: only the first marker's own
    number sets the start; every following item's own source number is
    ignored for numbering purposes and normal display always increments by 1.
    """
    if count <= 0:
        raise ValueError("count must be positive")
    return [seed + offset for offset in range(count)]


def leading_number_pattern(number: int, content_pattern: str) -> str:
    return rf"(?<!\d){number}\s*[.)]?\s*{content_pattern}"


def line_has_number_token(line: str, number: int) -> bool:
    return re.search(rf"(?<!\d){number}(?!\d)", line) is not None


def find_line(lines: list, pattern: str) -> Optional[str]:
    for line in lines:
        if re.search(pattern, line):
            return line
    return None


def find_ordered_line_indices(lines: list, patterns: list) -> Optional[list]:
    """patterns を先頭から順に、直前に一致した行より後ろの行だけを探して一致させる。

    途中で一致しない pattern があれば None を返す(未検出と、後ろへ探しても
    見つからない=期待順序で出現していない、の両方を区別しない fail-closed な
    判定)。同じ行が複数 pattern に一致することは想定していない。
    """
    indices = []
    cursor = 0
    for pattern in patterns:
        found = None
        for idx in range(cursor, len(lines)):
            if re.search(pattern, lines[idx]):
                found = idx
                break
        if found is None:
            return None
        indices.append(found)
        cursor = found + 1
    return indices


def evaluate_display_order(lines: list, patterns: list) -> dict:
    indices = find_ordered_line_indices(lines, patterns)
    if indices is None:
        return step(
            "initial_display_order", "fail",
            "normal display の OCR 行順序が期待した top-to-bottom の項目順と一致しない"
            "(項目が入れ替わっているか、いずれかの項目が確認できない)",
            expected_patterns=list(patterns), ocr_lines=lines,
        )
    return step(
        "initial_display_order", "pass",
        expected_patterns=list(patterns), matched_line_indices=indices,
    )


def evaluate_nested_position(label: str, child_label: str, baseline_evidence: dict, child_evidence: dict) -> dict:
    """nested な子項目本文の click-text bounding box x 位置(minX)を、同一 screenshot 内に
    ある「子と同じ marker 種別を持つトップレベル行」(baseline)の x 位置と比較する。

    bullet marker(例: "-")と ordered marker(例: "1.")は marker glyph 自体の幅が異なるため、
    marker 種別が異なる親の本文 x をそのまま基準にすると、実際には平坦(非ネスト)でも
    marker 幅の差だけで child_x が親より右に出て、ネスト崩れを誤って pass にできてしまう
    (Issue #126 review, discussion_r4051650816)。baseline は child と同じ marker 種別の
    トップレベル行なので、marker 幅の差はキャンセルされ、x の差は実際の構造上のネスト
    オフセットだけを反映する。固定ピクセル座標には依存しない。
    """
    name = f"nested_position_{label}_{child_label}"
    baseline_x = baseline_evidence["bounding_box"]["minX"]
    child_x = child_evidence["bounding_box"]["minX"]
    detail = {"baseline_x": baseline_x, "child_x": child_x}
    if child_x <= baseline_x:
        return step(
            name, "fail",
            f"nested な '{child_label}' の x 位置が同種 marker のトップレベル基準行以下で、"
            "右側へインデントされたネストを確認できない",
            **detail,
        )
    return step(name, "pass", **detail)


def parse_click_evidence(stdout: str) -> dict:
    """click-text の stdout(JSON 1行)から OCR/クリック evidence を取り出す。

    hosted_gui_interaction.py の同名関数と同じ契約(必須フィールド欠落や非 JSON は
    helper 自身の契約違反として ValueError にし、呼び出し側で製品 fail と区別できる
    procedure blocked として扱わせる)。
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


# ---------------------------------------------------------------------------
# Fixtures (#126 B/C/D). Deterministic and self-contained: no dependency on
# the current date, unlike the sidebar date-badge fixtures.
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class InitialCheck:
    label: str
    content_pattern: str
    expected_number: Optional[int] = None
    forbidden_number: Optional[int] = None


@dataclass(frozen=True)
class MarkerCheck:
    content_pattern: str
    raw_marker_pattern: str
    normalized_pattern: Optional[str] = None


@dataclass(frozen=True)
class NestedPositionCheck:
    label: str
    baseline_content_pattern: str
    child_content_patterns: tuple


@dataclass(frozen=True)
class ScenarioSpec:
    name: str
    fixture_filename: str
    fixture_text: str
    initial_checks: tuple
    marker_check: MarkerCheck
    nested_position_checks: tuple = ()


def build_mixed_nested_list_fixture() -> str:
    return (
        "# Mixed Nested List Fixture\n"
        "\n"
        "- Alpha lead item\n"
        "  1. Alpha nested one\n"
        "  2. Alpha nested two\n"
        "- Beta lead item\n"
        "\n"
        "1. Gamma lead first\n"
        "   - Gamma nested bullet\n"
        "   - Gamma nested second\n"
        "2. Gamma lead second\n"
    )


MIXED_NESTED_LIST_SPEC = ScenarioSpec(
    name="mixed_nested_list",
    fixture_filename="normal-list-mixed-nested.md",
    fixture_text=build_mixed_nested_list_fixture(),
    initial_checks=(
        InitialCheck("alpha_lead", r"Alpha lead item"),
        InitialCheck("alpha_nested_one", r"Alpha nested one"),
        InitialCheck("alpha_nested_two", r"Alpha nested two"),
        InitialCheck("beta_lead", r"Beta lead item"),
        InitialCheck("gamma_lead_first", r"Gamma lead first"),
        InitialCheck("gamma_nested_bullet", r"Gamma nested bullet"),
        InitialCheck("gamma_nested_second", r"Gamma nested second"),
        InitialCheck("gamma_lead_second", r"Gamma lead second"),
    ),
    marker_check=MarkerCheck(
        content_pattern=r"Beta lead item",
        raw_marker_pattern=r"-\s*Beta lead item",
    ),
    # baseline_content_pattern は各 check の子と同じ marker 種別を持つトップレベル行を選ぶ。
    # Alpha の子は ordered marker("1."/"2.")なので、同じ ordered marker のトップレベル行
    # "Gamma lead first" を baseline にする(bullet の "Alpha lead item" 自身を基準にすると
    # marker 幅の差で誤判定しうる)。Gamma の子は bullet marker("-")なので、同じ bullet
    # marker のトップレベル行 "Beta lead item" を baseline にする。
    nested_position_checks=(
        NestedPositionCheck(
            label="alpha", baseline_content_pattern=r"Gamma lead first",
            child_content_patterns=(r"Alpha nested one", r"Alpha nested two"),
        ),
        NestedPositionCheck(
            label="gamma", baseline_content_pattern=r"Beta lead item",
            child_content_patterns=(r"Gamma nested bullet", r"Gamma nested second"),
        ),
    ),
)


def build_paragraph_child_list_fixture() -> str:
    return (
        "# Paragraph Child List Fixture\n"
        "\n"
        "1. First item intro line\n"
        "\n"
        "   First item continued paragraph.\n"
        "\n"
        "   - Child bullet one\n"
        "   - Child bullet two\n"
        "\n"
        "2. Second item after return\n"
    )


PARAGRAPH_CHILD_LIST_SPEC = ScenarioSpec(
    name="paragraph_child_list",
    fixture_filename="normal-list-paragraph-child.md",
    fixture_text=build_paragraph_child_list_fixture(),
    initial_checks=(
        InitialCheck("first_intro", r"First item intro line"),
        InitialCheck("first_continued", r"First item continued paragraph\."),
        InitialCheck("child_bullet_one", r"Child bullet one"),
        InitialCheck("child_bullet_two", r"Child bullet two"),
        InitialCheck("second_after_return", r"Second item after return", expected_number=2),
    ),
    marker_check=MarkerCheck(
        content_pattern=r"Second item after return",
        raw_marker_pattern=r"2\.\s*Second item after return",
    ),
)


NONSEQUENTIAL_ORDERED_RAW_NUMBERS = (3, 41, 100)
NONSEQUENTIAL_ORDERED_EXPECTED_NUMBERS = tuple(
    expected_sequential_numbers(NONSEQUENTIAL_ORDERED_RAW_NUMBERS[0], len(NONSEQUENTIAL_ORDERED_RAW_NUMBERS))
)


def build_nonsequential_ordered_fixture() -> str:
    return (
        "# Nonsequential Ordered List Fixture\n"
        "\n"
        f"{NONSEQUENTIAL_ORDERED_RAW_NUMBERS[0]}. Alpha row\n"
        f"{NONSEQUENTIAL_ORDERED_RAW_NUMBERS[1]}. Beta row\n"
        f"{NONSEQUENTIAL_ORDERED_RAW_NUMBERS[2]}. Gamma row\n"
    )


NONSEQUENTIAL_ORDERED_SPEC = ScenarioSpec(
    name="nonsequential_ordered_list",
    fixture_filename="normal-list-nonsequential-ordered.md",
    fixture_text=build_nonsequential_ordered_fixture(),
    initial_checks=(
        InitialCheck("alpha_row", r"Alpha row", expected_number=NONSEQUENTIAL_ORDERED_EXPECTED_NUMBERS[0]),
        InitialCheck(
            "beta_row", r"Beta row",
            expected_number=NONSEQUENTIAL_ORDERED_EXPECTED_NUMBERS[1],
            forbidden_number=NONSEQUENTIAL_ORDERED_RAW_NUMBERS[1],
        ),
        InitialCheck(
            "gamma_row", r"Gamma row",
            expected_number=NONSEQUENTIAL_ORDERED_EXPECTED_NUMBERS[2],
            forbidden_number=NONSEQUENTIAL_ORDERED_RAW_NUMBERS[2],
        ),
    ),
    marker_check=MarkerCheck(
        content_pattern=r"Beta row",
        raw_marker_pattern=rf"{NONSEQUENTIAL_ORDERED_RAW_NUMBERS[1]}\.\s*Beta row",
        normalized_pattern=rf"{NONSEQUENTIAL_ORDERED_EXPECTED_NUMBERS[1]}\.\s*Beta row",
    ),
)


ALL_SCENARIO_SPECS = (MIXED_NESTED_LIST_SPEC, PARAGRAPH_CHILD_LIST_SPEC, NONSEQUENTIAL_ORDERED_SPEC)


# ---------------------------------------------------------------------------
# Editing fixture (#126 E, hosted-normal-list/3): source marker/body direct
# edit, ASCII input, Japanese IME commit, selection replace, undo/redo, save
# and reopen-after-restart, all verified against real fixture file bytes
# rather than OCR text. Pure and self-contained: no GUI/subprocess here.
# ---------------------------------------------------------------------------


def single_line_replacement(text: str, old_line: str, new_line: str) -> str:
    """text 内に old_line がちょうど1回だけ現れることを確認したうえで new_line に置き換える。

    editing fixture の期待 byte 列を構築するためだけに使う純粋関数。occurrences が
    0または2以上の場合は fixture 自体の前提が崩れているとみなし fail-closed で例外にする。
    """
    count = text.count(old_line)
    if count != 1:
        raise ValueError(f"expected exactly one occurrence of {old_line!r}, found {count}")
    return text.replace(old_line, new_line, 1)


def match_insertion_offset(text: str, pattern: str, *, edge: str) -> int:
    """hosted_gui_interaction.py の同名関数と同じ契約の純粋関数(意図的な複製)。

    この module は GUI 実行時にしか import できない hosted_gui_interaction.py の
    実装詳細に純粋計算を依存させず、scripts/tests/ から直接ユニットテストできる
    ように自己完結させる(parse_click_evidence と同じ方針)。
    """
    match = re.search(pattern, text)
    if not match:
        raise ValueError(f"pattern not found in expected fixture text: {pattern}")
    return match.end() if edge == "end" else match.start()


def insert_at_match(text: str, pattern: str, insertion: str, *, edge: str) -> str:
    position = match_insertion_offset(text, pattern, edge=edge)
    return text[:position] + insertion + text[position:]


def caret_right_count_from_doc_start(text: str, pattern: str, *, edge: str) -> int:
    """text の document start(offset 0)から pattern/edge の位置まで、`move-caret right`で
    何文字キーボード移動すれば到達するかを純粋に計算する。match_insertion_offset は文字列
    offset なので、document start が常に offset 0 である move-doc-start 直後の caret からは、
    その offset 自体がそのまま右移動の回数になる。pattern が見つからない場合は
    match_insertion_offset が ValueError にする(fail-closed)。click-text/OCR には一切
    依存しない(PR #208 review PRRT_kwDOUETGuM6j8L1G)。
    """
    return match_insertion_offset(text, pattern, edge=edge)


EDITING_FIXTURE_FILENAME = "normal-list-editing.md"
EDITING_FIXTURE_ORIGINAL = (
    "# Normal List Editing Fixture\n"
    "\n"
    "3. Alpha row\n"
    "41. Beta row\n"
    "100. Gamma row\n"
)

# 本文の直接編集(ASCII): "Beta row" の末尾に直接クリックして ASCII 文字列を追記する。
BODY_DIRECT_EDIT_OCR_PATTERN = r"Beta row"
BODY_DIRECT_EDIT_INSERTION = " Z"
EDITING_AFTER_BODY_DIRECT_EDIT = single_line_replacement(
    EDITING_FIXTURE_ORIGINAL, "41. Beta row", "41. Beta row Z"
)

# source marker の直接編集(ASCII): document start から raw marker "41." の先頭の
# canonical source offset まで move-caret right だけで決定的に移動して ASCII 文字を挿入する。
# 本文クリックからの固定文字数移動には依存しない(click-text の bounding box start/end は
# canonical source caret 位置の証拠ではなく、1文字の OCR 着地誤差でも正常製品を fail に
# しうる。PR #208 review PRRT_kwDOUETGuM6j8L1G)。raw marker disclosure の screenshot は
# evidence として撮るだけで、そこへの click-text/OCR には依存しない。
MARKER_DIRECT_EDIT_RAW_PATTERN = r"41\.\s*Beta row"
MARKER_DIRECT_EDIT_INSERTION = "9"
MARKER_DIRECT_EDIT_CARET_RIGHT_COUNT = caret_right_count_from_doc_start(
    EDITING_FIXTURE_ORIGINAL, MARKER_DIRECT_EDIT_RAW_PATTERN, edge="start",
)
EDITING_AFTER_MARKER_DIRECT_EDIT = single_line_replacement(
    EDITING_FIXTURE_ORIGINAL, "41. Beta row", "941. Beta row"
)

# 日本語 IME 確定(caret 位置): pristine baseline 復元直後の document start から "Alpha row"
# 末尾の canonical source offset まで move-caret right だけで決定的に移動してから、ローマ字
# 入力から変換・確定した日本語文字列を挿入する。click-text/OCR には依存しない(上記と同じ
# PR #208 review PRRT_kwDOUETGuM6j8L1G の理由により、各 attempt で同じ誤帰属を避ける)。
IME_COMMIT_OCR_PATTERN = r"Alpha row"
IME_COMMIT_ROMAJI = "nihongo"
IME_COMMIT_EXPECTED_TEXT = "日本語"
IME_COMMIT_CARET_RIGHT_COUNT = caret_right_count_from_doc_start(
    EDITING_FIXTURE_ORIGINAL, IME_COMMIT_OCR_PATTERN, edge="end",
)
EDITING_AFTER_IME_COMMIT = single_line_replacement(
    EDITING_FIXTURE_ORIGINAL, "3. Alpha row", "3. Alpha row日本語"
)

# TISSelectInputSource() が currentSourceID() に日本語ソースを反映させたことは、Hane 側の
# IME session が実際に変換可能な状態になったことを証明しない(Issue #126 post-merge GUI
# run: currentSourceID() は既に日本語ソースを報告していたのに romaji が変換されず未変換の
# ままだった、PR #208 review PRRT_kwDOUETGuM6j8CB_)。readiness の観測値は「保存された実
# バイトが期待する日本語へ変換・確定されたか」に置き換え、それが揃うまで pristine baseline
# (実ファイルとアプリ内状態の両方)への復元・caret 再配置・単一 helper invocation での
# source選択・変換・確定・保存を最初からやり直す。無条件リトライや固定 sleep だけで pass に
# しないよう、この小さな固定上限までにどの attempt でも実バイトが一致しなければ fail とする。
IME_COMMIT_MAX_ATTEMPTS = 3

# 選択置換(ASCII) + Undo/Redo: document start から "Gamma" 先頭の canonical source offset
# まで move-caret right だけで決定的に移動し、そこから Shift+→ で正確に
# SELECTION_REPLACE_WORD_LENGTH 文字だけキーボード選択して "Delta" で置換する。OCR
# bounding box の start→end drag は文字境界の真値ではなく、末尾の文字を取りこぼすことが
# ある(Issue #126 post-merge GUI run 35410838091: "Gamma" の選択が "Gamm" までしか及ばず
# "Deltaa row" になった)し、click-text の bounding box start/end 自体も canonical source
# caret 位置の証拠ではない(PR #208 review PRRT_kwDOUETGuM6j8L1G)ため、選択開始位置も選択
# 範囲もキーボード操作だけで決定的に確定する。click-text/OCR には一切依存しない。
SELECTION_REPLACE_WORD = "Gamma"
SELECTION_REPLACE_WORD_PATTERN = r"Gamma"
SELECTION_REPLACE_WORD_LENGTH = len(SELECTION_REPLACE_WORD)
SELECTION_REPLACE_CARET_RIGHT_COUNT = caret_right_count_from_doc_start(
    EDITING_FIXTURE_ORIGINAL, SELECTION_REPLACE_WORD_PATTERN, edge="start",
)
SELECTION_REPLACE_REPLACEMENT = "Delta"
EDITING_AFTER_SELECTION_REPLACE = single_line_replacement(
    EDITING_FIXTURE_ORIGINAL, "100. Gamma row", "100. Delta row"
)

# 再起動直前に実ファイルへ残す通常リストの変更済み状態と、再オープン後にその変更を
# 識別するための OCR 手がかり。EDITING_FIXTURE_ORIGINAL ではなく選択置換後の編集済み
# 状態そのものを再オープン検証の期待値にすることで、変更済みリストの復元が壊れていて
# 元の内容のまま復元される製品を pass にしない。
REOPEN_EXPECTED_BYTES = EDITING_AFTER_SELECTION_REPLACE
REOPEN_VISIBLE_TEXT = "Delta row"


# ---------------------------------------------------------------------------
# Fail-closed OCR/step evaluation (pure, given already-fetched OCR lines).
# ---------------------------------------------------------------------------


def evaluate_initial_check(lines: list, check: InitialCheck) -> dict:
    line = find_line(lines, check.content_pattern)
    if line is None:
        return step(
            f"initial_{check.label}", "fail",
            f"normal display で '{check.content_pattern}' の内容を OCR で確認できない",
            content_pattern=check.content_pattern, ocr_lines=lines,
        )
    detail = {"content_pattern": check.content_pattern, "matched_line": line}
    if check.expected_number is not None:
        if not re.search(leading_number_pattern(check.expected_number, check.content_pattern), line):
            return step(
                f"initial_{check.label}", "fail",
                f"normal display の番号が期待した連番 {check.expected_number} になっていない",
                expected_number=check.expected_number, **detail,
            )
        detail["expected_number"] = check.expected_number
    if check.forbidden_number is not None and line_has_number_token(line, check.forbidden_number):
        return step(
            f"initial_{check.label}", "fail",
            f"normal display に生のソース番号 {check.forbidden_number} がそのまま表示されている(連番化されていない)",
            forbidden_number=check.forbidden_number, **detail,
        )
    return step(f"initial_{check.label}", "pass", **detail)


def evaluate_marker_disclosure(after_lines: list, check: MarkerCheck) -> dict:
    after_line = find_line(after_lines, check.raw_marker_pattern)
    if after_line is None:
        return step(
            "marker_disclosure", "fail",
            "マーカー付近をクリックしても生の source marker が同じ行で確認できない(disclosure が機能していない疑い)",
            raw_marker_pattern=check.raw_marker_pattern, ocr_lines_after=after_lines,
        )
    return step(
        "marker_disclosure", "pass",
        raw_marker_pattern=check.raw_marker_pattern, matched_line_after=after_line,
    )


def evaluate_marker_reclosure(after_lines: list, check: MarkerCheck) -> dict:
    """Only meaningful when raw source marker and normal display differ.

    #126 D (non-1/non-sequential ordered source) is the only fixture where a
    raw source marker (e.g. `41.`) and its normal-display number (`4.`)
    necessarily differ, so only that fixture's `MarkerCheck` sets
    `normalized_pattern`; otherwise this reports `skipped` rather than
    asserting something the fixture cannot distinguish.
    """
    if check.normalized_pattern is None:
        return skipped_step("marker_reclosure", "raw marker と normal display の表示が区別できない fixture のため対象外")
    still_raw = find_line(after_lines, check.raw_marker_pattern)
    if still_raw is not None:
        return step(
            "marker_reclosure", "fail",
            "caret を離した後も生の source marker が表示されたままで、連番化された表示へ戻っていない",
            raw_marker_pattern=check.raw_marker_pattern, matched_line=still_raw,
        )
    normalized_line = find_line(after_lines, check.normalized_pattern)
    if normalized_line is None:
        return step(
            "marker_reclosure", "fail",
            "caret を離した後に連番化された normal display の番号が確認できない",
            normalized_pattern=check.normalized_pattern, ocr_lines=after_lines,
        )
    return step("marker_reclosure", "pass", normalized_pattern=check.normalized_pattern, matched_line=normalized_line)


# ---------------------------------------------------------------------------
# GUI-driving code. Not unit tested (requires a real macOS hosted runner);
# only calls into scripts/gui_validate.py and scripts/hosted_gui_interaction.py
# primitives loaded dynamically from the trusted control checkout.
# ---------------------------------------------------------------------------


def ocr_lines(interaction_module, helper, screenshot: Path, timeout: float) -> tuple:
    ok, text, error = interaction_module.run_helper(helper, ["ocr", str(screenshot)], timeout)
    if not ok:
        return False, [], error
    return True, [normalized_ocr_text(line) for line in text.splitlines() if line.strip()], ""


def click_content(interaction_module, helper, pid, screenshot: Path, pattern: str, edge: str, timeout: float) -> tuple:
    ok, out, err = interaction_module.run_helper(
        helper, ["click-text", str(pid), str(screenshot), pattern, edge], timeout
    )
    if not ok:
        return False, None, err
    try:
        evidence = parse_click_evidence(out)
    except ValueError as exc:
        return False, None, str(exc)
    return True, evidence, ""


def run_nested_position_checks(
    interaction_module, helper, pid, screenshot: Path, checks: tuple, timeout: float,
) -> list:
    """checks の baseline(child と同じ marker 種別のトップレベル行)/子の本文それぞれへ
    click-text し、同一 screenshot 内の bounding box x 位置を比較する。helper failure・
    OCR failure はここでも pass にせず blocked にする。
    """
    steps: list = []
    for check in checks:
        clicked, baseline_evidence, err = click_content(
            interaction_module, helper, pid, screenshot, check.baseline_content_pattern, "start", timeout,
        )
        if not clicked:
            steps.append(step(
                f"nested_position_{check.label}_baseline_click", "blocked", err,
                content_pattern=check.baseline_content_pattern,
            ))
            for child_pattern in check.child_content_patterns:
                steps.append(skipped_step(
                    f"nested_position_{check.label}_{child_pattern}", "baseline 行へのクリックに失敗した",
                ))
            continue
        for child_pattern in check.child_content_patterns:
            clicked_child, child_evidence, err = click_content(
                interaction_module, helper, pid, screenshot, child_pattern, "start", timeout,
            )
            if not clicked_child:
                steps.append(step(
                    f"nested_position_{check.label}_{child_pattern}", "blocked", err,
                    content_pattern=child_pattern,
                ))
                continue
            steps.append(evaluate_nested_position(check.label, child_pattern, baseline_evidence, child_evidence))
    return steps


def run_scenario(
    gui_validate_module, interaction_module, env, target_dir: Path, swift_helper, capture_helper,
    base_run_dir: Path,
    binary_path: Path, expected_sha: str, request_id: str, startup_timeout: float, window_timeout: float,
    helper_timeout: float, priority: dict, spec: ScenarioSpec,
) -> dict:
    run_dir = base_run_dir / spec.name
    fixture_path = run_dir / spec.fixture_filename
    run_dir.mkdir(parents=True, exist_ok=True)
    fixture_path.write_text(spec.fixture_text, encoding="utf-8")
    steps: list = []
    process_holder: dict = {"process": None}
    config = interaction_module.make_config(
        gui_validate_module, workspace_dir=target_dir, scenario=f"normal-list-{spec.name}",
        expected_sha=expected_sha, request_id=request_id, generation="1", run_dir=run_dir,
        fixture_path=fixture_path, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
        capture_helper=capture_helper,
    )
    try:
        session_steps, window_id = interaction_module.open_session(
            gui_validate_module, env, config, binary_path, process_holder, "before", swift_helper, helper_timeout
        )
        steps += session_steps
        pid = interaction_module.current_pid(process_holder)
        if pid is None or window_id is None:
            steps.append(skipped_step("scenario_body", "launch/window discovery が pass しなかった"))
        else:
            before_screenshot = run_dir / "before.png"
            ok, lines, err = ocr_lines(interaction_module, swift_helper, before_screenshot, helper_timeout)
            if not ok:
                steps.append(step("initial_display_ocr", "blocked", err))
            else:
                steps.append(step("initial_display_ocr", "pass", ocr_lines=lines))
                for check in spec.initial_checks:
                    steps.append(evaluate_initial_check(lines, check))
                if spec.initial_checks:
                    steps.append(evaluate_display_order(lines, [check.content_pattern for check in spec.initial_checks]))

            if spec.nested_position_checks:
                steps += run_nested_position_checks(
                    interaction_module, swift_helper, pid, before_screenshot,
                    spec.nested_position_checks, helper_timeout,
                )

            marker_check = spec.marker_check
            clicked, evidence, err = click_content(
                interaction_module, swift_helper, pid, before_screenshot,
                marker_check.content_pattern, "start", helper_timeout,
            )
            if not clicked:
                steps.append(step("marker_click", "blocked", err, content_pattern=marker_check.content_pattern))
                steps.append(skipped_step("marker_disclosure", "marker click に失敗した"))
                steps.append(skipped_step("marker_reclosure", "marker click に失敗した"))
            else:
                steps.append(step("marker_click", "pass", click_evidence=evidence))
                steps.append(interaction_module.capture_named(gui_validate_module, env, config, window_id, run_dir, "disclosed"))
                ok, disclosed_lines, err = ocr_lines(interaction_module, swift_helper, run_dir / "disclosed.png", helper_timeout)
                if not ok:
                    steps.append(step("marker_disclosure", "blocked", err))
                    steps.append(skipped_step("marker_reclosure", "disclosure 確認前の OCR が失敗した"))
                else:
                    steps.append(evaluate_marker_disclosure(disclosed_lines, marker_check))
                    ok, _out, err = interaction_module.run_helper(swift_helper, ["move-doc-start", str(pid)], helper_timeout)
                    if not ok:
                        steps.append(step("marker_reclosure_move", "blocked", err))
                        steps.append(skipped_step("marker_reclosure", "caret を戻せなかった"))
                    else:
                        steps.append(interaction_module.capture_named(gui_validate_module, env, config, window_id, run_dir, "reclosed"))
                        ok, reclosed_lines, err = ocr_lines(interaction_module, swift_helper, run_dir / "reclosed.png", helper_timeout)
                        if not ok:
                            steps.append(step("marker_reclosure", "blocked", err))
                        else:
                            steps.append(evaluate_marker_reclosure(reclosed_lines, marker_check))

            steps.append(interaction_module.capture_named(gui_validate_module, env, config, window_id, run_dir, "after"))
    finally:
        steps.append(interaction_module.close_session(gui_validate_module, env, process_holder))

    return {
        "name": spec.name, "steps": steps, "result": worst(steps, priority), "reason": reason_for(steps),
        "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)},
    }


def _boundary_edit_subtest(
    interaction_module, swift_helper, pid, fixture_path, baseline, screenshot_path,
    ocr_pattern: str, ocr_edge: str, source_pattern: str, source_edge: str, insertion: str,
    helper_timeout: float, poll_timeout: float, name: str,
) -> dict:
    """source marker/本文の直接 ASCII 編集を、hosted_gui_interaction.py の
    boundary_edit_check(click → 挿入 → 実バイト列比較 → undo → 実バイト列比較)へ委譲する。
    helper failure・OCR failure・破損はここでも pass にならない(boundary_edit_check 自身が
    fail-closed に分類する)。
    """
    status, reason, detail = interaction_module.boundary_edit_check(
        swift_helper, screenshot_path, pid, fixture_path, baseline,
        ocr_pattern, ocr_edge, source_pattern, source_edge, insertion,
        helper_timeout, poll_timeout,
    )
    return step(name, status, reason, **detail)


def _caret_move_edit_check(
    interaction_module, swift_helper, pid, fixture_path, baseline,
    direction: str, count: int, source_pattern: str, source_edge: str, insertion: str,
    helper_timeout: float, poll_timeout: float,
) -> tuple:
    """caret がすでに決定的な基準位置にある状態から、既知の文字数だけキーボード
    (move-caret)で移動して境界へ到達し、type-save で挿入した実バイト列と undo 復元を
    検証する。screenshot・click-text・Vision OCR を一切経由しないため、screenshot 自体が
    明瞭でも別の文字列へ誤認識する2回目の OCR 失敗の影響を受けない
    (Issue #126 post-merge GUI run 35410838091: marker_disclosed.png には "41. Beta row" と
    caret が明瞭に写っていたが、そこへの2回目の click-text の Vision OCR が
    "AppleM2ScalerParavirtDriver" へ誤認識して失敗した)。挿入位置は既知の移動量から
    決定的に定まるため、期待値との不一致は helper/OCR 誤差ではなく製品側の疑いとして
    fail とする(procedure blocked への退避はしない)。
    """
    detail = {"move_direction": direction, "move_count": count}
    expected_offset = match_insertion_offset(baseline, source_pattern, edge=source_edge)
    detail["expected_canonical_source_offset"] = expected_offset
    if count > 0:
        ok, _out, err = interaction_module.run_helper(
            swift_helper, ["move-caret", str(pid), direction, str(count)], helper_timeout
        )
        if not ok:
            return "blocked", f"境界へのキーボード移動に失敗した: {err}", detail
    ok, _out, err = interaction_module.run_helper(swift_helper, ["type-save", str(pid), insertion], helper_timeout)
    if not ok:
        return "blocked", (
            f"境界への入力に失敗した(timeout、起動失敗、integrity mismatch などの"
            f"helper/実行環境側要因の可能性があり、procedure blocked とする): {err}"
        ), detail
    expected = insert_at_match(baseline, source_pattern, insertion, edge=source_edge)
    matched, actual_bytes = interaction_module.wait_for_fixture_bytes(fixture_path, expected.encode("utf-8"), poll_timeout)
    actual = actual_bytes.decode("utf-8", errors="replace")
    detail.update(expected_after_insert=expected, actual_after_insert=actual)
    if not matched:
        return "fail", (
            "キーボード移動による境界への直接編集後の内容が期待値と一致しない"
            "(挿入位置は既知の移動量から決定的に定まるため、製品側の疑いとして fail とする)"
        ), detail
    ok, _out, err = interaction_module.run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
    if not ok:
        return "fail", f"undo に失敗した: {err}", detail
    matched, actual_bytes = interaction_module.wait_for_fixture_bytes(fixture_path, baseline.encode("utf-8"), poll_timeout)
    detail.update(expected_after_undo=baseline, actual_after_undo=actual_bytes.decode("utf-8", errors="replace"))
    if not matched:
        return "fail", "undo 後に元の内容へ戻らない", detail
    return "pass", None, detail


def _marker_direct_edit_subtest(
    gui_validate_module, interaction_module, env, config, window_id, swift_helper, pid,
    fixture_path, run_dir: Path, helper_timeout: float, poll_timeout: float,
) -> list:
    """document start から raw marker "41." 先頭の canonical source offset まで move-caret
    right だけで決定的に移動して ASCII 文字を挿入し、undo を実バイト列で検証する。click-text の
    bounding box start/end は canonical source caret 位置の証拠ではなく、本文クリック後の固定
    文字数移動は1文字の OCR 着地誤差でも正常製品を fail にしうるため(PR #208 review
    PRRT_kwDOUETGuM6j8L1G)、caret 位置決めに click-text/OCR を一切使わない。
    marker_disclosed.png は caret 到達時点の raw marker disclosure の evidence として撮る
    だけで、そこへの click-text/OCR には依存しない。パスの判定は保存された実バイトと undo
    復元だけで行う。
    """
    name = "direct_marker_edit_ascii_undo"
    steps: list = []
    ok, _out, err = interaction_module.run_helper(swift_helper, ["move-doc-start", str(pid)], helper_timeout)
    if not ok:
        steps.append(step(name, "blocked", f"caret を document start へ移動できなかった: {err}"))
        return steps
    if MARKER_DIRECT_EDIT_CARET_RIGHT_COUNT > 0:
        ok, _out, err = interaction_module.run_helper(
            swift_helper,
            ["move-caret", str(pid), "right", str(MARKER_DIRECT_EDIT_CARET_RIGHT_COUNT)],
            helper_timeout,
        )
        if not ok:
            steps.append(step(name, "blocked", f"raw marker 先頭へのキーボード移動に失敗した: {err}"))
            return steps
    disclosed_capture = interaction_module.capture_named(
        gui_validate_module, env, config, window_id, run_dir, "marker_disclosed",
    )
    steps.append(disclosed_capture)
    if disclosed_capture["result"] != "pass":
        steps.append(step(name, "blocked", disclosed_capture.get("reason")))
        return steps
    # caret はすでに move-caret で raw marker 先頭に決定的に到達している。
    status, reason, detail = _caret_move_edit_check(
        interaction_module, swift_helper, pid, fixture_path, EDITING_FIXTURE_ORIGINAL,
        "right", 0, MARKER_DIRECT_EDIT_RAW_PATTERN, "start", MARKER_DIRECT_EDIT_INSERTION,
        helper_timeout, poll_timeout,
    )
    steps.append(step(name, status, reason, **detail))
    return steps


def _ime_commit_subtest(
    gui_validate_module, interaction_module, env, config, window_id, swift_helper, pid,
    fixture_path, run_dir: Path, helper_timeout: float, poll_timeout: float,
) -> tuple:
    """caret 位置での日本語 IME 確定を実ファイル byte 列で検証する。

    入力ソース未提供・helper failure・OCR failure はいずれも pass にしない。呼び出し側が
    IME 工程の成功・失敗にかかわらず元の入力ソースへ復元できるよう、ここで確認できた
    original source id (未確認なら None)を合わせて返す。

    source 選択・settle・current-source 再確認・romaji 入力・変換/確定・保存は、すべて
    単一の helper invocation(type-romaji-at-caret-commit-save)の中で行う。事前に別の
    helper invocation で select-source を単独実行して「選択 pass」を先に報告すると、
    実際にはその後の変換が settle せず未変換のまま保存されても、選択自体は pass の
    ままになってしまう(Issue #126 post-merge GUI run 35410838091: helper が選択 pass と
    した直後でも保存値は "Alpha rownihongo \\n\\n" のように未変換だった)。そのため選択の
    成否は、実際に変換・確定・保存まで行った結果からのみ判定する。

    currentSourceID() が日本語ソースへの切り替わりを報告しても、Hane 側の IME session が
    実際に変換可能とは限らない(PR #208 review PRRT_kwDOUETGuM6j8CB_)。readiness の証拠は
    「保存された実バイトが期待する日本語へ変換・確定された」ことに限定し、それが揃わない
    attempt では次へ進む前に必ず pristine baseline(実ファイル・アプリ内状態の両方)まで
    restore_scenario_baseline で巻き戻し、attempt 固有の screenshot を撮影してから、
    document start(restore_scenario_baseline が呼ぶ move-doc-start で caret はすでに
    そこにある)から "Alpha row" 末尾の canonical source offset まで move-caret right だけで
    決定的に caret を再配置してやり直す。click-text/OCR で caret 位置を決めない(本文クリック
    からの位置決めは click-text の bounding box start/end を canonical source caret 位置の
    証拠として扱ってしまい、1文字の OCR 着地誤差でも正常製品を fail にしうる。PR #208 review
    PRRT_kwDOUETGuM6j8L1G)。前回の未変換テキストが次の attempt へ持ち越されることはない。
    """
    steps: list = []
    ok, out, err = interaction_module.run_helper(swift_helper, ["current-source"], helper_timeout)
    if not ok:
        steps.append(step("ime_commit_query_current_source", "blocked", err))
        return steps, None
    original_source = out
    steps.append(step("ime_commit_query_current_source", "pass", source_id=original_source))
    ok, out, err = interaction_module.run_helper(swift_helper, ["list-sources"], helper_timeout)
    if not ok:
        steps.append(step("ime_commit_list_input_sources", "blocked", err))
        return steps, original_source
    available = [line for line in out.splitlines() if line.strip()]
    japanese_source = next((source_id for source_id in available if source_id == interaction_module.JAPANESE_SOURCE), None)
    if japanese_source is None:
        steps.append(step(
            "ime_commit_identify_japanese_source", "blocked",
            "このランナーに組み込みの日本語入力ソースが見つからない",
        ))
        return steps, original_source
    steps.append(step("ime_commit_identify_japanese_source", "pass", identified_source=japanese_source))

    matched = False
    actual = b""
    attempts_evidence: list = []
    for attempt in range(1, IME_COMMIT_MAX_ATTEMPTS + 1):
        label = f"ime_commit_attempt_{attempt}"
        restore = interaction_module.restore_scenario_baseline(
            swift_helper, pid, fixture_path, EDITING_FIXTURE_ORIGINAL,
            helper_timeout, poll_timeout, f"{label}_baseline_restore",
        )
        steps.append(restore)
        if restore["result"] != "pass":
            steps.append(step(
                "direct_ime_commit_at_caret", "blocked",
                f"attempt {attempt} の pristine baseline 復元に失敗した: {restore.get('reason')}",
                attempts=attempts_evidence,
            ))
            return steps, original_source

        capture = interaction_module.capture_named(gui_validate_module, env, config, window_id, run_dir, label)
        steps.append(capture)
        if capture["result"] != "pass":
            steps.append(step(
                "direct_ime_commit_at_caret", "blocked", capture.get("reason"), attempts=attempts_evidence,
            ))
            return steps, original_source
        screenshot = run_dir / f"{label}.png"

        # restore_scenario_baseline はすでに move-doc-start で caret を document start へ
        # 戻している。ここから "Alpha row" 末尾の canonical source offset まで、click-text/OCR
        # には一切依存せず move-caret right だけで決定的に移動する(docstring 参照)。
        if IME_COMMIT_CARET_RIGHT_COUNT > 0:
            ok, _out, err = interaction_module.run_helper(
                swift_helper, ["move-caret", str(pid), "right", str(IME_COMMIT_CARET_RIGHT_COUNT)], helper_timeout
            )
            if not ok:
                steps.append(step(
                    "direct_ime_commit_at_caret", "blocked",
                    f"attempt {attempt} の caret 配置移動に失敗した: {err}", attempts=attempts_evidence,
                ))
                return steps, original_source

        # source 選択・settle・current-source 再確認・romaji 入力・変換/確定・保存を単一の
        # helper invocation の中で行う。ここで select-source を別途呼ばない(上の docstring 参照)。
        ok, _out, err = interaction_module.run_helper(
            swift_helper, ["type-romaji-at-caret-commit-save", str(pid), IME_COMMIT_ROMAJI, japanese_source], helper_timeout
        )
        if not ok:
            attempts_evidence.append({
                "attempt": attempt, "screenshot": str(screenshot),
                "helper_result": "blocked", "helper_error": err,
            })
            steps.append(step(
                "direct_ime_commit_at_caret", "blocked",
                f"attempt {attempt} の source 選択・settle・変換・確定・保存を行う単一 helper invocation が失敗した: {err}",
                attempts=attempts_evidence,
            ))
            return steps, original_source

        matched, actual = interaction_module.wait_for_fixture_bytes(fixture_path, EDITING_AFTER_IME_COMMIT.encode("utf-8"), poll_timeout)
        attempts_evidence.append({
            "attempt": attempt, "screenshot": str(screenshot),
            "helper_result": "pass", "matched": matched, "actual_bytes": actual.decode("utf-8", errors="replace"),
        })
        if matched:
            break

    steps.append(step(
        "direct_ime_commit_at_caret", "pass" if matched else "fail",
        reason=None if matched else (
            f"{IME_COMMIT_MAX_ATTEMPTS} 回試行しても caret 位置での日本語 IME 確定後の内容が"
            "期待した文字列と一致しない"
        ),
        expected=EDITING_AFTER_IME_COMMIT, actual=actual.decode("utf-8", errors="replace"),
        attempts=attempts_evidence,
    ))
    return steps, original_source


def _restore_ime_side_effects(
    interaction_module, swift_helper, pid, fixture_path, original_source: Optional[str],
    helper_timeout: float, poll_timeout: float,
) -> list:
    """IME 工程の成功・失敗にかかわらず、後続の ASCII 選択置換の前に元の入力ソースと
    document baseline を復元する。"""
    steps: list = []
    if original_source:
        ok, _out, err = interaction_module.run_helper(swift_helper, ["select-source", original_source], helper_timeout)
        steps.append(step(
            "restore_input_source_before_selection_replace", "pass" if ok else "blocked",
            reason=None if ok else err, restored_source=original_source,
        ))
    else:
        steps.append(step(
            "restore_input_source_before_selection_replace", "blocked",
            "元の入力ソースを取得できなかったため復元できない",
        ))
    restore_step = interaction_module.restore_scenario_baseline(
        swift_helper, pid, fixture_path, EDITING_FIXTURE_ORIGINAL,
        helper_timeout, poll_timeout, "restore_document_baseline_after_ime_commit",
    )
    steps.append(restore_step)
    return steps


def _selection_replace_subtest(
    gui_validate_module, interaction_module, env, config, window_id, swift_helper, pid,
    fixture_path, run_dir: Path, helper_timeout: float, poll_timeout: float,
) -> list:
    """document start から "Gamma" 先頭の canonical source offset まで move-caret right だけで
    決定的に移動し、そこから Shift+→ で正確に SELECTION_REPLACE_WORD_LENGTH 文字だけキーボード
    選択して "Delta" へ選択置換し、undo/redo を実バイト列で検証する。OCR bounding box の
    start→end drag は文字境界の真値ではなく、末尾の文字を取りこぼすことがある(Issue #126
    post-merge GUI run 35410838091: "Gamma" の選択が "Gamm" までしか及ばず "Deltaa row" に
    なった)し、click-text の bounding box start/end 自体も canonical source caret 位置の
    証拠ではない(PR #208 review PRRT_kwDOUETGuM6j8L1G)ため、選択開始位置も選択範囲そのものも
    click-text/OCR に一切依存しないキーボード操作だけで決定的に確定する。
    """
    name = "selection_replace_ascii_undo_redo"
    steps: list = []
    ok, _out, err = interaction_module.run_helper(swift_helper, ["move-doc-start", str(pid)], helper_timeout)
    if not ok:
        steps.append(step(name, "blocked", f"caret を document start へ移動できなかった: {err}"))
        return steps
    if SELECTION_REPLACE_CARET_RIGHT_COUNT > 0:
        ok, _out, err = interaction_module.run_helper(
            swift_helper, ["move-caret", str(pid), "right", str(SELECTION_REPLACE_CARET_RIGHT_COUNT)], helper_timeout,
        )
        if not ok:
            steps.append(step(name, "blocked", f"'{SELECTION_REPLACE_WORD}' 先頭へのキーボード移動に失敗した: {err}"))
            return steps
    capture = interaction_module.capture_named(gui_validate_module, env, config, window_id, run_dir, "selection_replace_before")
    steps.append(capture)
    if capture["result"] != "pass":
        steps.append(step(name, "blocked", capture.get("reason")))
        return steps
    screenshot = run_dir / "selection_replace_before.png"
    ok, _out, err = interaction_module.run_helper(
        swift_helper, ["shift-select", str(pid), "right", str(SELECTION_REPLACE_WORD_LENGTH)], helper_timeout,
    )
    if not ok:
        steps.append(step(name, "blocked", f"Shift+矢印での選択に失敗した: {err}"))
        return steps
    ok, _out, err = interaction_module.run_helper(swift_helper, ["type-save", str(pid), SELECTION_REPLACE_REPLACEMENT], helper_timeout)
    if not ok:
        steps.append(step(name, "blocked", f"選択置換の入力に失敗した: {err}"))
        return steps
    matched, actual = interaction_module.wait_for_fixture_bytes(fixture_path, EDITING_AFTER_SELECTION_REPLACE.encode("utf-8"), poll_timeout)
    detail = {"screenshot": str(screenshot), "expected_after_replace": EDITING_AFTER_SELECTION_REPLACE,
              "actual_after_replace": actual.decode("utf-8", errors="replace")}
    if not matched:
        steps.append(step(name, "fail", "選択置換後の内容が期待値と一致しない", **detail))
        return steps
    ok, _out, err = interaction_module.run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(step(name, "blocked", f"undo に失敗した: {err}"))
        return steps
    matched, actual = interaction_module.wait_for_fixture_bytes(fixture_path, EDITING_FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout)
    detail["undo_actual"] = actual.decode("utf-8", errors="replace")
    if not matched:
        steps.append(step(name, "fail", "undo 後に元の内容へ戻らない", **detail))
        return steps
    ok, _out, err = interaction_module.run_helper(swift_helper, ["redo-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(step(name, "blocked", f"redo に失敗した: {err}"))
        return steps
    matched, actual = interaction_module.wait_for_fixture_bytes(fixture_path, EDITING_AFTER_SELECTION_REPLACE.encode("utf-8"), poll_timeout)
    detail["redo_actual"] = actual.decode("utf-8", errors="replace")
    if not matched:
        steps.append(step(name, "fail", "redo 後に置換結果へ戻らない", **detail))
        return steps
    ok, _out, err = interaction_module.run_helper(swift_helper, ["undo-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(step(name, "blocked", f"最終 undo に失敗した: {err}"))
        return steps
    matched, actual = interaction_module.wait_for_fixture_bytes(fixture_path, EDITING_FIXTURE_ORIGINAL.encode("utf-8"), poll_timeout)
    detail["restored_actual"] = actual.decode("utf-8", errors="replace")
    steps.append(step(name, "pass" if matched else "fail",
                      reason=None if matched else "最終 undo 後に元の内容へ戻らない", **detail))
    if not matched:
        return steps
    # 再起動・再オープンの検証(#126 E)は、実ファイルが変更済み状態のまま再起動された
    # ケースを確認する必要がある。上の undo/redo の往復確認自体は変更しないまま、
    # ここで改めて redo して、再起動直前の実ファイルを選択置換後の編集済み状態に戻す。
    ok, _out, err = interaction_module.run_helper(swift_helper, ["redo-save", str(pid)], helper_timeout)
    if not ok:
        steps.append(step(
            "redo_to_edited_state_before_reopen", "blocked",
            f"再起動前に編集済み状態へ戻す redo に失敗した: {err}",
        ))
        return steps
    matched_edited, actual_edited = interaction_module.wait_for_fixture_bytes(
        fixture_path, EDITING_AFTER_SELECTION_REPLACE.encode("utf-8"), poll_timeout
    )
    steps.append(step(
        "redo_to_edited_state_before_reopen", "pass" if matched_edited else "fail",
        reason=None if matched_edited else "再起動前に編集済み状態へ戻す redo 後、内容が編集済み状態と一致しない",
        expected=EDITING_AFTER_SELECTION_REPLACE, actual=actual_edited.decode("utf-8", errors="replace"),
    ))
    return steps


def run_editing_scenario(
    gui_validate_module, interaction_module, env, target_dir: Path, swift_helper, capture_helper,
    base_run_dir: Path,
    binary_path: Path, expected_sha: str, request_id: str, startup_timeout: float, window_timeout: float,
    helper_timeout: float, poll_timeout: float, priority: dict,
) -> dict:
    """#126 E (hosted-normal-list/3): 通常リスト固有の source marker/本文の直接編集、
    ASCII 入力、日本語 IME 確定、選択置換、Undo/Redo、保存、再起動後の再オープンを
    実ファイルの byte 列で検証する。B/C/D の OCR 表示検証(run_scenario)とは別の focused
    scenario として追加し、既存の B/C/D の判定には影響しない。
    """
    name = "list_source_marker_body_editing"
    run_dir = base_run_dir / name
    fixture_path = run_dir / EDITING_FIXTURE_FILENAME
    run_dir.mkdir(parents=True, exist_ok=True)
    fixture_path.write_text(EDITING_FIXTURE_ORIGINAL, encoding="utf-8")
    steps: list = []
    process_holder: dict = {"process": None}
    config = interaction_module.make_config(
        gui_validate_module, workspace_dir=target_dir, scenario="normal-list-editing",
        expected_sha=expected_sha, request_id=request_id, generation="1", run_dir=run_dir,
        fixture_path=fixture_path, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
        capture_helper=capture_helper,
    )
    try:
        session_steps, window_id = interaction_module.open_session(
            gui_validate_module, env, config, binary_path, process_holder, "before", swift_helper, helper_timeout
        )
        steps += session_steps
        pid = interaction_module.current_pid(process_holder)
        if pid is None or window_id is None:
            for subname in (
                "direct_body_edit_ascii_undo", "direct_marker_edit_ascii_undo",
                "direct_ime_commit_at_caret", "selection_replace_ascii_undo_redo",
            ):
                steps.append(skipped_step(subname, "launch/window discovery が pass しなかった"))
        else:
            before_screenshot = run_dir / "before.png"

            steps.append(_boundary_edit_subtest(
                interaction_module, swift_helper, pid, fixture_path, EDITING_FIXTURE_ORIGINAL,
                before_screenshot, BODY_DIRECT_EDIT_OCR_PATTERN, "end", BODY_DIRECT_EDIT_OCR_PATTERN, "end",
                BODY_DIRECT_EDIT_INSERTION, helper_timeout, poll_timeout, "direct_body_edit_ascii_undo",
            ))

            steps += _marker_direct_edit_subtest(
                gui_validate_module, interaction_module, env, config, window_id, swift_helper, pid,
                fixture_path, run_dir, helper_timeout, poll_timeout,
            )

            ime_steps, original_source = _ime_commit_subtest(
                gui_validate_module, interaction_module, env, config, window_id, swift_helper, pid,
                fixture_path, run_dir, helper_timeout, poll_timeout,
            )
            steps += ime_steps
            steps += _restore_ime_side_effects(
                interaction_module, swift_helper, pid, fixture_path, original_source, helper_timeout, poll_timeout,
            )

            steps += _selection_replace_subtest(
                gui_validate_module, interaction_module, env, config, window_id, swift_helper, pid,
                fixture_path, run_dir, helper_timeout, poll_timeout,
            )

            steps.append(interaction_module.capture_named(gui_validate_module, env, config, window_id, run_dir, "after"))
    finally:
        steps.append(interaction_module.close_session(gui_validate_module, env, process_holder))

    if worst(steps, priority) != "pass":
        steps.append(skipped_step("reopen_after_restart", "再オープン前の工程が pass しなかった"))
        return {"name": name, "steps": steps, "result": worst(steps, priority), "reason": reason_for(steps),
                "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}

    reopen_dir = run_dir / "reopen"
    reopen_process_holder: dict = {"process": None}
    reopen_config = interaction_module.make_config(
        gui_validate_module, workspace_dir=target_dir, scenario="normal-list-editing-reopen",
        expected_sha=expected_sha, request_id=request_id, generation="2", run_dir=reopen_dir,
        fixture_path=fixture_path, features=["timing-probe"], extra_env={},
        startup_timeout=startup_timeout, window_timeout=window_timeout,
        capture_helper=capture_helper,
    )
    try:
        session_steps, _window_id = interaction_module.open_session(
            gui_validate_module, env, reopen_config, binary_path, reopen_process_holder, "reopen", swift_helper, helper_timeout
        )
        steps += session_steps
        steps.append(interaction_module.verify_visible_text(
            swift_helper, reopen_dir / "reopen.png", REOPEN_VISIBLE_TEXT, helper_timeout
        ))
        matched, actual = interaction_module.wait_for_fixture_bytes(
            fixture_path, REOPEN_EXPECTED_BYTES.encode("utf-8"), 1.0
        )
        steps.append(step(
            "reopen_content_check", "pass" if matched else "fail",
            reason=None if matched else "再オープン後もフィクスチャが再起動直前の編集済み内容と一致することを確認できない",
            note="ファイル内容の一致は再オープンの証跡の一部に過ぎず、描画結果そのものの証明ではない",
            expected=REOPEN_EXPECTED_BYTES,
            actual=actual.decode("utf-8", errors="replace"),
        ))
    finally:
        steps.append(interaction_module.close_session(gui_validate_module, env, reopen_process_holder))
    return {"name": name, "steps": steps, "result": worst(steps, priority), "reason": reason_for(steps),
            "evidence": {"fixture_path": str(fixture_path), "run_dir": str(run_dir)}}


def env_float(name: str, default: float) -> float:
    value = os.environ.get(name)
    if not value:
        return default
    parsed = float(value)
    if parsed != parsed or parsed <= 0 or parsed == float("inf"):
        raise ValueError(f"{name} must be positive and finite")
    return parsed


def main() -> int:
    target_value = os.environ.get("HANE_NORMAL_LIST_GUI_TARGET_DIR")
    control_value = os.environ.get("HANE_NORMAL_LIST_GUI_CONTROL_DIR")
    expected_sha = os.environ.get("HANE_NORMAL_LIST_GUI_EXPECTED_SHA", "")
    if not target_value or not control_value or not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        print("target dir, control dir and full expected SHA are required", file=sys.stderr)
        return 3

    target_dir = Path(target_value).resolve()
    control_dir = Path(control_value).resolve()
    run_dir = Path(
        os.environ.get("HANE_NORMAL_LIST_GUI_RUN_DIR")
        or (Path(tempfile.gettempdir()) / "hane-normal-list-gui")
    ).resolve()
    request_id = os.environ.get("HANE_NORMAL_LIST_GUI_REQUEST_ID") or f"normal-list-{os.getpid()}"
    startup_timeout = env_float("HANE_NORMAL_LIST_GUI_STARTUP_TIMEOUT_SECS", 15.0)
    window_timeout = env_float("HANE_NORMAL_LIST_GUI_WINDOW_TIMEOUT_SECS", 30.0)
    helper_timeout = env_float("HANE_NORMAL_LIST_GUI_HELPER_TIMEOUT_SECS", 20.0)
    poll_timeout = env_float("HANE_NORMAL_LIST_GUI_POLL_TIMEOUT_SECS", 20.0)
    run_dir.mkdir(parents=True, exist_ok=True)
    result_path = run_dir / "result.json"

    gui_validate_module = load_module(control_dir, "scripts/gui_validate.py", "normal_list_gui_validate")
    interaction_module = load_module(control_dir, "scripts/hosted_gui_interaction.py", "normal_list_hosted_gui_interaction")
    env = gui_validate_module.RealEnvironment()
    priority = gui_validate_module.RESULT_PRIORITY

    top_steps: list = []
    scenarios: list = []
    target_info: dict = {}
    build_info: dict = {}
    started_at = env.clock.now_iso()
    helper_tmp = tempfile.TemporaryDirectory(prefix="hane-normal-list-helper-")
    original_input_source: Optional[str] = None

    try:
        env.acquire_execution()
        preflight_config = interaction_module.make_config(
            gui_validate_module, workspace_dir=target_dir, scenario="normal-list-preflight",
            expected_sha=expected_sha, request_id=request_id, generation="0", run_dir=run_dir / "_preflight",
            fixture_path=None, features=["timing-probe"], extra_env={},
            startup_timeout=startup_timeout, window_timeout=window_timeout,
        )
        preflight, target_info = gui_validate_module.do_preflight(env, preflight_config)
        top_steps.append(preflight)
        binary_path = None
        swift_helper = None
        if preflight["result"] != "pass":
            top_steps.append(skipped_step("build", "preflight が pass しなかった"))
        else:
            try:
                swift_helper = interaction_module.prepare_helper(
                    control_dir / "scripts" / "hosted_gui_interaction.swift", Path(helper_tmp.name)
                )
                top_steps.append(step("prepare_helper", "pass", sha256=swift_helper.digest))
            except (OSError, subprocess.SubprocessError) as exc:
                swift_helper = None
                top_steps.append(step("prepare_helper", "blocked", str(exc)))

            if swift_helper is None:
                top_steps.append(skipped_step("build", "trusted helper が利用できない"))
            else:
                build_step, binary_path, build_info = gui_validate_module.do_build(env, preflight_config, target_info["actual_sha"])
                top_steps.append(build_step)
                if build_step["result"] != "pass":
                    binary_path = None

            input_source_ready = False
            source_attempts = 0
            if swift_helper is not None:
                ok, source_id, error = interaction_module.run_helper(
                    swift_helper, ["current-source"], helper_timeout
                )
                if ok:
                    original_input_source = source_id
                    ok, selected_source, error, source_attempts = interaction_module.select_input_source(
                        swift_helper, ASCII_INPUT_SOURCE, helper_timeout
                    )
                    input_source_ready = ok and selected_source == ASCII_INPUT_SOURCE
                top_steps.append(
                    step(
                        "select_ascii_input_source",
                        "pass" if input_source_ready else "blocked",
                        None if input_source_ready else (error or "ABC入力ソースを選択できない"),
                        source_id=ASCII_INPUT_SOURCE,
                        attempts=source_attempts,
                    )
                )

            if binary_path is not None and swift_helper is not None and input_source_ready:
                for spec in ALL_SCENARIO_SPECS:
                    try:
                        scenarios.append(run_scenario(
                            gui_validate_module, interaction_module, env, target_dir, swift_helper, None,
                            run_dir, binary_path, expected_sha, request_id,
                            startup_timeout, window_timeout, helper_timeout,
                            priority, spec,
                        ))
                    except (gui_validate_module.Aborted, Exception) as exc:
                        scenarios.append({"name": spec.name, "result": "blocked", "reason": str(exc), "steps": []})
                        if isinstance(exc, gui_validate_module.Aborted):
                            break

                try:
                    scenarios.append(run_editing_scenario(
                        gui_validate_module, interaction_module, env, target_dir, swift_helper, None,
                        run_dir, binary_path, expected_sha, request_id,
                        startup_timeout, window_timeout, helper_timeout, poll_timeout, priority,
                    ))
                except (gui_validate_module.Aborted, Exception) as exc:
                    scenarios.append({
                        "name": "list_source_marker_body_editing", "result": "blocked", "reason": str(exc), "steps": [],
                    })
            elif binary_path is not None and swift_helper is not None:
                scenarios.append({
                    "name": "normal_list_gui_setup", "result": "blocked",
                    "reason": "ASCII入力ソースの初期化に失敗した", "steps": [],
                })

            if swift_helper is not None and original_input_source:
                ok, _output, error = interaction_module.run_helper(
                    swift_helper, ["select-source", original_input_source], helper_timeout
                )
                top_steps.append(
                    step(
                        "restore_input_source",
                        "pass" if ok else "blocked",
                        None if ok else error,
                        source_id=original_input_source,
                    )
                )

        all_results = [s["result"] for s in top_steps if s["result"] in priority] + [
            s["result"] for s in scenarios if s["result"] in priority
        ]
        overall = min(all_results, key=lambda value: priority[value]) if all_results else "blocked"
        reasons = [s.get("reason") for s in top_steps if s.get("reason") and s["result"] != "pass"]
        reasons += [s.get("reason") for s in scenarios if s.get("reason") and s["result"] != "pass"]
        reason = "; ".join(r for r in reasons if r) or "Issue #126 focused normal-list GUI checks passed"
        control_sha = env.git_head(control_dir)
        result = {
            "schema_version": SCHEMA_VERSION,
            "procedure_version": PROCEDURE_VERSION,
            "verification_kind": VERIFICATION_KIND,
            "scope_note": SCOPE_NOTE,
            "request_id": request_id,
            "started_at": started_at,
            "finished_at": env.clock.now_iso(),
            "target": target_info,
            "control": {"sha": control_sha},
            "runner": {
                "os": os.environ.get("RUNNER_OS", ""),
                "arch": os.environ.get("RUNNER_ARCH", ""),
                "macos_version": platform.mac_ver()[0],
                "machine": platform.machine(),
            },
            "build": build_info,
            "top_level_steps": top_steps,
            "scenarios": scenarios,
            "overall_result": overall,
            "overall_reason": reason,
        }
        label = {"pass": "PASS", "fail": "FAIL", "blocked": "BLOCKED"}.get(overall, "BLOCKED")
        result["summary"] = f"[{label}] {PROCEDURE_VERSION} — {reason}"
        result_path.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        (run_dir / "summary.md").write_text(result["summary"] + "\n", encoding="utf-8")
        print(result["summary"])
        print(f"result: {result_path}")
        return EXIT_PASS if overall == "pass" else EXIT_NONPASS
    except (gui_validate_module.Aborted, gui_validate_module.EnvError, OSError, subprocess.SubprocessError, ValueError, RuntimeError) as exc:
        fallback = {
            "schema_version": SCHEMA_VERSION,
            "procedure_version": PROCEDURE_VERSION,
            "verification_kind": VERIFICATION_KIND,
            "scope_note": SCOPE_NOTE,
            "request_id": request_id,
            "overall_result": "blocked",
            "overall_reason": str(exc),
            "top_level_steps": top_steps,
            "scenarios": scenarios,
        }
        result_path.write_text(json.dumps(fallback, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"[BLOCKED] {PROCEDURE_VERSION} — {exc}")
        return EXIT_NONPASS
    finally:
        env.release_execution()
        helper_tmp.cleanup()


if __name__ == "__main__":
    sys.exit(main())

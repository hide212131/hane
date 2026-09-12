"""Reconcile durable AADW commit statuses into Issue/PR lifecycle comments.

Producer workflows keep commit statuses as the machine-readable source of truth.
This controller mirrors those histories into Conversation comments so every
user-visible process exposes start/normal-end/abnormal-end without duplicating
comment logic in each producer workflow.
"""
import argparse
import datetime as dt
import os
import re
import sys

import aadw_notify

CONTEXTS = {
    "hane/codex-review": "codex-review",
    "hane/copilot-routing": "copilot-pre-gui-routing",
    "hane/claude-fix": "claude-fix",
    "hane/gui-requirement": "gui-requirement",
    "hane/gui-validation": "gui-validation",
    "hane/final-judge": "final-judge",
}
_ACTION_RUN = re.compile(r"/actions/runs/([1-9][0-9]*)(?:/attempts/([1-9][0-9]*))?(?:[/?#]|$)")
_GUI_GENERATION = re.compile(r" g([1-9][0-9]*)-([1-9][0-9]*)$")
_ROUTING_OK = re.compile(
    r"Copilot routing: (?:fix|continue-validation|blocked|workflow changes require owner) for [0-9a-f]{12}$"
)
_GUI_NORMAL = re.compile(r"GUI (?:pass|fail|blocked) v[0-9]+ [0-9a-f]{12} g[1-9][0-9]*-[1-9][0-9]*$")
_FINAL_NORMAL = re.compile(r"Final (?:ready|merged|fix|blocked) v[0-9]+ [0-9a-f]{12} e[0-9a-f]+$")
_FALLBACK_MARKER = re.compile(
    r"process=codex-review-fallback kind=pr number=([1-9][0-9]*) sha=([0-9a-f]{40}) "
    r"run=([1-9][0-9]*) attempt=([1-9][0-9]*) -->"
)


def pages(call, path):
    rows = []
    for page in range(1, 101):
        current = call(path + ("&" if "?" in path else "?") + f"per_page=100&page={page}")
        if not isinstance(current, list):
            raise RuntimeError("AADW通知reconcileでGitHub APIの一覧を取得できませんでした。")
        rows.extend(current)
        if len(current) < 100:
            return rows
    raise RuntimeError("AADW通知reconcileのページ数が上限を超えました。")


def parse_time(value):
    if not isinstance(value, str) or not value:
        return None
    try:
        return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def status_identity(status):
    """Return explicit (run, attempt) evidence without inventing an attempt."""
    description = status.get("description") or ""
    gui = _GUI_GENERATION.search(description)
    if gui:
        return gui.group(1), gui.group(2)
    match = _ACTION_RUN.search(status.get("target_url") or "")
    if match:
        return match.group(1), match.group(2)
    return None, None


def build_generations(statuses):
    """Group one context's append-only status history into process runs.

    A status URL often identifies the Actions run but omits its rerun attempt.
    Do not derive that attempt from the number of observed status generations:
    an earlier attempt may have failed before writing any status.  Such
    generations deliberately keep ``attempt=None`` until notification time,
    when the Actions attempt history is correlated by timestamp.

    A terminal status closes its generation.  Later status rows without an
    Actions URL must never be attached to an already-finished execution.
    """
    generations = []
    current = None
    for row in sorted(statuses, key=lambda item: item.get("id", 0)):
        state = row.get("state", "")
        explicit_run, explicit_attempt = status_identity(row)

        if state == "pending":
            if explicit_run is None:
                run_id, attempt = str(row.get("id")), "1"
            else:
                run_id, attempt = explicit_run, explicit_attempt
            current = {
                "generation_id": row.get("id"),
                "start": row,
                "latest": row,
                "run_id": run_id,
                "attempt": attempt,
            }
            generations.append(current)
            continue

        if current is not None:
            same_run = explicit_run is None or explicit_run == current["run_id"]
            same_attempt = (
                explicit_attempt is None
                or current["attempt"] is None
                or explicit_attempt == current["attempt"]
            )
            if same_run and same_attempt:
                current["latest"] = row
                if current["attempt"] is None and explicit_attempt is not None:
                    current["attempt"] = explicit_attempt
                current = None
                continue

        if explicit_run is None:
            run_id, attempt = str(row.get("id")), "1"
        else:
            run_id, attempt = explicit_run, explicit_attempt
        terminal = {
            "generation_id": row.get("id"),
            "start": None,
            "latest": row,
            "run_id": run_id,
            "attempt": attempt,
        }
        generations.append(terminal)
        current = None
    return generations


def codex_lifecycle_rows(statuses):
    """Exclude fallback compatibility results from the normal Codex lifecycle.

    The fallback workflow writes a successful ``hane/review-source`` status and
    then a compatible ``hane/codex-review`` clean/findings status with the same
    Copilot review URL.  That shared target URL is durable provenance for the
    fallback result.  Use it instead of temporal adjacency so a late real Codex
    result cannot be discarded merely because it arrived between those writes.
    """
    fallback_targets = {}
    for row in statuses:
        description = row.get("description") or ""
        target_url = row.get("target_url") or ""
        if (
            row.get("context") == "hane/review-source"
            and row.get("state") == "success"
            and description.startswith("Review source: Copilot fallback for ")
            and target_url
            and isinstance(row.get("id"), int)
        ):
            fallback_targets[target_url] = max(fallback_targets.get(target_url, -1), row["id"])

    rows = []
    for row in sorted(statuses, key=lambda item: item.get("id", 0)):
        if row.get("context") != "hane/codex-review":
            continue
        description = row.get("description") or ""
        compatible = (
            row.get("state") in ("success", "failure")
            and (
                description.startswith("Codex review clean for ")
                or description.startswith("Codex findings for ")
            )
        )
        marker_id = fallback_targets.get(row.get("target_url") or "")
        if compatible and marker_id is not None and row.get("id", -1) > marker_id:
            continue
        rows.append(row)
    return rows


def _attempt_starts(call, repository, run_id, cache):
    if run_id in cache:
        return cache[run_id]
    run = call(f"repos/{repository}/actions/runs/{run_id}")
    max_attempt = run.get("run_attempt") if isinstance(run, dict) else None
    if not isinstance(max_attempt, int) or max_attempt < 1:
        raise RuntimeError(f"Actions run {run_id} のattempt数を取得できませんでした。")
    starts = []
    for attempt in range(1, max_attempt + 1):
        row = call(f"repos/{repository}/actions/runs/{run_id}/attempts/{attempt}")
        started = parse_time(row.get("run_started_at") if isinstance(row, dict) else None)
        actual = row.get("run_attempt") if isinstance(row, dict) else None
        if actual != attempt or started is None:
            raise RuntimeError(f"Actions run {run_id} attempt {attempt} の開始時刻を取得できませんでした。")
        starts.append((attempt, started))
    cache[run_id] = starts
    return starts


def resolve_attempt(call, repository, generation, cache=None):
    explicit = generation.get("attempt")
    if explicit is not None:
        return str(explicit)
    run_id = str(generation.get("run_id") or "")
    if not re.fullmatch(r"[1-9][0-9]*", run_id):
        raise RuntimeError("AADW status generationのrun idが不正です。")
    evidence = generation.get("start") or generation.get("latest") or {}
    observed = parse_time(evidence.get("created_at"))
    if observed is None:
        raise RuntimeError(f"Actions run {run_id} のattemptを解決するstatus時刻がありません。")
    starts = _attempt_starts(call, repository, run_id, cache if cache is not None else {})
    candidates = [(attempt, started) for attempt, started in starts if started <= observed]
    if not candidates:
        raise RuntimeError(f"Actions run {run_id} のstatus時刻に対応するattemptがありません。")
    return str(max(candidates, key=lambda item: item[1])[0])


def outcome(context, status):
    state = status.get("state", "")
    description = status.get("description") or ""
    if state == "pending":
        return "start"
    if context in ("hane/codex-review", "hane/gui-requirement"):
        return "success" if state in ("success", "failure") else "failure"
    if context == "hane/copilot-routing":
        return "success" if _ROUTING_OK.fullmatch(description) else "failure"
    if context == "hane/claude-fix":
        return "success" if state == "success" and description.startswith("Claude fix completed for ") else "failure"
    if context == "hane/gui-validation":
        return "success" if _GUI_NORMAL.fullmatch(description) else "failure"
    if context == "hane/final-judge":
        return "success" if _FINAL_NORMAL.fullmatch(description) else "failure"
    return "failure"


def recent(generation, cutoff):
    latest_at = parse_time(generation["latest"].get("created_at"))
    start_at = parse_time((generation.get("start") or {}).get("created_at"))
    return bool((latest_at and latest_at >= cutoff) or (start_at and start_at >= cutoff))


def notify_generation(call, repository, pr_number, sha, context, generation, source=None, attempt_cache=None):
    process = CONTEXTS[context]
    state = outcome(context, generation["latest"])
    description = generation["latest"].get("description") or "状態の説明はありません。"
    detail = f"状態: {description}"
    if context == "hane/codex-review" and source:
        detail += f"\nレビュー実施者: {source}"
    attempt = resolve_attempt(call, repository, generation, attempt_cache)
    return aadw_notify.notify(
        call,
        state=state,
        process=process,
        kind="pr",
        number=pr_number,
        sha=sha,
        repository=repository,
        run_id=generation["run_id"],
        attempt=attempt,
        detail=detail,
    )["action"]


def review_source(statuses):
    rows = [
        row for row in statuses
        if row.get("context") == "hane/review-source"
        and row.get("state") == "success"
        and (row.get("description") or "").startswith("Review source: Copilot fallback for ")
    ]
    return "GitHub Copilot fallback" if rows else None


def active_fallback_run(call, repository, pr_number, sha):
    found = []
    for comment in pages(call, f"repos/{repository}/issues/{pr_number}/comments"):
        if comment.get("user", {}).get("login") != "github-actions[bot]":
            continue
        body = comment.get("body") or ""
        match = _FALLBACK_MARKER.search(body)
        if (
            match
            and match.group(1) == str(pr_number)
            and match.group(2) == sha
            and "— 処理開始" in body
        ):
            found.append((comment.get("id", 0), match.group(3), match.group(4)))
    return max(found)[1:] if found else None


def reconcile_fallback(call, repository, pr_number, sha, statuses, cutoff):
    rows = [
        row for row in statuses
        if row.get("context") == "hane/review-source"
        and parse_time(row.get("created_at"))
        and parse_time(row.get("created_at")) >= cutoff
    ]
    if not rows:
        return "noop"
    latest = max(rows, key=lambda row: row.get("id", 0))
    correlation = active_fallback_run(call, repository, pr_number, sha)
    if not correlation:
        return "noop"
    state = (
        "success"
        if latest.get("state") == "success"
        and (latest.get("description") or "").startswith("Review source: Copilot fallback for ")
        else "failure"
    )
    detail = (
        "Copilotによる代替レビューが完了しました。"
        if state == "success"
        else "Copilotによる代替レビューを正常に完了できませんでした。"
    )
    return aadw_notify.notify(
        call,
        state=state,
        process="codex-review-fallback",
        kind="pr",
        number=pr_number,
        sha=sha,
        repository=repository,
        run_id=correlation[0],
        attempt=correlation[1],
        detail=detail,
    )["action"]


def trusted_pr(pr, repository):
    owner = repository.split("/", 1)[0]
    return (
        pr.get("state") == "open"
        and pr.get("draft") is False
        and (pr.get("head", {}).get("repo") or {}).get("full_name") == repository
        and pr.get("user", {}).get("login") in (owner, "github-actions[bot]", "claude[bot]")
        and re.fullmatch(r"[0-9a-f]{40}", pr.get("head", {}).get("sha", "")) is not None
    )


def reconcile_pr(call, repository, pr, cutoff):
    if not trusted_pr(pr, repository):
        return 0
    number = int(pr["number"])
    sha = pr["head"]["sha"]
    statuses = pages(call, f"repos/{repository}/commits/{sha}/statuses")
    source = review_source(statuses)
    attempt_cache = {}
    writes = 0
    for context, process in CONTEXTS.items():
        rows = codex_lifecycle_rows(statuses) if context == "hane/codex-review" else [
            row for row in statuses if row.get("context") == context
        ]
        for generation in build_generations(rows):
            if not recent(generation, cutoff):
                continue
            action = notify_generation(
                call, repository, number, sha, context, generation,
                source=source if process == "codex-review" else None,
                attempt_cache=attempt_cache,
            )
            writes += action != "noop"
    writes += reconcile_fallback(call, repository, number, sha, statuses, cutoff) != "noop"
    return writes


def reconcile(call, repository, *, lookback_seconds=7200, now=None):
    now = now or dt.datetime.now(dt.timezone.utc)
    cutoff = now - dt.timedelta(seconds=lookback_seconds)
    pulls = pages(call, f"repos/{repository}/pulls?state=open")
    writes = 0
    for pr in pulls:
        writes += reconcile_pr(call, repository, pr, cutoff)
    return writes


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--lookback-seconds", type=int, default=7200)
    args = parser.parse_args()
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        print("AADW通知reconcile: repositoryが不正です。", file=sys.stderr)
        return 1
    try:
        writes = reconcile(aadw_notify.api, repository, lookback_seconds=max(300, args.lookback_seconds))
    except Exception as exc:
        print(f"AADW通知reconcileに失敗しました: {exc}", file=sys.stderr)
        return 1
    print(f"AADW通知reconcile: {writes}件を作成または更新しました。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

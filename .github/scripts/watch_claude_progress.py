"""Mirror already-public Claude issue progress; never read an SDK transcript.

This runs on a separate, read-only hosted job. Its output is informational and
must not be consumed as CI, review, implementation, or GUI-validation evidence.
"""
from __future__ import annotations

import datetime as dt
import html
import json
import os
import re
import time
import unicodedata
import urllib.error
import urllib.parse
import urllib.request

BOT_ID = 209825114
APP_ID = 1236702
MAX_RESPONSE_BYTES = 8 * 1024 * 1024
POLL_SECONDS = 15
HEARTBEAT_SECONDS = 60
TIMEOUT_SECONDS = 63 * 60
CONCLUSIONS = {"success", "failure", "cancelled", "skipped", "timed_out", "neutral", "action_required", "stale", "startup_failure"}


class Unavailable(Exception):
    """Deliberately carries no server response, URL, token, or exception text."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class GitHubAPI:
    def __init__(self, repository: str, token: str):
        if (not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]*/[A-Za-z0-9_.-]+", repository)
                or repository.rsplit("/", 1)[-1] in {".", ".."} or not token):
            raise Unavailable()
        self.root = "https://api.github.com/repos/" + repository
        self.token = token
        self.opener = urllib.request.build_opener(NoRedirect())

    def get(self, suffix: str):
        request = urllib.request.Request(self.root + suffix, headers={
            "Authorization": "Bearer " + self.token,
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "Hane-Claude-Progress",
        })
        try:
            with self.opener.open(request, timeout=10) as response:
                raw = response.read(MAX_RESPONSE_BYTES + 1)
                if len(raw) > MAX_RESPONSE_BYTES:
                    raise Unavailable()
                return json.loads(raw), 'rel="next"' in response.headers.get("Link", "")
        except (OSError, ValueError, RecursionError, urllib.error.URLError):
            raise Unavailable() from None

    def pages(self, suffix: str, key: str | None = None):
        items = []
        for page in range(1, 6):
            separator = "&" if "?" in suffix else "?"
            data, more = self.get(suffix + separator + f"per_page=100&page={page}")
            batch = data.get(key) if key and isinstance(data, dict) else data
            if not isinstance(batch, list):
                raise Unavailable()
            items.extend(batch)
            if not more:
                return items
        # Never silently select a comment from an incomplete result set.
        raise Unavailable()

    def job(self, run_id: int, attempt: int):
        jobs = self.pages(f"/actions/runs/{run_id}/attempts/{attempt}/jobs", "jobs")
        matches = [j for j in jobs if isinstance(j, dict) and j.get("name") == "implement" and j.get("run_id") == run_id]
        if len(matches) > 1:
            raise Unavailable()
        return matches[0] if matches else None

    def comments(self, issue: int, since: str):
        return self.pages(f"/issues/{issue}/comments?since=" + urllib.parse.quote(since, safe=""))


def timestamp(value):
    if not isinstance(value, str):
        return None
    try:
        result = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        return result if result.tzinfo else None
    except ValueError:
        return None


def choose_comment(comments, repository, issue, run_id, job, bound_id=None, attempt=1):
    """Bind once; exclude older attempts, foreign authors and ambiguous comments.

A run link is preferred. The upstream MCP updater can remove that link; a
single official comment created on this issue during the serialized implement
job is then an informational fallback, not exact-run validation evidence.
"""
    start = timestamp(job.get("started_at"))
    end = timestamp(job.get("completed_at"))
    if start is None:
        return None
    candidates = []
    for comment in comments:
        if not isinstance(comment, dict):
            continue
        user = comment.get("user") or {}
        app = comment.get("performed_via_github_app") or {}
        created = timestamp(comment.get("created_at"))
        body = comment.get("body")
        if not isinstance(user, dict) or not isinstance(app, dict):
            continue
        if (user.get("id") != BOT_ID or user.get("login") != "claude[bot]"
                or user.get("type") != "Bot" or app.get("id") != APP_ID
                or app.get("slug") != "claude"
                or comment.get("issue_url") != f"https://api.github.com/repos/{repository}/issues/{issue}"
                or type(comment.get("id")) is not int
                or created is None or created < start or (end is not None and created > end)
                or not isinstance(body, str) or len(body) > 65536):
            continue
        markers = re.findall(r"Hane progress run=([0-9]+) attempt=([0-9]+)", body)
        if markers and (str(run_id), str(attempt)) not in markers:
            continue
        links = re.findall(r"https://github\.com/" + re.escape(repository) + r"/actions/runs/([0-9]+)(?![0-9])", body)
        if links and str(run_id) not in links:
            continue
        if bound_id is not None and comment["id"] != bound_id:
            continue
        candidates.append(comment)
    if len(candidates) == 1:
        return candidates[0]
    marked = [c for c in candidates if f"Hane progress run={run_id} attempt={attempt}" in c["body"]]
    if len(marked) == 1:
        return marked[0]
    linked = [c for c in candidates if re.search(r"/actions/runs/" + str(run_id) + r"(?![0-9])", c["body"])]
    return linked[0] if len(linked) == 1 else None


def safe_line(value: str) -> str:
    # The source is already public; additionally strip common credentials,
    # terminal controls and workflow-command syntax. This is not secret DLP.
    value = html.unescape(value)
    value = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", value)
    value = "".join(c if c == "\t" or not unicodedata.category(c).startswith("C") else " " for c in value)
    value = re.sub(r"<(?:[^>]+)>", "", value)
    value = re.sub(r"\b(?:github_pat_|gh[pousr]_)[A-Za-z0-9_]+", "[REDACTED]", value)
    value = re.sub(r"\bsk-[A-Za-z0-9_-]{12,}", "[REDACTED]", value)
    value = re.sub(r"(?i)\b(?:authorization\s*[:=]\s*|bearer\s+)\S+(?:\s+\S+)?", "[REDACTED]", value)
    value = value.replace("::", ": :").replace("##[", "# #[")
    return " ".join(value.split())[:400]


def public_lines(body: str):
    body = re.sub(r"<!--.*?-->", "", body, flags=re.S)
    body = re.sub(r"-----BEGIN [^-]*PRIVATE KEY-----.*?-----END [^-]*PRIVATE KEY-----", "[REDACTED]", body, flags=re.S)
    fence = None
    result = []
    for raw in body.splitlines():
        line = raw.strip()
        marker = re.match(r"^(`{3,}|~{3,})", line)
        if marker:
            token = marker.group(1)
            if fence is None:
                fence = token
            elif token[0] == fence[0] and len(token) >= len(fence):
                fence = None
            continue
        if fence or raw.startswith(("    ", "\t")):
            continue
        if not line or line.startswith(("#", "---", "<", "[View job", "[View branch", "**Claude ", "Hane progress run=")):
            continue
        cleaned = safe_line(line)
        if cleaned:
            result.append(cleaned)
        if len(result) == 100:
            break
    return result


def watch(api, repository, issue, run_id, attempt, emit, *, clock=time.monotonic,
          sleep=time.sleep, timeout=TIMEOUT_SECONDS):
    began = last_heartbeat = clock()
    last_update = None
    previous = set()
    printed = 0
    bound_id = None
    failures = 0
    emit("[監視] Claude の公開進捗を確認します。これは実装・テストの合格判定ではありません。")
    while clock() - began < timeout:
        try:
            job = api.job(run_id, attempt)
            if job and timestamp(job.get("started_at")) is not None:
                # Fetch after job state, so the final poll also picks up the final comment.
                comments = api.comments(issue, job["started_at"])
                comment = choose_comment(comments, repository, issue, run_id, job, bound_id, attempt)
                if comment:
                    if bound_id is None:
                        bound_id = comment["id"]
                        emit(f"[監視] Issue #{issue} の Claude コメントを表示します。")
                    lines = public_lines(comment["body"])
                    for line in lines:
                        if line not in previous and printed < 4000:
                            printed += 1
                            last_update = clock()
                            emit("[Claude の報告] " + line)
                    previous = set(lines)
            failures = 0
            if job and job.get("status") == "completed":
                conclusion = job.get("conclusion")
                conclusion = conclusion if isinstance(conclusion, str) and conclusion in CONCLUSIONS else "unknown"
                emit(f"[監視] 実装ジョブが終了しました: {conclusion}。CI・GUI 検証の結果は別に確認してください。")
                return 0
        except Unavailable:
            failures += 1
            if failures == 1:
                emit("[監視] GitHub から進捗を取得できません。実装が停止したとは判断していません。")
            if failures >= 5:
                emit("[監視] 取得失敗が続いたため表示を終了します。実装ジョブは停止・再実行しません。")
                return 2
        now = clock()
        if now - last_heartbeat >= HEARTBEAT_SECONDS:
            elapsed = int(now - began)
            silence = "まだ進捗報告を確認できていません。" if last_update is None else f"新しい進捗報告は {int(now - last_update)} 秒間ありません。"
            emit(f"[監視] 表示処理は継続中（経過 {elapsed} 秒）。{silence}Claude の動作継続を保証する表示ではありません。")
            last_heartbeat = now
        sleep(POLL_SECONDS)
    emit("[監視] 表示の待機上限に達しました。実装ジョブの成功・失敗は未判定です。")
    return 2


def main():
    def emit(message):
        stamp = dt.datetime.now(dt.timezone.utc).strftime("%H:%M:%S UTC")
        print(f"[{stamp}] {message}", flush=True)
    try:
        repository = os.environ["REPOSITORY"]
        values = [os.environ[name] for name in ("ISSUE_NUMBER", "IMPLEMENTATION_RUN_ID", "IMPLEMENTATION_RUN_ATTEMPT")]
        if any(not re.fullmatch(r"[1-9][0-9]{0,19}", value) for value in values):
            raise Unavailable()
        issue, run_id, attempt = map(int, values)
        api = GitHubAPI(repository, os.environ["GH_TOKEN"])
        return watch(api, repository, issue, run_id, attempt, emit)
    except (KeyError, Unavailable):
        emit("[監視] 設定を確認できないため表示を開始できません。")
        return 2
    except KeyboardInterrupt:
        emit("[監視] 表示を中断しました。実装ジョブの結果とは区別してください。")
        return 2


if __name__ == "__main__":
    raise SystemExit(main())

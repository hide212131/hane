"""Bounded receipt reads shared by GUI reconciliation and final judgement."""
import io
import json
import subprocess
import zipfile


def artifact_json(api, run_id, name, filename):
    rows = api.pages(api.repo(f'actions/runs/{run_id}/artifacts'), 'artifacts')
    candidates = [a for a in rows if a['name'] == name and a.get('expired') is False]
    if len(candidates) != 1 or candidates[0]['size_in_bytes'] > 2 * 1024 * 1024:
        raise ValueError('missing, duplicate, expired or oversized receipt artifact')
    archive = subprocess.run(['gh', 'api', api.repo(f'actions/artifacts/{candidates[0]["id"]}/zip')],
                             capture_output=True, timeout=60, check=True).stdout
    if len(archive) > 2 * 1024 * 1024:
        raise ValueError('oversized artifact download')
    try:
        with zipfile.ZipFile(io.BytesIO(archive)) as zipped:
            if zipped.namelist().count(filename) != 1:
                raise ValueError('missing or duplicate receipt member')
            info = zipped.getinfo(filename)
            if info.file_size > 1024 * 1024:
                raise ValueError('oversized receipt')
            value = json.loads(zipped.read(info))
            if not isinstance(value, dict):
                raise ValueError('receipt is not an object')
            return value
    except zipfile.BadZipFile as exc:
        raise ValueError('invalid receipt archive') from exc

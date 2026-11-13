#!/usr/bin/env python3
"""Automated remote build + install for the rust-agent-harness `luna` binary.

Triggers the `release-build.yml` workflow on GitHub Actions, waits for it to
finish, downloads the `luna-binary` artifact, and installs it locally.

Environment variables:
  GITHUB_TOKEN  - PAT with `workflow` + `contents` scope (required)
  GITHUB_REPO   - owner/repo            (default: daryllvelonio2-bit/luna-)
  GITHUB_REF    - branch/tag/SHA        (default: main)
  INSTALL_DIR   - install directory     (default: ~/.local/bin)
  WORKFLOW_ID   - workflow filename/id  (default: release-build.yml)
"""

import io
import json
import os
import stat
import sys
import time
import zipfile
import urllib.error
import urllib.request

GITHUB_TOKEN = os.environ.get("GITHUB_TOKEN", "")
GITHUB_REPO = os.environ.get("GITHUB_REPO", "daryllvelonio2-bit/luna-")
GITHUB_REF = os.environ.get("GITHUB_REF", "main")
INSTALL_DIR = os.environ.get("INSTALL_DIR", os.path.expanduser("~/.local/bin"))
WORKFLOW_ID = os.environ.get("WORKFLOW_ID", "release-build.yml")
ARTIFACT_NAME = "luna-binary"
API_BASE = "https://api.github.com"
POLL_INTERVAL = 15  # seconds
TIMEOUT_SECS = 30 * 60  # 30 minutes


class BuildError(RuntimeError):
    pass


class _StripAuthOnRedirect(urllib.request.HTTPRedirectHandler):
    """Follow a redirect without carrying the API token to the new host.

    GitHub answers an artifact download with a 302 to a pre-signed Blob URL.
    urllib forwards request headers on redirect, so the `Authorization` header
    reaches Azure, which rejects it as malformed and the download fails with
    HTTP 401 even though the build succeeded. The signed URL needs no token.
    """

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
        if redirected is not None:
            redirected.remove_header("Authorization")
        return redirected


OPENER = urllib.request.build_opener(_StripAuthOnRedirect)


def api_request(method, path, data=None, parse=True):
    """Make an authenticated GitHub API request."""
    url = f"{API_BASE}{path}"
    headers = {
        "Accept": "application/vnd.github+json",
        "Authorization": f"Bearer {GITHUB_TOKEN}",
        "User-Agent": "luna-auto-build/1.0",
    }
    body = None
    if data is not None:
        body = json.dumps(data).encode("utf-8")
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with OPENER.open(req, timeout=60) as resp:
            raw = resp.read()
            if not parse:
                return raw
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode("utf-8", errors="replace")
        raise BuildError(f"GitHub API {method} {path} -> HTTP {exc.code}: {detail}")


def resolve_workflow_id():
    """Return the numeric workflow id for WORKFLOW_ID."""
    workflows = api_request("GET", f"/repos/{GITHUB_REPO}/actions/workflows")
    for wf in workflows.get("workflows", []):
        if wf.get("path", "").endswith(WORKFLOW_ID) or wf.get("name") == WORKFLOW_ID:
            return wf["id"]
    raise BuildError(f"Workflow '{WORKFLOW_ID}' not found in repo {GITHUB_REPO}")


def trigger_build(workflow_id):
    """Trigger a workflow_dispatch run; return the run id."""
    api_request(
        "POST",
        f"/repos/{GITHUB_REPO}/actions/workflows/{workflow_id}/dispatches",
        data={"ref": GITHUB_REF, "inputs": {"ref": GITHUB_REF}},
    )
    # Wait a moment for the run to be created, then find the most recent run.
    time.sleep(5)
    runs = api_request(
        "GET",
        f"/repos/{GITHUB_REPO}/actions/workflows/{workflow_id}/runs?per_page=5",
    )
    matching = [r for r in runs.get("workflow_runs", []) if r.get("head_branch") == GITHUB_REF]
    if not matching:
        # Fall back to the most recent run regardless of branch.
        matching = runs.get("workflow_runs", [])
    if not matching:
        raise BuildError("No workflow runs found after dispatch")
    run = matching[0]
    print(f"Triggered build run #{run['run_number']} (id {run['id']}) on ref {GITHUB_REF}")
    return run["id"]


def wait_for_completion(run_id):
    """Poll the run until it finishes; raise on failure."""
    deadline = time.time() + TIMEOUT_SECS
    while time.time() < deadline:
        run = api_request("GET", f"/repos/{GITHUB_REPO}/actions/runs/{run_id}")
        status = run.get("status")
        conclusion = run.get("conclusion")
        print(f"Run status: {status} ({conclusion or 'pending'})")
        if status == "completed":
            if conclusion == "success":
                return
            raise BuildError(f"Build failed with conclusion: {conclusion}")
        time.sleep(POLL_INTERVAL)
    raise BuildError("Build timed out waiting for completion")


def download_artifact(run_id):
    """Download the luna-binary artifact zip; return raw bytes."""
    artifacts = api_request(
        "GET", f"/repos/{GITHUB_REPO}/actions/runs/{run_id}/artifacts"
    )
    for art in artifacts.get("artifacts", []):
        if art.get("name") == ARTIFACT_NAME:
            print(f"Downloading artifact '{ARTIFACT_NAME}' (id {art['id']})...")
            raw = api_request(
                "GET",
                f"/repos/{GITHUB_REPO}/actions/artifacts/{art['id']}/zip",
                parse=False,
            )
            return raw
    available = [a.get("name") for a in artifacts.get("artifacts", [])]
    raise BuildError(
        f"Artifact '{ARTIFACT_NAME}' not found. Available: {available}"
    )


def install_binary(zip_bytes):
    """Extract the `luna` binary from the zip and install it to INSTALL_DIR."""
    os.makedirs(INSTALL_DIR, exist_ok=True)
    dest = os.path.join(INSTALL_DIR, "luna")
    # A running `luna` holds the destination inode open, and writing that path fails with
    # ETXTBSY. Write beside it and rename: rename swaps the directory entry atomically, the
    # running process keeps the old inode, and the next launch gets the new binary.
    staging = dest + ".new"
    try:
        with zipfile.ZipFile(io.BytesIO(zip_bytes)) as zf:
            # Find the `luna` file inside the zip (artifact zips preserve path structure).
            candidates = [n for n in zf.namelist() if n.endswith("/luna") or n == "luna"]
            if not candidates:
                raise BuildError(f"`luna` binary not found in artifact: {zf.namelist()}")
            with zf.open(candidates[0]) as src, open(staging, "wb") as out:
                out.write(src.read())
        st = os.stat(staging)
        os.chmod(staging, st.st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        os.replace(staging, dest)
    finally:
        if os.path.exists(staging):
            os.remove(staging)
    print(f"Installed `luna` to {dest}")
    print(f"Run: {dest} --help")


def main():
    if not GITHUB_TOKEN:
        print("ERROR: GITHUB_TOKEN environment variable is required.", file=sys.stderr)
        sys.exit(2)

    workflow_id = resolve_workflow_id()
    run_id = trigger_build(workflow_id)
    wait_for_completion(run_id)
    zip_bytes = download_artifact(run_id)
    install_binary(zip_bytes)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Trigger the CI workflow (tests + build) on GitHub Actions and wait for the result.

Companion to auto_build_install.py, which triggers the release build and installs
the binary. This one runs the CI suite and only reports pass/fail; it does not
download anything.

Environment variables:
  GITHUB_TOKEN  - PAT with `workflow` + `contents` scope (required)
  GITHUB_REPO   - owner/repo            (default: daryllvelonio2-bit/luna-)
  GITHUB_REF    - branch/tag/SHA to test (default: main)
  CI_WORKFLOW   - workflow file to run  (default: ci.yml)
"""

import os
import sys

# Reuse the API helpers from the build script: one implementation of dispatch,
# run discovery, and completion polling for both remote workflows.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import auto_build_install as gha  # noqa: E402


def main() -> int:
    if not gha.GITHUB_TOKEN:
        print("ERROR: GITHUB_TOKEN environment variable is required.", file=sys.stderr)
        return 2

    gha.WORKFLOW_ID = os.environ.get("CI_WORKFLOW", "ci.yml")
    workflow_id = gha.resolve_workflow_id()
    run_id = gha.trigger_build(workflow_id)
    try:
        gha.wait_for_completion(run_id)
    except gha.BuildError as exc:
        print(f"CI failed: {exc}", file=sys.stderr)
        return 1
    print("CI passed: tests and build succeeded on GitHub Actions.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

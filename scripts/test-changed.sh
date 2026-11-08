#!/usr/bin/env bash
# Run only the tests affected by the current diff.
#
# The implementation lives in test-changed.py next to this file; this wrapper
# exists so the command is the same from any directory and does not depend on
# the caller remembering `python3`.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$here/test-changed.py" "$@"

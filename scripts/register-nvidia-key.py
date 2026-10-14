#!/usr/bin/env python3
"""Register the NVIDIA NIM key from the Shiina CLI config into the Luna harness.

The harness (v0.1) stores credentials as plaintext JSON at ~/.luna/credentials.json
(see crates/harness-providers/src/auth.rs). This script is the only supported way to
get a key in: it never prints the key, only the fact that it was written.

Usage:  python3 scripts/register-nvidia-key.py [--provider nvidia] [--label nim-main]
"""
import argparse
import json
import os
import re
import stat
import sys
import uuid
from pathlib import Path

SHIINA_CONFIG = Path(os.environ.get("SHIINA_CONFIG", "~/.shiina/config.yaml")).expanduser()
LUNA_CREDS = Path(os.environ.get("LUNA_CREDENTIALS", "~/.luna/credentials.json")).expanduser()


def read_key(config: Path, provider: str) -> str:
    """Pull providers.<provider>.api_key out of the Shiina config without a YAML dep."""
    text = config.read_text()
    match = re.search(
        rf"^  {re.escape(provider)}:\n(?:.*\n)*?    api_key: (\S+)\s*$", text, re.MULTILINE
    )
    if not match:
        sys.exit(f"no providers.{provider}.api_key in {config}")
    return match.group(1).strip().strip('"\'')


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--provider", default="nvidia")
    ap.add_argument("--label", default="nim-main")
    args = ap.parse_args()

    key = read_key(SHIINA_CONFIG, args.provider)
    store = {"providers": {}}
    if LUNA_CREDS.exists():
        store = json.loads(LUNA_CREDS.read_text() or '{"providers": {}}')
    store.setdefault("providers", {})

    entry = {
        "id": str(uuid.uuid4()),
        "label": args.label,
        "api_key": key,
        "is_priority": True,
        "usage_count": 0,
        "last_used": None,
        "is_temporarily_disabled": False,
    }
    store["providers"][args.provider] = {
        "strategy": "fill_first",
        "credentials": [entry],
        "current_round_robin_index": 0,
    }

    LUNA_CREDS.parent.mkdir(parents=True, exist_ok=True)
    LUNA_CREDS.write_text(json.dumps(store, indent=2))
    LUNA_CREDS.chmod(stat.S_IRUSR | stat.S_IWUSR)
    print(f"wrote {LUNA_CREDS} provider={args.provider} label={args.label} key=***{key[-4:]}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

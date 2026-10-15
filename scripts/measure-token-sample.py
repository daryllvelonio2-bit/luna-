"""Measure what the harness model reports for one fixed prose sample.

Reads the NVIDIA NIM key out of ~/.luna/credentials.json, sends the sample in
crates/harness-core/tests/fixtures/token_sample.txt as a single user message, and prints only the
usage numbers and the sample's character count. The key is never printed.
"""

import json
import os
import urllib.request

CREDENTIALS = os.path.expanduser("~/.luna/credentials.json")
SAMPLE = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "crates",
    "harness-core",
    "tests",
    "fixtures",
    "token_sample.txt",
)
MODEL = "deepseek-ai/deepseek-v4.1-flash"
ENDPOINT = "https://integrate.api.nvidia.com/v1/chat/completions"

with open(CREDENTIALS, encoding="utf-8") as handle:
    stored = json.load(handle)
key = stored["providers"]["nvidia"]["credentials"][0]["api_key"]

with open(SAMPLE, encoding="utf-8") as handle:
    sample = handle.read()

body = json.dumps(
    {
        "model": MODEL,
        "messages": [{"role": "user", "content": sample}],
        "max_tokens": 1024,
        "temperature": 0.0,
    }
).encode()

request = urllib.request.Request(
    ENDPOINT,
    data=body,
    headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"},
)

with urllib.request.urlopen(request, timeout=120) as response:
    payload = json.load(response)

print("sample_chars =", len(sample))
print("usage =", payload.get("usage"))

# The `luna eval` regression gate

One page, for whoever adds the next case. Code: `crates/harness-cli/src/eval/`. Suite: `tests/eval/`.

`luna eval run` drives each case through the **real agent loop** (`harness_core::Agent`) and checks
what came out. It is the only automated check that a change to the loop, the provider layer or a
tool did not make the whole turn worse — the unit tests under `crates/*/tests` cover pieces, not a
turn end to end.

## Running it

```bash
cargo test --workspace                     # includes the offline suite (crates/harness-cli/tests/eval_suite_test.rs)
target/release/luna eval run --suite tests/eval --offline          # exit 0, one line per case
target/release/luna eval run --suite tests/eval --filter write-then-read
target/release/luna eval run --suite tests/eval --live --provider nvidia --model deepseek-ai/deepseek-v4.1-flash
```

- **`--offline` is the default** (also when only `--suite` is given): the scripted provider replays
  the case's own canned turns. No credential, no network, no quota, deterministic. Use this in CI.
- **`--live`** drives the real provider. With no credential registered for the provider the suite
  prints `skip  skipped: no credential` and still exits 0; it is skipped, not failed.
- `--filter <substring>` selects cases by name; `--report <path>` (default `eval-report.json`)
  chooses where the machine-readable report goes. Parent directories in the path are created if
  missing.
- Exit codes: `0` all pass or skipped, `1` at least one case failed — or the suite itself could not
  be loaded (bad YAML, missing directory, filter matched nothing), which prints
  `Error: ConfigurationError(…)` and exits 1 as well.
- Default live model is `deepseek-ai/deepseek-v4.1-flash` on `nvidia` (the fleet's model under test,
  `AGENT_RULES.md` §2); override with `--model`.

## Case format

One YAML file per case in the suite directory (`tests/eval/`). Unknown keys are rejected, so a typo
fails the run instead of being ignored.

```yaml
case: write-then-read            # required; unique; [A-Za-z0-9._-]; also the --filter name
prompt: The user turn handed to the agent loop.     # required
tools: [eval_write_file, eval_read_file]            # built-in eval tools to register (see below)
approval:
  deny: [eval_write_file]        # calls a reviewer would refuse
permissions:                     # optional standing rules (tools.permissions shape); a deny here
  - action: deny                 # refuses before the registry is reached, so no call is observed
    tool: eval_write_file
    arguments: { path: "blocked*" }
script:                          # OFFLINE ONLY: the canned model turns, in order
  - tool_calls:
      - name: eval_write_file
        arguments: { path: "out.txt", content: "luna-eval-42\n" }   # free-form JSON per tool
  - answer: out.txt contains luna-eval-42.                        # last turn = the final answer
expect:
  tool_calls:                    # the observed calls must match this list one for one, in order
    - name: eval_write_file
      arguments_contain: { path: "out.txt" }   # each key must equal this value exactly
      error: false                             # optional; omit to ignore error status
  final_answer_contains: ["luna-eval-42"]      # substrings of the final answer
  files:
    - path: out.txt              # sandbox-relative, no absolute paths, no ".."
      exists: true
      content_equals: "luna-eval-42\n"          # exact bytes, trailing newline included
```

Assertions that are not in `expect` are not checked. A case fails on the **first** failed
assertion; the report names it.

Things worth knowing before writing a case:

- **The sandbox** is `<tmp>/luna-eval-<pid>/<case>/`, wiped before each run. Only the eval tools can
  reach it; nothing in a case touches `~/.luna` or the repository.
- **`tokens`** in the report: offline are whitespace word counts (reproducible, not tokenizer-exact);
  live are `null` because the non-streaming provider path drops the API's usage field.
- **Approval** is applied by a registry wrapper in the eval runner, because the agent loop has no
  gate yet (`AgentConfig::require_hitl_for_mutation` is unused — card `t_c4f9c07d` owns that). A
  denied call returns `Human approval rejected: …` as a tool result and never reaches the tool.
- **Memory and notes are no-op engines** in a case: the suite measures the loop, the
  provider and the case's tools, not persistence. A case that wants a real tool goes through
  `register_builtin` and the tool list below.
- A live backend ignores `script:` — the real model decides its own tool calls, so a live run of case
  whose `expect` block is strict can legitimately fail. That is a finding, not a flake.

## Built-in eval tools

| Tool | Arguments | Behaviour |
| --- | --- | --- |
| `eval_write_file` | `path`, `content` | Writes exact bytes inside the sandbox. |
| `eval_read_file` | `path` | Returns the file verbatim, or a tool error if it is missing. |
| `eval_echo` | `text` | Always succeeds; returns the text. |
| `eval_fail` | `message` | Always fails, to exercise failure recovery. |

Malformed arguments (missing key, wrong type, a payload that is not an object) are a tool error the
model sees, never a panic — that is what `malformed_tool_arguments.yaml` pins down.

## The cases that ship

`read_only_turn` (zero tool calls) · `write_then_read` (exact bytes on disk) · `denied_approval`
(nothing written, rejection reaches the model) · `failing_tool_recovery` (a failing tool does not end
the turn) · `malformed_tool_arguments` (tool error, loop continues) · `permission_rule_allow` (a
standing allow rule does not block) · `permission_rule_deny` (a standing deny rule refuses before
the registry is reached) · `missing-file-read-recovery` (a missing file errors, the loop recovers
and the file ends up written) · `failure-counter-reset` (three failures in a row stay under the cap
and the next success clears the counter) · `denied-call-not-a-failure` (a policy denial is not tool
thrashing: it does not count toward the failure cap and does not end the turn) ·
`sandbox-path-escape` (a path outside the sandbox is refused, the in-sandbox write still lands) ·
`multi-call-single-turn` (several tool calls in one assistant message all execute, in order).

Adding a case: drop a YAML file in `tests/eval/`, run
`target/release/luna eval run --suite tests/eval --offline --filter <your-case>`, and confirm it
fails when you break one assertion by hand — a case that cannot fail is not a test.

# Failed turns audit — 2026-11-06

## What the number actually means

`/usage` reported **166 failed turn(s)** across **649** recorded turns, with **31 denied call(s)** and **9510** tool calls. The label was wrong: the ledger stores `ok = false` when **any tool call in the turn returned `is_error`**, so one failed `ls`, one missing file, or one failing `cargo test` marks the whole turn "failed" even when the model recovered and answered. It never meant the conversation turn failed.

In the transcripts there are **753 turns**; **215** contain at least one errored tool result, and **168 of those 215 (78%) still ended with a final assistant answer**. The log contains only **4** genuine `Halt triggered: 5 consecutive tool failures` events. So the honest reading is: *166 ledger rows had a tool call return non-zero*, not *166 turns failed*.

## Every tool error by cause

Across `sessions.db` there are **540** errored tool results (many turns have several).

| Count | Cause | Typical tool |
|---:|---|---|
| 170 | Command failed (tests/build/shell) | `terminal` (169), `write_file` (1) |
| 86 | Wrong/unknown path | `read_file` (31), `terminal` (21), `list_files` (19) |
| 75 | Other | `read_note` (19), `desktop_click` (13), `skill` (8) |
| 64 | Timeout | `terminal` (59), `grep` (5) |
| 36 | Invalid tool arguments | `read_file` (33), `search_files` (2), `terminal` (1) |
| 26 | Patch target mismatch | `patch` (20), `edit` (6) |
| 19 | Denied: background subagent cannot prompt | `terminal` (18), `search_notes` (1) |
| 17 | Sandbox: path outside workspace | `read_file` (10), `list_files` (3), `search_files` (2) |
| 15 | Denied: approval prompt timed out | `terminal` (13), `desktop_click` (1), `patch` (1) |
| 11 | Stale file snapshot | `patch` (8), `write_file` (3) |
| 11 | Unknown tool name | `shell` (4), `desktop_keys` (2), `glob` (2) |
| 7 | Denied: plan mode | `terminal` (5), `patch` (2) |
| 3 | Denied: user/policy | `terminal` (2), `delegate_task` (1) |

### Recurring exact errors

| Count | Tool | Error |
|---:|---|---|
| 38 | `terminal` | Command timed out after 120s; it and everything it had started were killed. A cold build or a full test suite needs a bigger budget: pass timeout_s (e.g. 600) for this call, or run |
| 33 | `read_file` | Missing required argument 'path' |
| 18 | `terminal` | approval denied: delegated subagent: interactive approval is unavailable in background execution |
| 13 | `terminal` | approval denied: no answer within 120s |
| 12 | `terminal` | exit code: 2    FAIL  Tests\Feature\Ui\ClinicalIntake\PatientIndexTest   ⨯ the directory renders for an allowed role and is refused for a disallowed one   ⨯ the list searches filte |
| 8 | `terminal` | Command timed out after 300s; it and everything it had started were killed. A cold build or a full test suite needs a bigger budget: pass timeout_s (e.g. 600) for this call, or run |
| 8 | `skill` |  |
| 6 | `terminal` | exit code: 2 ls: cannot access 'crates/': No such file or directory |
| 6 | `desktop_click` | AMBIGUOUS_TARGET: More than one live element matches section, so the action was not sent |
| 5 | `terminal` | approval denied: plan mode active: mutating tool calls are disabled (press Tab to toggle plan mode off) |
| 5 | `read_file` | Failed to read file: Is a directory (os error 21) |
| 5 | `grep` | {"message":"Search timed out after 30 seconds. Consider using a more specific path or pattern.","type":"tool.execution"} |
| 4 | `terminal` | exit code: 1 cp: cannot create regular file '/home/janelle/.local/bin/luna': Text file busy |
| 4 | `read` |  |
| 4 | `edit` | {"message":"Could not find oldString in /home/janelle/Documents/GitHub/rust-agent-harness/crates/harness-cli/src/commands/chat/render/mod.rs. It must match exactly, including white |
| 4 | `shell` | Tool 'shell' not found in registry |
| 4 | `search_files` | Path not found: 'crates/harness-cli' (relative to workspace root '/home/janelle') |
| 4 | `desktop_click` | ACTION_NOT_SUPPORTED: The element offers no action named 'click' or 'press' or 'activate' |
| 3 | `web_fetch` | https://github.com disallows this path for 'LunaHarness/0.1 (+https://github.com/luna-ai/rust-agent-harness)' in robots.txt |
| 3 | `terminal` | exit code: 1 |

## Failed-turn rows by model

| Failed rows | Model |
|---:|---|
| 54 | `antigravity/gemini-3.8-flash` |
| 20 | `nvidia/minimaxai/minimax-m3.1-flash` |
| 17 | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` |
| 16 | `nvidia/deepseek-ai/deepseek-v4.1-flash` |
| 16 | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` |
| 10 | `nvidia/nvidia/nemotron-3-ultra-550b-a55b` |
| 7 | `nvidia/z-ai/glm-5.3-flash` |
| 6 | `nvidia/mistralai/mistral-large-4-1t-a52b` |
| 5 | `kilocode/kilo-auto/free` |
| 5 | `opencode/deepseek-v4.1-flash-free` |
| 3 | `antigravity/gpt-oss-120b` |
| 2 | `nvidia/nvidia/nemotron-3.5-lightning-30b-a3b` |
| 2 | `opencode/flint-40b-free` |
| 2 | `opencode-cli/opencode/deepseek-v4.1-flash-free` |
| 1 | `tokenharbor/deepseek-v4.1-flash:free` |

## Sessions with the most tool errors

| Errors | Session | Model |
|---:|---|---|
| 41 | `s94066114` | `antigravitygemini-3.8-flash` |
| 39 | `sb6989a71` | `antigravitygemini-3.8-flash` |
| 25 | `se42f23` | `nvidianvidia/nemotron-3.5-super-vl-120b-a12b` |
| 20 | `sub_20261105_075453_1` | `(unset)` |
| 19 | `saaedfedb` | `nvidiaz-ai/glm-5.3-flash` |
| 18 | `s95b9d46d` | `nvidianvidia/nemotron-3.5-super-vl-120b-a12b` |
| 18 | `sf29f8384` | `nvidiaminimaxai/minimax-m3.1-flash` |
| 16 | `s53f0fbff` | `nvidiaminimaxai/minimax-m3.1-flash` |
| 16 | `s8dae9fa1` | `antigravitygemini-3.8-flash` |
| 15 | `s41253240` | `nvidianvidia/nemotron-3.5-super-vl-120b-a12b` |
| 14 | `s06a267f6` | `nvidianvidia/nemotron-3.5-lightning-30b-a3b` |
| 14 | `saac38806` | `antigravitygemini-3.8-flash` |
| 13 | `s2e1680f5` | `nvidiaz-ai/glm-5.3-flash` |
| 12 | `s3329dc1a` | `opencodedeepseek-v4.1-flash-free` |
| 12 | `sub_20261102_052631_2` | `(unset)` |

## All 166 recorded failed-turn rows

| # | When (UTC) | Session | Model | Tool calls | Denied | Source |
|---:|---|---|---|---:|---:|---|
| 1 | 2026-10-28T18:09:48 | `safee2818` | `antigravity/gemini-3.8-flash` | 102 | 0 | chat |
| 2 | 2026-10-28T18:17:25 | `s5007d91c` | `nvidia/minimaxai/minimax-m3.1-flash` | 15 | 0 | chat |
| 3 | 2026-10-29T02:46:33 | `s4b27f1bd` | `nvidia/z-ai/glm-5.3-flash` | 7 | 0 | chat |
| 4 | 2026-10-29T03:23:13 | `sa051e13e` | `antigravity/gemini-3.8-flash` | 99 | 0 | chat |
| 5 | 2026-10-29T03:29:49 | `sa051e13e` | `antigravity/gemini-3.8-flash` | 8 | 0 | chat |
| 6 | 2026-10-29T08:53:27 | `sd14d41ca` | `antigravity/gpt-oss-120b` | 7 | 0 | chat |
| 7 | 2026-10-29T08:55:06 | `sd14d41ca` | `kilocode/kilo-auto/free` | 4 | 0 | chat |
| 8 | 2026-10-29T09:05:52 | `sd14d41ca` | `kilocode/kilo-auto/free` | 30 | 0 | chat |
| 9 | 2026-10-29T09:20:36 | `sa051e13e` | `nvidia/z-ai/glm-5.3-flash` | 33 | 1 | chat |
| 10 | 2026-10-29T10:36:34 | `s2c2b7047` | `antigravity/gpt-oss-120b` | 9 | 0 | chat |
| 11 | 2026-10-29T10:42:57 | `sd9ee4ef5` | `nvidia/minimaxai/minimax-m3.1-flash` | 137 | 0 | chat |
| 12 | 2026-10-29T10:52:21 | `sa1f39834` | `kilocode/kilo-auto/free` | 12 | 0 | chat |
| 13 | 2026-10-29T10:54:42 | `s4b1f1dc3` | `nvidia/minimaxai/minimax-m3.1-flash` | 42 | 0 | chat |
| 14 | 2026-10-29T10:57:05 | `sa1f39834` | `kilocode/kilo-auto/free` | 5 | 0 | chat |
| 15 | 2026-10-29T12:20:12 | `sa1f39834` | `nvidia/minimaxai/minimax-m3.1-flash` | 52 | 0 | chat |
| 16 | 2026-10-29T14:01:14 | `s575284d2` | `kilocode/kilo-auto/free` | 8 | 0 | chat |
| 17 | 2026-10-29T14:15:58 | `s575284d2` | `antigravity/gemini-3.8-flash` | 20 | 0 | chat |
| 18 | 2026-10-29T14:26:03 | `s9ce280e5` | `nvidia/nvidia/nemotron-3-ultra-550b-a55b` | 3 | 0 | chat |
| 19 | 2026-10-29T14:38:16 | `s9ce280e5` | `nvidia/nvidia/nemotron-3-ultra-550b-a55b` | 20 | 0 | chat |
| 20 | 2026-10-29T14:39:24 | `s575284d2` | `nvidia/z-ai/glm-5.3-flash` | 32 | 0 | chat |
| 21 | 2026-10-29T15:53:33 | `s3d395026` | `nvidia/nvidia/nemotron-3-ultra-550b-a55b` | 5 | 0 | chat |
| 22 | 2026-10-29T15:58:06 | `sd9ee4ef5` | `antigravity/gemini-3.8-flash` | 32 | 0 | chat |
| 23 | 2026-10-29T16:21:08 | `s3d395026` | `nvidia/nvidia/nemotron-3-ultra-550b-a55b` | 39 | 0 | chat |
| 24 | 2026-10-29T17:43:49 | `s2e1680f5` | `nvidia/minimaxai/minimax-m3.1-flash` | 9 | 0 | chat |
| 25 | 2026-10-29T18:30:33 | `s4ea92ed3` | `nvidia/minimaxai/minimax-m3.1-flash` | 5 | 0 | chat |
| 26 | 2026-10-29T19:30:05 | `s2e1680f5` | `antigravity/gemini-3.8-flash` | 23 | 0 | chat |
| 27 | 2026-10-29T19:41:00 | `s2e1680f5` | `antigravity/gemini-3.8-flash` | 49 | 0 | chat |
| 28 | 2026-10-29T19:50:36 | `s2e1680f5` | `antigravity/gemini-3.8-flash` | 12 | 0 | chat |
| 29 | 2026-10-29T20:05:04 | `sd38c92c6` | `nvidia/minimaxai/minimax-m3.1-flash` | 3 | 0 | chat |
| 30 | 2026-10-29T20:51:16 | `sa83214da` | `nvidia/mistralai/mistral-large-4-1t-a52b` | 8 | 0 | chat |
| 31 | 2026-10-29T21:04:06 | `sa83214da` | `nvidia/mistralai/mistral-large-4-1t-a52b` | 37 | 1 | chat |
| 32 | 2026-10-29T21:50:27 | `sa83214da` | `nvidia/mistralai/mistral-large-4-1t-a52b` | 101 | 0 | chat |
| 33 | 2026-10-30T06:54:36 | `sa83214da` | `nvidia/mistralai/mistral-large-4-1t-a52b` | 10 | 0 | chat |
| 34 | 2026-10-30T10:54:20 | `s31107496` | `nvidia/minimaxai/minimax-m3.1-flash` | 70 | 0 | chat |
| 35 | 2026-10-30T11:40:59 | `s31107496` | `nvidia/minimaxai/minimax-m3.1-flash` | 41 | 5 | chat |
| 36 | 2026-10-30T15:53:37 | `s31107496` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 231 | 0 | chat |
| 37 | 2026-10-30T16:59:19 | `scf4e852e` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 4 | 0 | chat |
| 38 | 2026-10-30T17:22:02 | `s31107496` | `nvidia/minimaxai/minimax-m3.1-flash` | 76 | 0 | chat |
| 39 | 2026-10-30T18:30:09 | `s31107496` | `nvidia/nvidia/nemotron-3-ultra-550b-a55b` | 19 | 1 | chat |
| 40 | 2026-10-30T19:10:50 | `s46dac08b` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 40 | 0 | chat |
| 41 | 2026-10-30T19:13:24 | `s31107496` | `nvidia/nvidia/nemotron-3-ultra-550b-a55b` | 79 | 0 | chat |
| 42 | 2026-10-30T19:21:31 | `s46c64d15` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 19 | 0 | chat |
| 43 | 2026-10-30T19:23:23 | `s31107496` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 10 | 0 | chat |
| 44 | 2026-10-30T19:38:31 | `sd15e51d8` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 21 | 0 | chat |
| 45 | 2026-10-30T19:43:51 | `s31107496` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 50 | 0 | chat |
| 46 | 2026-10-30T19:57:06 | `sd15e51d8` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 39 | 1 | chat |
| 47 | 2026-10-30T20:28:12 | `s2d5636cb` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 51 | 2 | chat |
| 48 | 2026-10-30T22:13:44 | `s2d5636cb` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 188 | 0 | chat |
| 49 | 2026-10-30T22:30:25 | `sd15e51d8` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 182 | 0 | chat |
| 50 | 2026-10-31T04:19:28 | `s46dac08b` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 164 | 0 | chat |
| 51 | 2026-10-31T06:35:20 | `s32580172` | `nvidia/minimaxai/minimax-m3.1-flash` | 22 | 1 | chat |
| 52 | 2026-10-31T06:57:26 | `s32580172` | `nvidia/minimaxai/minimax-m3.1-flash` | 24 | 0 | chat |
| 53 | 2026-10-31T09:43:56 | `s32580172` | `nvidia/minimaxai/minimax-m3.1-flash` | 53 | 0 | chat |
| 54 | 2026-10-31T10:37:14 | `s4b11921d` | `nvidia/minimaxai/minimax-m3.1-flash` | 5 | 0 | chat |
| 55 | 2026-10-31T10:57:54 | `s4b11921d` | `nvidia/minimaxai/minimax-m3.1-flash` | 62 | 0 | chat |
| 56 | 2026-10-31T11:03:42 | `s32580172` | `antigravity/gemini-3.8-flash` | 84 | 0 | chat |
| 57 | 2026-10-31T11:08:32 | `s37bbafde` | `nvidia/minimaxai/minimax-m3.1-flash` | 146 | 0 | chat |
| 58 | 2026-10-31T15:24:40 | `sbd9aea3b` | `antigravity/gemini-3.8-flash` | 113 | 0 | chat |
| 59 | 2026-10-31T16:36:15 | `s8dae9fa1` | `antigravity/gemini-3.8-flash` | 17 | 0 | chat |
| 60 | 2026-10-31T17:35:04 | `s8dae9fa1` | `antigravity/gemini-3.8-flash` | 209 | 0 | chat |
| 61 | 2026-10-31T17:40:52 | `s8dae9fa1` | `antigravity/gemini-3.8-flash` | 6 | 0 | chat |
| 62 | 2026-10-31T17:46:52 | `s8dae9fa1` | `antigravity/gemini-3.8-flash` | 24 | 0 | chat |
| 63 | 2026-10-31T18:01:41 | `se45dbf63` | `nvidia/nvidia/nemotron-3-ultra-550b-a55b` | 9 | 0 | chat |
| 64 | 2026-10-31T18:06:06 | `se45dbf63` | `nvidia/nvidia/nemotron-3-ultra-550b-a55b` | 7 | 0 | chat |
| 65 | 2026-10-31T18:07:51 | `s89eb1454` | `tokenharbor/deepseek-v4.1-flash:free` | 36 | 0 | chat |
| 66 | 2026-10-31T18:41:37 | `s89eb1454` | `antigravity/gemini-3.8-flash` | 114 | 0 | chat |
| 67 | 2026-10-31T18:47:59 | `s89eb1454` | `antigravity/gpt-oss-120b` | 2 | 0 | chat |
| 68 | 2026-10-31T18:50:44 | `s89eb1454` | `antigravity/gemini-3.8-flash` | 5 | 0 | chat |
| 69 | 2026-10-31T19:09:44 | `s89eb1454` | `antigravity/gemini-3.8-flash` | 48 | 1 | chat |
| 70 | 2026-11-01T17:11:58 | `s757c607d` | `nvidia/z-ai/glm-5.3-flash` | 17 | 0 | chat |
| 71 | 2026-11-01T18:09:52 | `s757c607d` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 42 | 0 | chat |
| 72 | 2026-11-01T19:58:41 | `s8552818f` | `antigravity/gemini-3.8-flash` | 92 | 0 | chat |
| 73 | 2026-11-01T20:32:09 | `sub_20261101_175041_3` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 26 | 0 | chat |
| 74 | 2026-11-02T05:50:48 | `sac897dbb` | `nvidia/minimaxai/minimax-m3.1-flash` | 112 | 2 | chat |
| 75 | 2026-11-02T06:39:06 | `sb9d614c0` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 3 | 0 | chat |
| 76 | 2026-11-02T07:11:05 | `sb9d614c0` | `nvidia/minimaxai/minimax-m3.1-flash` | 64 | 0 | chat |
| 77 | 2026-11-02T08:54:48 | `s4d81b563` | `antigravity/gemini-3.8-flash` | 72 | 2 | chat |
| 78 | 2026-11-02T12:10:25 | `sb6989a71` | `antigravity/gemini-3.8-flash` | 165 | 0 | chat |
| 79 | 2026-11-02T12:26:33 | `sb6989a71` | `antigravity/gemini-3.8-flash` | 33 | 0 | chat |
| 80 | 2026-11-02T12:31:36 | `s7c079695` | `nvidia/nvidia/nemotron-3.5-lightning-30b-a3b` | 9 | 0 | chat |
| 81 | 2026-11-02T16:32:18 | `sb6989a71` | `antigravity/gemini-3.8-flash` | 125 | 0 | chat |
| 82 | 2026-11-02T16:43:19 | `sb6989a71` | `antigravity/gemini-3.8-flash` | 65 | 0 | chat |
| 83 | 2026-11-02T16:45:08 | `sb40dc9a0` | `nvidia/mistralai/mistral-large-4-1t-a52b` | 11 | 0 | chat |
| 84 | 2026-11-02T16:50:27 | `s2b93d73b` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 1 | 0 | chat |
| 85 | 2026-11-02T16:52:34 | `s91d5d13a` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 56 | 1 | chat |
| 86 | 2026-11-02T16:58:33 | `s3796ed27` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 1 | 0 | exec |
| 87 | 2026-11-02T17:00:01 | `s2b93d73b` | `nvidia/nvidia/nemotron-3.5-lightning-30b-a3b` | 38 | 0 | chat |
| 88 | 2026-11-02T17:02:44 | `sb75d41b7` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 1 | 0 | exec |
| 89 | 2026-11-02T17:02:59 | `s16141feb` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 4 | 0 | exec |
| 90 | 2026-11-02T17:03:19 | `sa9905dec` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 1 | 0 | exec |
| 91 | 2026-11-02T17:04:25 | `s8195b0c7` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 3 | 0 | exec |
| 92 | 2026-11-02T17:04:47 | `s8520eb99` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 2 | 0 | exec |
| 93 | 2026-11-02T17:05:05 | `sffffbc71` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 1 | 0 | exec |
| 94 | 2026-11-02T17:33:30 | `sb6989a71` | `antigravity/gemini-3.8-flash` | 127 | 0 | chat |
| 95 | 2026-11-02T19:16:45 | `s91d5d13a` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 151 | 0 | chat |
| 96 | 2026-11-02T20:33:44 | `sb6989a71` | `antigravity/gemini-3.8-flash` | 94 | 0 | chat |
| 97 | 2026-11-03T04:39:29 | `s8fa05fbd` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 5 | 0 | chat |
| 98 | 2026-11-03T04:59:22 | `sb6989a71` | `antigravity/gemini-3.8-flash` | 7 | 0 | chat |
| 99 | 2026-11-03T05:21:32 | `s704cfa90` | `antigravity/gemini-3.8-flash` | 2 | 0 | exec |
| 100 | 2026-11-03T05:36:22 | `s9b80026a` | `antigravity/gemini-3.8-flash` | 2 | 0 | exec |
| 101 | 2026-11-03T06:55:56 | `s4fc7fe21` | `nvidia/deepseek-ai/deepseek-v4.1-flash` | 48 | 1 | chat |
| 102 | 2026-11-03T07:04:35 | `s6ee42bba` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 10 | 0 | chat |
| 103 | 2026-11-03T07:12:23 | `s6ee42bba` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 15 | 1 | chat |
| 104 | 2026-11-03T07:23:33 | `s94066114` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 83 | 0 | chat |
| 105 | 2026-11-03T08:35:17 | `se1689e45` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 14 | 0 | chat |
| 106 | 2026-11-03T08:47:53 | `s94066114` | `antigravity/gemini-3.8-flash` | 196 | 0 | chat |
| 107 | 2026-11-03T08:49:27 | `sb6989a71` | `antigravity/gemini-3.8-flash` | 179 | 0 | chat |
| 108 | 2026-11-03T09:38:40 | `s94066114` | `antigravity/gemini-3.8-flash` | 18 | 0 | chat |
| 109 | 2026-11-03T09:51:29 | `s94350157` | `antigravity/gemini-3.8-flash` | 36 | 0 | chat |
| 110 | 2026-11-03T12:00:08 | `s94350157` | `nvidia/z-ai/glm-5.3-flash` | 19 | 0 | chat |
| 111 | 2026-11-03T12:03:02 | `s06a267f6` | `antigravity/gemini-3.8-flash` | 15 | 0 | chat |
| 112 | 2026-11-03T12:44:09 | `s06a267f6` | `antigravity/gemini-3.8-flash` | 104 | 0 | chat |
| 113 | 2026-11-03T14:10:31 | `s06a267f6` | `antigravity/gemini-3.8-flash` | 7 | 0 | chat |
| 114 | 2026-11-03T17:53:07 | `s512bbc31` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 8 | 1 | chat |
| 115 | 2026-11-03T17:54:17 | `s512bbc31` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 4 | 0 | chat |
| 116 | 2026-11-03T19:51:10 | `s4729d5ec` | `opencode/flint-40b-free` | 14 | 0 | chat |
| 117 | 2026-11-03T20:23:24 | `s4729d5ec` | `opencode/flint-40b-free` | 47 | 0 | chat |
| 118 | 2026-11-03T22:14:40 | `sff6facd8` | `nvidia/nvidia/nemotron-3-ultra-550b-a55b` | 12 | 0 | chat |
| 119 | 2026-11-03T22:19:43 | `sff6facd8` | `nvidia/nvidia/nemotron-3-ultra-550b-a55b` | 26 | 0 | chat |
| 120 | 2026-11-04T04:33:15 | `s8bfe355d` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 7 | 0 | chat |
| 121 | 2026-11-04T04:51:24 | `s8bfe355d` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 1 | 0 | chat |
| 122 | 2026-11-04T05:41:50 | `s56d9bfc1` | `nvidia/mistralai/mistral-large-4-1t-a52b` | 2 | 0 | chat |
| 123 | 2026-11-04T07:04:36 | `sd7f72bc9` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 5 | 0 | chat |
| 124 | 2026-11-04T07:21:29 | `sd7f72bc9` | `opencode/deepseek-v4.1-flash-free` | 46 | 0 | chat |
| 125 | 2026-11-04T12:07:37 | `sd7f72bc9` | `antigravity/gemini-3.8-flash` | 6 | 3 | chat |
| 126 | 2026-11-04T12:25:16 | `s1bc4e2ec` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 14 | 0 | chat |
| 127 | 2026-11-04T13:28:14 | `s3329dc1a` | `opencode-cli/opencode/deepseek-v4.1-flash-free` | 59 | 0 | chat |
| 128 | 2026-11-04T13:31:02 | `s3329dc1a` | `opencode-cli/opencode/deepseek-v4.1-flash-free` | 16 | 0 | chat |
| 129 | 2026-11-04T13:35:47 | `s418e0b77` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 3 | 0 | chat |
| 130 | 2026-11-04T14:30:10 | `s3329dc1a` | `opencode/deepseek-v4.1-flash-free` | 108 | 0 | chat |
| 131 | 2026-11-04T18:39:17 | `s1020d45b` | `opencode/deepseek-v4.1-flash-free` | 40 | 0 | chat |
| 132 | 2026-11-04T19:12:45 | `s3329dc1a` | `opencode/deepseek-v4.1-flash-free` | 94 | 0 | chat |
| 133 | 2026-11-04T19:39:38 | `s3329dc1a` | `opencode/deepseek-v4.1-flash-free` | 47 | 0 | chat |
| 134 | 2026-11-05T06:48:32 | `s7372de70` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 3 | 0 | chat |
| 135 | 2026-11-05T07:00:09 | `s7372de70` | `antigravity/gemini-3.8-flash` | 33 | 0 | chat |
| 136 | 2026-11-05T07:06:58 | `s7372de70` | `antigravity/gemini-3.8-flash` | 19 | 0 | chat |
| 137 | 2026-11-05T07:24:43 | `saac38806` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 10 | 0 | chat |
| 138 | 2026-11-05T07:32:47 | `saac38806` | `antigravity/gemini-3.8-flash` | 17 | 0 | chat |
| 139 | 2026-11-05T07:44:32 | `saac38806` | `antigravity/gemini-3.8-flash` | 41 | 0 | chat |
| 140 | 2026-11-05T07:44:41 | `s7372de70` | `antigravity/gemini-3.8-flash` | 37 | 0 | chat |
| 141 | 2026-11-05T07:46:27 | `s20758fb7` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 5 | 0 | chat |
| 142 | 2026-11-05T07:53:24 | `s7372de70` | `antigravity/gemini-3.8-flash` | 42 | 0 | chat |
| 143 | 2026-11-05T08:01:22 | `s7372de70` | `antigravity/gemini-3.8-flash` | 25 | 0 | chat |
| 144 | 2026-11-05T08:25:24 | `saac38806` | `antigravity/gemini-3.8-flash` | 86 | 0 | chat |
| 145 | 2026-11-05T08:39:15 | `s7372de70` | `antigravity/gemini-3.8-flash` | 21 | 0 | chat |
| 146 | 2026-11-05T08:59:11 | `saac38806` | `antigravity/gemini-3.8-flash` | 109 | 0 | chat |
| 147 | 2026-11-05T09:08:44 | `s7372de70` | `antigravity/gemini-3.8-flash` | 70 | 0 | chat |
| 148 | 2026-11-05T10:06:01 | `s20758fb7` | `nvidia/minimaxai/minimax-m3.1-flash` | 301 | 0 | chat |
| 149 | 2026-11-05T10:20:58 | `s53f0fbff` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 30 | 0 | chat |
| 150 | 2026-11-05T10:23:17 | `s7372de70` | `antigravity/gemini-3.8-flash` | 7 | 1 | chat |
| 151 | 2026-11-05T10:40:14 | `s20758fb7` | `antigravity/gemini-3.8-flash` | 101 | 0 | chat |
| 152 | 2026-11-05T11:04:01 | `s20758fb7` | `antigravity/gemini-3.8-flash` | 45 | 0 | chat |
| 153 | 2026-11-05T11:18:39 | `saaedfedb` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 34 | 4 | chat |
| 154 | 2026-11-05T11:22:21 | `saaedfedb` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 13 | 0 | chat |
| 155 | 2026-11-05T11:30:25 | `saaedfedb` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 70 | 0 | chat |
| 156 | 2026-11-05T11:35:41 | `saaedfedb` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 18 | 0 | chat |
| 157 | 2026-11-05T11:39:02 | `s20758fb7` | `antigravity/gemini-3.8-flash` | 55 | 0 | chat |
| 158 | 2026-11-05T11:39:48 | `saaedfedb` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 20 | 0 | chat |
| 159 | 2026-11-05T11:52:14 | `s23efe365` | `nvidia/nvidia/nemotron-3.5-super-vl-120b-a12b` | 10 | 1 | chat |
| 160 | 2026-11-05T11:59:21 | `s89dfc4f3` | `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 9 | 0 | exec |
| 161 | 2026-11-05T12:12:47 | `s20758fb7` | `antigravity/gemini-3.8-flash` | 74 | 0 | chat |
| 162 | 2026-11-05T13:06:31 | `s7372de70` | `antigravity/gemini-3.8-flash` | 25 | 0 | chat |
| 163 | 2026-11-05T14:16:55 | `s843199fd` | `nvidia/minimaxai/minimax-m3.1-flash` | 26 | 0 | chat |
| 164 | 2026-11-05T14:28:43 | `s7372de70` | `antigravity/gemini-3.8-flash` | 59 | 0 | chat |
| 165 | 2026-11-05T17:54:44 | `s70a274e5` | `nvidia/z-ai/glm-5.3-flash` | 8 | 0 | chat |
| 166 | 2026-11-05T18:03:26 | `s70a274e5` | `nvidia/z-ai/glm-5.3-flash` | 20 | 1 | chat |

## Findings and fixes

1. **The counter was mislabelled.** `UsageLedger::record_turn` set `ok = false` for any errored tool call and `/usage` called that a failed turn. Fixed: a denied call is now reported only in `denied_calls`, and both usage screens say `N turn(s) hit a tool error` instead of `N failed turn(s)`.

2. **Most errors are normal development noise.** 170 command failures are failing tests, compile errors, wrong working directories and `Text file busy` installs; 64 are timeouts (38 at the old 120 s default). These are the agent doing work, not the harness breaking. Fixed: default `tools.terminal_timeout_secs` raised 120 → 300.

3. **Delegated children ignored `--yes`.** A background subagent always fell back to `NonInteractive`, so `tools.approval = allow` still produced `approval denied: delegated subagent: interactive approval is unavailable in background execution` (19 occurrences). Fixed: the child now inherits the session mode — `allow` → `AllowAll`, `deny` → `DenyAll`, only `ask` fails closed.

4. **Truncated tool calls ran as `{}`.** Both streaming and buffered paths did `serde_json::from_str(&raw).unwrap_or(json!({}))`. A tool call cut off mid-JSON therefore executed with no arguments — the 33 `read_file` → `Missing required argument 'path'` clusters — instead of being retried. Fixed: a non-empty, unparseable argument fragment now fails the model call as an incomplete tool call, which is retryable, and is never dispatched.

5. **Model-side path guessing.** 86 errors are the model asking for a path that does not exist under the session workspace root, usually `crates/...` while the root is `/home/janelle`. This is a prompt/workspace issue rather than a counter bug; the tool result already names the workspace root, and the new report makes it visible instead of hiding it inside "failed turns".

6. **Unknown tool names** (11) are model hallucinations from other harnesses (`shell`, `cd`, `glob`, `list_file`, `desktop_idle`). They are rejected correctly; they are not missing features.


## Reproduce

```bash
# the ledger's own view
sqlite3 ~/.luna/usage.db "select count(*), sum(ok=0), sum(denied_calls) from usage;"

# the errors behind those rows
sqlite3 ~/.luna/sessions.db \
  "select count(*) from messages where tool_result_json like '%\"is_error\":true%';"

# turns that actually halted
rg -n 'Halt triggered' ~/.luna/luna.log
```

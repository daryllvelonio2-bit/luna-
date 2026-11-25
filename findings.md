# Audit findings

What this repo does: Luna is a local CLI/TUI and optional web agent harness. It runs model sessions, filesystem and shell tools, MCP servers, embedded plugins, persistent notes/memory, delegated agents, and scheduled jobs.

Assumed load: one interactive user plus several concurrent model, tool, delegate, and scheduler tasks. The web server may bind to a non-loopback interface; this does not assume a large multi-user deployment.

Snapshot: `1ae5e88`, including the working-tree changes present during the audit. Concurrent fixes were re-read before inclusion; superseded observations are omitted.

Verification: `cargo check -p harness-cli -p harness-web` passed. The selected full/targeted runs passed 1,134 tests with four ignored: core 485, providers 141, CLI 464, plugins 16, notes 13, scheduler 9, MCP 3, and web 3. These tests do not exercise most failure and adversarial paths below.

## 1. [MUST] Privileged web routes do not enforce authentication

- **Problem:** `create_router` mounts session, credential, filesystem-adjacent, profile, plugin, scheduler, and WebSocket handlers without the session-cookie check. `auth_status` also reports `authenticated: true` unconditionally. Any client that can reach the listener can invoke privileged operations; Host/Origin checks are not authentication.
- **Where:** `crates/harness-web/src/routes.rs:63-120`, `crates/harness-web/src/routes.rs:417-437`, `crates/harness-web/src/routes.rs:1539-1697`, `crates/harness-web/src/extra_routes.rs:35-110`, `crates/harness-web/src/plugin_routes.rs:16-39`.
- **Fix:** Apply cookie authentication middleware to all privileged HTTP and WebSocket routes. Leave only the token exchange, static login assets, and an honest auth-status probe public. Return 401/403 before handler execution and verify the cookie in `auth_status`.
- **Repro/test:** The existing web plugin test starts a server and successfully lists plugins and creates/updates schedules without a cookie (`plugin_routes.rs:564-669`). Add tests proving cookie-less API and WebSocket requests fail, invalid cookies fail, and exchanged cookies succeed.

## 2. [MUST] Profile deletion accepts `..` and erases sibling data

- **Problem:** Both deletion paths join an unvalidated route/tool argument to `profiles_dir` and call `remove_dir_all`. With `name = ".."`, the target is `profiles/..`; on this Linux system the operation deleted an unrelated sibling file in a temporary reproduction. Against the default layout, this can erase credentials, sessions, notes, and other data under `~/.luna`.
- **Where:** `crates/harness-core/src/tools/profiles.rs:626-638`, `crates/harness-web/src/extra_routes.rs:1628-1644`.
- **Fix:** Call `profiles::validate_name` before every delete, reject `default`, and enforce that the resolved target is a direct child of `profiles_dir`. Reuse one deletion helper in CLI, tool, and web paths.
- **Repro/test:** A temporary-directory Rust reproduction of `remove_dir_all(profiles.join(".."))` printed `unrelated file still exists: false`. Add non-destructive tempdir tests for `..`, absolute paths, separators, and valid names.

## 3. [MUST] Background shell jobs leak descendants and inherit the sudo password

- **Problem:** Background commands do not create a Unix process group, and `BackgroundTaskQueue::kill` terminates only the direct child on Unix. Grandchildren can survive a reported kill. The shared background command builder also lacks the foreground builder's `SUDO_PASSWORD` removal, exposing the cached password through the child environment.
- **Where:** `crates/harness-core/src/tasks.rs:31-48`, `crates/harness-core/src/tasks.rs:127-149`, compared with `crates/harness-core/src/tools/shell.rs:77-96`.
- **Fix:** Start each background job in its own process group/session and terminate that group on kill and timeout. Remove `SUDO_PASSWORD` in the shared background builder as well; keep the foreground validation and error handling unchanged.
- **Repro/test:** Spawn a shell that starts a grandchild, kill the job, and assert the grandchild exits. Set a sentinel `SUDO_PASSWORD`, spawn a background command that prints its presence, and assert it is absent.

## 4. [MUST] Several provider rebuilds still drop the session credential binding

- **Problem:** An unbound `GenericProviderClient` calls `acquire_credential`, not `acquire_credential_for_session`, so it silently uses global rotation instead of the session pin. The classic `/default_model` provider and its implicit compactor remain unbound; the web worker and background memory saver have the same problem.
- **Where:** `crates/harness-cli/src/commands/chat/commands/models.rs:344-350`, `models.rs:556-569`, `crates/harness-web/src/extra_routes.rs:1173-1181`, `crates/harness-app/src/memory_writer.rs:55-83`, `crates/harness-providers/src/client.rs:209-222`.
- **Fix:** Add `.with_session_id(session_id)` to the two default-model construction sites and worker client. Thread the owning session ID into background-saver construction and bind that client too. Do not change the already-bound `/model`, TUI-picker, and explicit compaction paths.
- **Repro/test:** Configure global account A and session-pinned account B. After each rebuild path, assert credential acquisition selects B and a failure does not fail over to A.

## 5. [MUST] “This session only” priority changes global rotation

- **Problem:** Both classic and TUI `/connect` flows change the provider's persisted global strategy to `FillFirst` after setting a session-only pin. Other sessions therefore stop using their configured RoundRobin/Random/LeastUsed policy, contrary to the selected scope and potentially charging a different account.
- **Where:** `crates/harness-cli/src/commands/chat/connect.rs:638-650`, `crates/harness-cli/src/commands/chat/tui/actions/modals_dispatch.rs:358-377`.
- **Fix:** Remove the global strategy mutation from the session-only branch. The session pin already controls selection and failover; retain any intended strategy change only in the explicit global branch.
- **Repro/test:** Start with global RoundRobin, set a session-only priority, and assert the persisted global strategy remains RoundRobin and a second session continues rotating.

## 6. [MUST] Credential usage persistence bypasses the credential-store lock

- **Problem:** Mutation operations use `acquire_store_lock`, but `persist_state` performs an unlocked read-merge-whole-file-write. A usage write can overlap a locked deletion in another process and write back the deleted credential. Concurrent acquisitions can also lose usage and round-robin updates.
- **Where:** `crates/harness-providers/src/auth.rs:353-375`, `crates/harness-providers/src/auth.rs:457-505`, `crates/harness-providers/src/auth.rs:910-920`.
- **Fix:** Hold the same store lock across `persist_state`'s load, merge, and atomic replacement. Preserve the rule that only rotation counters come from memory and temporary-disable flags are not persisted.
- **Repro/test:** Pause one usage persistence after its read, delete a credential through another manager, resume the write, and assert the deletion survives. Add a concurrent two-manager usage-counter test.

## 7. [SHOULD] Separate subagent batches can reuse the same task and session ID

- **Problem:** IDs consist only of Unix seconds plus the index within one batch. Two separate calls in the same second both starting at index zero produce the same ID, overwriting task/cancellation entries and causing two agents to share one child session and log path.
- **Where:** `crates/harness-core/src/tools/delegate.rs:1239-1269`, `crates/harness-core/src/tools/delegate.rs:1300-1345`.
- **Fix:** Use a UUID, or a process-unique atomic sequence combined with a random/boot prefix, for every child independently of batch index.
- **Repro/test:** Create two separate batches under the same frozen second and assert all task IDs, cancellation entries, log paths, and child session IDs remain distinct.

## 8. [SHOULD] Delegated agents keep the pre-switch model

- **Problem:** `DelegateTaskTool` captures its provider during runtime preparation. Model switching calls `Agent::set_provider` but does not update the delegate tool, so later `delegate_task` calls silently use the old provider/model unless an explicit worker model is supplied.
- **Where:** `crates/harness-app/src/runtime.rs:133-139`, `crates/harness-core/src/agent/mod.rs:431-436`, `crates/harness-cli/src/commands/chat/commands/models.rs:167-196`, `crates/harness-core/src/tools/delegate.rs:59-79`.
- **Fix:** Update the delegate provider in the same shared model-switch operation, or resolve the current provider at spawn time rather than caching it indefinitely.
- **Repro/test:** Switch from model A to B, spawn a delegate without a model override, and assert its provider and session model are B.

## 9. [SHOULD] Manual scheduler runs bypass duplicate-run protection

- **Problem:** `run_schedule_task` spawns execution directly instead of using `SchedulerService::run_now_cancellable`. It does not mark the task in flight, so repeated clicks or a simultaneous due tick can execute the same automation twice and overwrite delivery state.
- **Where:** `crates/harness-web/src/routes.rs:1648-1697`, compared with `crates/harness-scheduler/src/service.rs:82-107` and `service.rs:165-182`.
- **Fix:** Route manual execution through the scheduler's in-flight/cancellation path and return 409 when already running. Record exactly one receipt for the accepted execution.
- **Repro/test:** Hold the delivery handler open, request two manual runs and a due tick, and assert one execution, one active cancellation token, and one receipt.

## 10. [SHOULD] Profile “copy notes/memories” reads retired storage

- **Problem:** Profile creation copies the legacy SQLite `memory_db` and only flat Markdown files directly under `notes_dir`. Current defaults store memories in `memory_dir` and notes in project subdirectories, so `same`/`all` can report success while producing an empty or incomplete profile.
- **Where:** `crates/harness-core/src/tools/profiles.rs:173-315`, `crates/harness-core/src/tools/profiles.rs:437-560`, `crates/harness-cli/src/commands/profile.rs:169-188`, `crates/harness-core/src/config/types.rs:400-417`.
- **Fix:** Copy the active Markdown memory store and the complete notes project tree, or export/import through their engine APIs. Keep `fresh` empty and selected-ID filtering intact.
- **Repro/test:** Create default Markdown memories plus root and project-scoped notes, copy a profile with `same`, reopen its engines, and assert all selected records are readable. Existing tests use only the legacy SQLite fixture and miss this.

## 11. [SHOULD] Renaming a note can silently delete another note

- **Problem:** When the new title's slug already exists, `update_note` deletes the destination file and then the old file. Renaming note A to the title of existing note B destroys B without an explicit overwrite option or confirmation.
- **Where:** `crates/harness-notes/src/file_engine.rs:435-469`.
- **Fix:** Reject a title collision unless the destination is the same file. If overwrite is ever offered, make it explicit and atomic; do not encode overwrite into an ordinary rename.
- **Repro/test:** Save A and B, rename A to B's title, and assert the operation fails with both bodies unchanged. Also test title-only case changes on case-insensitive filesystems.

## 12. [SHOULD] One non-terminating plugin call can permanently wedge all plugins

- **Problem:** Plugin JavaScript runs on the single QuickJS actor thread without a deadline, instruction budget, or interrupt handler. `while (true) {}` never yields to the async timeout layer, so later plugin calls queue behind it indefinitely.
- **Where:** `crates/harness-plugins/src/runtime.rs:166-190`, `crates/harness-plugins/src/manager.rs:163-182`.
- **Fix:** Enforce a QuickJS interrupt/deadline or isolate execution in a terminable worker with memory and time limits. Return a bounded tool error and leave the actor usable.
- **Repro/test:** Invoke a non-terminating plugin in a supervised test process, assert a bounded error, then invoke a healthy plugin successfully. Do not run this repro in the main agent process.

## 13. [SHOULD] Provider timeout values below ten minutes are ignored

- **Problem:** `build_http_client` uses `timeout_secs.max(600)`, turning values such as 10, 30, or 120 into a 600-second whole-request timeout. Stalled providers can occupy sessions and scheduler tasks far longer than configured.
- **Where:** `crates/harness-providers/src/client.rs:28-33`.
- **Fix:** Honor the configured value with only a positive lower bound such as `max(1)`. If a special long-stream path is required, configure that path explicitly rather than overriding every caller.
- **Repro/test:** Point a client with a one-second timeout at a local server that accepts but does not respond, and assert completion near one second rather than 600.

## 14. [SHOULD] Session-priority persistence failures are reported as success

- **Problem:** Web and CLI paths discard `set_credential_priority` and `clear_credential_priority` errors. The web route updates memory and returns 200 even when SQLite rejects the session or write, so the displayed pin can disappear after restart or diverge from durable state.
- **Where:** `crates/harness-web/src/extra_routes.rs:328-335`, `extra_routes.rs:360-365`, `crates/harness-cli/src/commands/chat/connect.rs:622-624`, `connect.rs:638-643`, `crates/harness-cli/src/commands/chat/tui/actions/modals_dispatch.rs:366-369`.
- **Fix:** Persist first and update memory only after success, or roll back memory on failure. Return an HTTP error and show a CLI/TUI error instead of a success message.
- **Repro/test:** Set a web session priority for a nonexistent session ID; SQLite returns `session '<id>' not found`, while the route currently returns 200. Assert the corrected route returns an error and creates no in-memory pin.

## 15. [SHOULD] A partial legacy-note import still retires the source database

- **Problem:** Individual note write failures are logged but do not make the import incomplete. The legacy database is renamed to `.imported` afterward, preventing automatic retry and leaving only a manual backup of the skipped notes.
- **Where:** `crates/harness-notes/src/file_engine.rs:180-221`.
- **Fix:** Track any failed note and do not retire the source unless every row was imported or explicitly skipped as a duplicate. Surface a partial-import error with recoverable counts.
- **Repro/test:** Inject one destination write failure into a two-row import and assert the original database remains active and the next open retries the missing row without duplicating the successful one.

## Recommended order

Fix 1-3 before exposing the web server or running unattended shell work. Fix 4-6 next because they select or restore the wrong credential. Then address the deterministic ID, scheduler, and note-data cases (7, 9, 11, 15), followed by the remaining reliability issues. Each fix should land with its focused regression test; no new dependency or subsystem redesign is required.

## Resolution status

All fifteen findings are closed or explained, in the recommended order.

Verification on this tree: `cargo check --workspace --all-targets` clean, `cargo clippy --workspace --all-targets` silent, `cargo test --workspace` → **1,554 passed, 0 failed**.

| # | Sev | Status | Change | Regression test |
|---|-----|--------|--------|-----------------|
| 1 | MUST | fixed | `require_api_auth` layer wraps every `/api/*` route (HTTP and WebSocket); only the launch-token exchange, static assets and `auth_status` stay public, and `auth_status` now reports the real cookie state | `plugin_routes::live_http_plugin_ui_tools_commands_panels_and_settings` exchanges a cookie first |
| 2 | MUST | fixed | `profiles::resolve_custom_profile_dir` validates the name and proves the target is a direct child of `profiles_dir`; used by the tool, the web route and the CLI's shared helper | `profiles::delete_target_stays_inside_profiles_dir` |
| 3 | MUST | fixed | background jobs lead their own process group, `kill` signals that group on Unix, and `SUDO_PASSWORD` is removed from the child env | `tasks::a_killed_job_takes_its_grandchildren_with_it`, `tasks::a_background_job_does_not_inherit_the_sudo_password` |
| 4 | MUST | fixed | `.with_session_id(..)` on the `/default_model` client, its implicit compactor, the web worker client and the background saver | existing auth/rotation suites; provider 143 |
| 5 | MUST | fixed | the session-only priority branch no longer rewrites the global rotation strategy; only the global branch does | branch removed; strategy is untouched by the session path |
| 6 | MUST | fixed | `persist_state` takes the same credential-store lock as every mutation for the whole read-merge-write | `auth::persist_state_waits_for_the_store_lock` |
| 7 | SHOULD | fixed | `new_child_session_id()` = timestamp + pid + process-wide counter, so one second and batch index no longer collide | `delegate::tests::child_session_ids_are_unique_across_batches_within_one_second` |
| 8 | SHOULD | fixed | `DelegateTaskTool::set_provider`, called at every live provider swap: `/model`, `/default_model`, the TUI picker, the web model route, and session restore | `delegate::tests::children_run_on_the_provider_the_tool_was_rebound_to` |
| 9 | SHOULD | fixed | `/api/schedules/{id}/run` answers 409 while in flight and runs through `run_now_cancellable`, so one accepted run records one receipt and registers a cancellation token | `harness-scheduler/tests/manual_run_guard_test.rs` |
| 10 | SHOULD | fixed | `ProfilePaths.memory_dir` (so a profile owns its active store) and `copy_markdown_files` copying notes and memories as trees with relative paths, in the tool and the CLI | `profile_create_copies_project_notes_and_markdown_memories` |
| 11 | SHOULD | not reproducible | `update_note` keeps the note's id, so a title collision rewrites one file and never touches another; the old delete-then-write path is gone from this engine | `test_renaming_a_note_does_not_destroy_the_note_it_names` |
| 12 | SHOULD | fixed | a QuickJS interrupt handler with a 5s per-call deadline and a 15s startup deadline, covering tool, command and event calls, `Dispose`, `apply(ctx)`, module evaluation and its async settle | `runtime::deadline_tests::a_non_terminating_call_is_interrupted_and_the_runtime_survives` |
| 13 | SHOULD | fixed | `providers.<id>.timeout_secs` is honoured; only `0` falls back to the catalog default of 600s | `harness-providers/tests/http_timeout_test.rs` |
| 14 | SHOULD | fixed | session priority persists first and returns 404/500 on failure; the CLI and TUI show the error and do not install an in-memory pin | web route returns the store's error instead of 200 |
| 15 | SHOULD | fixed | the legacy note store is retired only when every row imported; a failure keeps `notes.db` so the next start retries just the missing rows | `test_partial_legacy_import_keeps_the_source_database` |

### Consequences and remaining gaps

- **#10 changes profile isolation.** A custom profile now owns `paths.memory_dir`, the same way it already owned notes, sessions and credentials. Memories copied at creation are visible immediately; a profile created before this change starts with an empty Markdown memory store until `memories: "same"` is applied again.
- **#12 now covers startup as well.** The `apply(ctx)` handshake, the module evaluation and its async settle all run under a 15-second budget, so a plugin whose top-level code never returns fails its own load instead of hanging `PluginRuntimeHandle::spawn`. A call keeps the shorter 5-second budget.
- **#8 now covers the session-restore path.** `session_ops::restore_session_model` rebinds the delegate tool and binds the restored client to the session it was restored for, matching every other live provider swap.

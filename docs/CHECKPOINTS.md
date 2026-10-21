# Checkpoints and rewind

Every turn that writes a file first snapshots the file. `/rewind 3` puts those files back the way
they were when turn 3 started. It is the answer to "the edit was allowed and it was wrong".

## What gets snapshotted

The registry middleware copies a file immediately before a path-mutating tool touches it:

| tool | captured |
|---|---|
| `write_file` | yes — the file's bytes at the start of the turn, or "did not exist" |
| `patch` | yes — same |
| `read_file`, `search_files`, `list_files` | no — they do not write |
| `terminal` | **no** — a shell command's targets are not knowable from its argument string |

The `terminal` gap is real and documented rather than hidden: `git checkout`, `sed -i` and
`cargo fmt` write files the harness never sees. If you need undo across shell commands, keep the
work inside the file tools or use version control.

Each path is captured **once per turn**, before the first write — not before each call — so a turn
that writes the same file five times still restores to the state at the start of the turn.

## Use

```
/rewind          # list this session's checkpoints with a per-turn stat line (+1 ~2 -0)
/rewind 3        # restore the files of turn 3
/diff            # what the newest turn actually changed, hunks and all
/diff 3          # the same for turn 3
/diff --all      # every turn of this session

luna diff                          # the same review, outside the REPL
luna diff --turn 3 --stat          # counts only
luna diff --json | jq '.turns[].summary'
```

## Reviewing before you undo: `diff`

`/rewind` is a write; `diff` is the read that belongs before it, and it reads only — asking what a
turn did is safe even for the turn you are about to undo.

A review compares the turn's snapshot against the files as they are **now**, per file:

| kind | meaning |
|---|---|
| `created` | did not exist when the turn started, exists now |
| `modified` | bytes differ |
| `deleted` | existed then, gone now |
| `unchanged` | recorded by the turn but byte-identical — a no-op write, or already rewound |

The kind comes from the bytes, not from the manifest: a turn that wrote a file back to exactly what
it was reads as `unchanged`, so a rewind never looks more destructive than it is. Binary content is
reported as a kind plus a note rather than as mojibake, and non-UTF-8 files never fail the review.

Diffs are unified (`--- a/path`, `+++ b/path`, `@@` hunks) with per-file `+N -M` counts and a
per-turn summary (`+1 ~2 -0 ·3`: created, modified, deleted, unchanged). `luna diff` prints all
hunks; `/diff` in the REPL caps them at 40 lines per file and says so instead of scrolling the
conversation away.

Restoring is files-only: the transcript is untouched. A rewind puts the workspace back so the
conversation can continue from a known state; it does not un-say what was said.

## Where it lives

```
~/.luna/checkpoints/<session_id>/turn_0003/
  manifest.json    { seq, created_at, prompt, files: [{ path, existed, blob }] }
  blobs/0001       the file's bytes before the turn touched it
```

Plain files, no database, written **during** the turn rather than at the end — a turn that panics
or is killed is still restorable. `turn_0003/manifest.json` is readable with `cat`, which is the
point: an undo that cannot be inspected is an undo you have to trust.

Restoring only touches paths inside `paths.workspace_root`; a manifest that names anything else is
skipped and reported, so a doctored manifest cannot write outside the workspace.

## Configuration

```yaml
checkpoints:
  enabled: true      # default: on — undo is a safety net, not an experiment
  keep_turns: 50     # newest N turns per session; older ones are pruned

paths:
  checkpoints_dir: ~/.luna/checkpoints
```

`luna config set checkpoints.enabled false` turns the whole thing off for a machine that would
rather not spend the disk.

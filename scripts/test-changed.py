#!/usr/bin/env python3
"""Run only the tests affected by the current diff.

The default is the working tree against `HEAD` (staged, unstaged, and untracked
files). Pass paths to plan from those files instead, or `--base <rev>` to diff
against another revision.

Mapping rules:

  crates/<crate>/tests/<stem>.rs       -> cargo test -p <crate> --test <stem>
  crates/<crate>/src/<module>.rs       -> cargo test -p <crate> --lib <module::path>
                                          (or --bins for the bin-only harness-cli)
  crates/<crate>/src/bin/<name>.rs     -> cargo test -p <crate> --bin <name>
  crates/<crate>/Cargo.toml, build.rs  -> cargo test -p <crate>
  crates/<crate>/tests/<subdir>/...    -> cargo test -p <crate>   (shared fixture/helper)
  root Cargo.toml/Cargo.lock/.cargo/   -> cargo test --workspace
  docs, scripts, *.md                  -> no Rust tests

A changed library crate also runs `cargo check -p harness-cli --bin luna` first,
because the CLI depends on nearly every crate and is the cheapest way to catch a
dependent that no longer compiles. `--no-check` skips that gate.

Use `--full` (or `--workspace`) for the old `cargo test --workspace` behaviour;
that is the right call before pushing a change that spans crates.

Examples:

  scripts/test-changed.sh
  scripts/test-changed.sh --dry-run
  scripts/test-changed.sh crates/harness-core/src/tools
  scripts/test-changed.sh --base main
  scripts/test-changed.sh --full
"""

from __future__ import annotations

import argparse
import os
import shlex
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

# Files at the repository root that can change the whole dependency graph.
WORKSPACE_FILES = {"Cargo.toml", "Cargo.lock"}
WORKSPACE_PREFIXES = (".cargo/", "rust-toolchain")
# Trees that never carry Rust tests.
DOC_PREFIXES = ("docs/", ".harness/", ".github/", "scripts/")
DOC_SUFFIXES = (".md", ".txt")


@dataclass
class CratePlan:
    """What to run for one crate."""

    whole: bool = False
    integration: set[str] = field(default_factory=set)
    lib_filters: set[str] = field(default_factory=set)
    bin_filters: set[str] = field(default_factory=set)
    bins: set[str] = field(default_factory=set)


@dataclass
class Plan:
    crates: dict[str, CratePlan] = field(default_factory=dict)
    full_workspace: bool = False
    skipped: list[str] = field(default_factory=list)


def repo_root() -> Path:
    here = Path(__file__).resolve().parent.parent
    try:
        out = subprocess.run(
            ["git", "-C", str(here), "rev-parse", "--show-toplevel"],
            check=True,
            capture_output=True,
            text=True,
        )
        return Path(out.stdout.strip())
    except (OSError, subprocess.CalledProcessError):
        return here


def normalize(path: str, root: Path) -> str:
    candidate = Path(path)
    if candidate.is_absolute():
        try:
            candidate = candidate.relative_to(root)
        except ValueError:
            return candidate.as_posix()
    text = candidate.as_posix()
    while text.startswith("./"):
        text = text[2:]
    return text


def changed_files(root: Path, base: str, paths: list[str]) -> list[str]:
    if paths:
        expanded: list[str] = []
        for raw in paths:
            candidate = Path(raw)
            if not candidate.is_absolute():
                candidate = Path.cwd() / candidate
            candidate = candidate.resolve()
            if candidate.is_dir():
                expanded.extend(
                    normalize(str(p), root)
                    for p in candidate.rglob("*")
                    if p.is_file()
                )
            else:
                expanded.append(normalize(str(candidate), root))
        return sorted(set(expanded))

    tracked = subprocess.run(
        ["git", "-C", str(root), "diff", "--name-only", base],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    untracked = subprocess.run(
        ["git", "-C", str(root), "ls-files", "--others", "--exclude-standard"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    return sorted({line for line in tracked + untracked if line})


def integration_stems(crate: str, root: Path) -> list[str]:
    tests_dir = root / "crates" / crate / "tests"
    if not tests_dir.is_dir():
        return []
    return sorted(path.stem for path in tests_dir.glob("*.rs"))


def plan_src(crate: str, rest: str, root: Path, crate_plan: CratePlan) -> None:
    """Map one `crates/<crate>/src/...` file onto a unit-test target."""
    if not rest.endswith(".rs"):
        crate_plan.whole = True
        return
    # lib.rs / main.rs wire the whole target together; run all of its unit tests.
    if rest in ("lib.rs", "main.rs"):
        crate_plan.whole = True
        return
    if rest.startswith("bin/"):
        sub = rest[len("bin/") :]
        if sub.endswith("/main.rs"):
            name = sub[: -len("/main.rs")]
        else:
            name = sub[:-3]
        crate_plan.bins.add(name)
        return

    module = rest[: -len("/mod.rs")] if rest.endswith("/mod.rs") else rest[:-3]
    test_filter = module.replace("/", "::")

    # harness-cli is bin-only (`luna`); every other crate is a library.
    if (root / "crates" / crate / "src" / "lib.rs").is_file():
        crate_plan.lib_filters.add(test_filter)
    else:
        crate_plan.bin_filters.add(test_filter)

    # A module change often has a matching integration test (`shell` ->
    # `fs_shell_tools_test`). Match whole underscore-delimited words so `ui`
    # does not drag in every `*_test` file.
    leaf = test_filter.rsplit("::", 1)[-1]
    for stem in integration_stems(crate, root):
        if leaf in stem.split("_"):
            crate_plan.integration.add(stem)


def build_plan(files: list[str], root: Path) -> Plan:
    plan = Plan()
    for path in files:
        parts = Path(path).parts
        if not parts:
            continue

        if parts[0] != "crates":
            if path in WORKSPACE_FILES or path.startswith(WORKSPACE_PREFIXES):
                plan.full_workspace = True
            elif path.endswith(DOC_SUFFIXES) or path.startswith(DOC_PREFIXES):
                plan.skipped.append(path)
            else:
                plan.skipped.append(path)
            continue

        if len(parts) < 3:
            plan.skipped.append(path)
            continue

        crate = parts[1]
        rest = "/".join(parts[2:])
        if not (root / "crates" / crate).is_dir():
            # The crate itself was deleted; the root manifest change already
            # selects the workspace suite.
            plan.skipped.append(path)
            continue
        crate_plan = plan.crates.setdefault(crate, CratePlan())

        if rest in ("Cargo.toml", "build.rs"):
            crate_plan.whole = True
            continue
        if rest.startswith(("benches/", "examples/", "tests/fixtures/")):
            crate_plan.whole = True
            continue
        if rest.startswith("tests/"):
            relative = rest[len("tests/") :]
            if "/" not in relative and relative.endswith(".rs"):
                if (root / path).is_file():
                    crate_plan.integration.add(relative[:-3])
                else:
                    plan.skipped.append(path)
            else:
                # Shared helper or fixture: any test in the crate may use it.
                crate_plan.whole = True
            continue
        if rest.startswith("src/"):
            plan_src(crate, rest[len("src/") :], root, crate_plan)
            continue
        if rest.endswith(DOC_SUFFIXES):
            plan.skipped.append(path)
            continue
        # Assets, snapshots, and anything else: the whole crate is the safe scope.
        crate_plan.whole = True
    plan.crates = {
        crate: crate_plan
        for crate, crate_plan in plan.crates.items()
        if crate_plan.whole
        or crate_plan.integration
        or crate_plan.lib_filters
        or crate_plan.bin_filters
        or crate_plan.bins
    }
    return plan


def dedupe_filters(filters: set[str]) -> list[str]:
    """Drop filters already covered by a broader one (`a::b` covers `a::b::c`)."""
    if "" in filters:
        return [""]
    kept: list[str] = []
    for current in sorted(filters, key=lambda value: (value.count("::"), len(value), value)):
        if any(current == broad or current.startswith(broad + "::") for broad in kept):
            continue
        kept.append(current)
    return kept


def commands_for(plan: Plan, no_check: bool) -> list[list[str]]:
    if plan.full_workspace:
        return [["cargo", "test", "--workspace"]]

    commands: list[list[str]] = []

    # The CLI is the workspace's integration point. A change in any library
    # crate is checked through it before the filtered tests run.
    changed = set(plan.crates)
    if changed and not no_check and any(crate != "harness-cli" for crate in changed):
        commands.append(["cargo", "check", "-p", "harness-cli", "--bin", "luna"])

    for crate in sorted(plan.crates):
        crate_plan = plan.crates[crate]
        if crate_plan.whole:
            commands.append(["cargo", "test", "-p", crate])
            continue

        if crate_plan.integration:
            command = ["cargo", "test", "-p", crate]
            for stem in sorted(crate_plan.integration):
                command.extend(["--test", stem])
            commands.append(command)

        for test_filter in dedupe_filters(crate_plan.lib_filters):
            command = ["cargo", "test", "-p", crate, "--lib"]
            if test_filter:
                command.append(test_filter)
            commands.append(command)

        for test_filter in dedupe_filters(crate_plan.bin_filters):
            command = ["cargo", "test", "-p", crate, "--bins"]
            if test_filter:
                command.append(test_filter)
            commands.append(command)

        for name in sorted(crate_plan.bins):
            commands.append(["cargo", "test", "-p", crate, "--bin", name])

    return commands


def describe(plan: Plan) -> None:
    if plan.full_workspace:
        print("Workspace-level change detected: running the full workspace suite.")
    if plan.skipped:
        print(f"Skipping {len(plan.skipped)} non-Rust file(s):")
        for path in plan.skipped:
            print(f"  {path}")
    if not plan.crates and not plan.full_workspace:
        print("No Rust changes; nothing to test.")
        return
    if plan.crates:
        print("Crates in scope:")
        for crate in sorted(plan.crates):
            crate_plan = plan.crates[crate]
            if crate_plan.whole:
                print(f"  {crate}: all tests (manifest, build script, lib/main, or shared fixture)")
                continue
            bits: list[str] = []
            if crate_plan.integration:
                bits.append("tests: " + ", ".join(sorted(crate_plan.integration)))
            if crate_plan.lib_filters:
                bits.append("lib: " + ", ".join(dedupe_filters(crate_plan.lib_filters)))
            if crate_plan.bin_filters:
                bits.append("bins: " + ", ".join(dedupe_filters(crate_plan.bin_filters)))
            if crate_plan.bins:
                bits.append("bin: " + ", ".join(sorted(crate_plan.bins)))
            print(f"  {crate}: " + ("; ".join(bits) if bits else "nothing"))


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Run only the tests affected by the current diff.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument(
        "paths",
        nargs="*",
        help="Optional files or directories to plan from instead of the git diff.",
    )
    parser.add_argument(
        "--base",
        default="HEAD",
        help="Git revision to diff against (default: HEAD).",
    )
    parser.add_argument(
        "--full",
        "--workspace",
        action="store_true",
        dest="full",
        help="Run the full `cargo test --workspace` suite.",
    )
    parser.add_argument(
        "--no-check",
        action="store_true",
        help="Skip the `cargo check -p harness-cli --bin luna` dependent-compile gate.",
    )
    parser.add_argument(
        "--keep-going",
        action="store_true",
        help="Run every command even after one fails.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the plan and commands without running them.",
    )
    args = parser.parse_args(argv)

    root = repo_root()
    if args.full:
        plan = Plan(full_workspace=True)
    else:
        files = changed_files(root, args.base, args.paths)
        if not files:
            print("No changed files; nothing to test.")
            return 0
        plan = build_plan(files, root)

    describe(plan)
    commands = commands_for(plan, args.no_check)
    if not commands:
        return 0

    print("\nCommands:")
    for index, command in enumerate(commands, 1):
        print(f"  {index}. {shlex.join(command)}")

    if args.dry_run:
        return 0

    env = os.environ.copy()
    env.setdefault("CARGO_BUILD_JOBS", "2")

    failures: list[list[str]] = []
    for index, command in enumerate(commands, 1):
        print(f"\n[{index}/{len(commands)}] {shlex.join(command)}", flush=True)
        result = subprocess.run(command, cwd=root, env=env)
        if result.returncode != 0:
            failures.append(command)
            print(f"FAILED: {shlex.join(command)}", file=sys.stderr)
            if not args.keep_going:
                break

    if failures:
        print(f"\n{len(failures)} command(s) failed:", file=sys.stderr)
        for command in failures:
            print(f"  {shlex.join(command)}", file=sys.stderr)
        return 1

    print(f"\nAll {len(commands)} command(s) passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))

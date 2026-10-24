#!/usr/bin/env python3
"""Enforce a line-coverage floor on a `cargo llvm-cov --json` report.

    cargo llvm-cov --all-targets --all-features --json --output-path cov.json
    scripts/coverage-gate.py cov.json

Only the codebase under test counts: files under the configured roots
(`src/` by default). Build output (`target/`), dependencies, and other
generated or third-party paths are ignored and listed, never folded into the
total. The allowlist (coverage-allowlist.json) lists files inside the roots
that cannot execute on a given runner, each with a reason; they are excluded
from both the per-file check and the total. Use `--mode report` to print the
numbers without failing.
"""

from __future__ import annotations

import argparse
import json
import platform
import sys
from pathlib import Path

DEFAULT_ALLOWLIST = "coverage-allowlist.json"


def host_os() -> str:
    system = platform.system()
    if system == "Darwin":
        return "macos"
    if system == "Windows":
        return "windows"
    return "linux"


def load_config(path: Path) -> dict:
    if not path.is_file():
        return {"threshold": 90.0, "per_file_threshold": 90.0, "allow": []}
    return json.loads(path.read_text(encoding="utf-8"))


def allowlisted(config: dict, target_os: str) -> dict[str, str]:
    out: dict[str, str] = {}
    for entry in config.get("allow", []):
        systems = entry.get("os")
        if systems and target_os not in systems:
            continue
        out[entry["path"]] = entry.get("reason", "no reason recorded")
    return out


def repo_relative(filename: str, root: Path) -> str | None:
    """Workspace-relative path, or `None` for anything outside the workspace.

    There is no basename fallback: a dependency or a build-output path must
    never be folded into the total under a bare file name.
    """
    try:
        relative = Path(filename).resolve().relative_to(root.resolve())
    except ValueError:
        return None
    # Windows spells the separator `\`; the roots are written POSIX-style.
    return relative.as_posix()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", help="cargo llvm-cov --json output")
    parser.add_argument(
        "--allowlist", default=DEFAULT_ALLOWLIST, help="allowlist JSON path"
    )
    parser.add_argument("--os", default=host_os(), choices=["macos", "linux", "windows"])
    parser.add_argument("--mode", choices=["enforce", "report"], default="enforce")
    parser.add_argument("--threshold", type=float, default=None)
    parser.add_argument(
        "--per-file",
        type=float,
        default=None,
        help="per-file floor; 0 disables the per-file check",
    )
    parser.add_argument("--worst", type=int, default=15)
    args = parser.parse_args()

    root = Path.cwd()
    config = load_config(root / args.allowlist)
    threshold = args.threshold if args.threshold is not None else config.get("threshold", 90.0)
    target = config.get("target", threshold)
    per_file = (
        args.per_file
        if args.per_file is not None
        else config.get("per_file_threshold", 90.0)
    )
    allowed = allowlisted(config, args.os)
    roots = tuple(config.get("roots", ["src"]))

    data = json.loads(Path(args.report).read_text(encoding="utf-8"))
    exported = data["data"][0]
    rows = []
    total_count = 0
    total_covered = 0
    skipped = []
    ignored = []
    for entry in exported["files"]:
        name = repo_relative(entry["filename"], root)
        lines = entry["summary"]["lines"]
        if name is None:
            ignored.append((entry["filename"], lines["count"], "outside the workspace"))
            continue
        if not name.startswith(tuple(f"{root_dir}/" for root_dir in roots)):
            ignored.append((name, lines["count"], "outside the measured roots"))
            continue
        if name in allowed:
            skipped.append((name, lines["percent"], lines["count"]))
            continue
        total_count += lines["count"]
        total_covered += lines["covered"]
        rows.append((lines["percent"], lines["covered"], lines["count"], name))

    if total_count == 0:
        print("no measurable lines in the report", file=sys.stderr)
        return 1

    percent = 100.0 * total_covered / total_count
    failures: list[str] = []
    print(
        f"measured line coverage ({args.os}): {total_covered}/{total_count} = {percent:.2f}%"
        f"  floor {threshold:.1f}%"
    )
    if target > threshold:
        gap = target / 100.0 * total_count - total_covered
        state = "reached" if gap <= 0 else f"{gap:.0f} lines to go"
        print(f"target {target:.1f}%: {state}")
    if percent + 1e-9 < threshold:
        failures.append(
            f"total {percent:.2f}% is below the {threshold:.1f}% floor "
            f"({threshold / 100 * total_count - total_covered:.0f} more lines)"
        )

    if per_file > 0:
        for file_percent, covered, count, name in sorted(rows):
            if file_percent + 1e-9 < per_file:
                failures.append(f"{name}: {file_percent:.1f}% ({covered}/{count})")

    rows.sort()
    if rows:
        print(f"\nworst {min(args.worst, len(rows))} measured files:")
        for file_percent, covered, count, name in rows[: args.worst]:
            print(f"  {file_percent:6.1f}%  {covered:5d}/{count:<5d}  {name}")
    if skipped:
        print(f"\nexcluded by {args.allowlist} for {args.os}: {len(skipped)} files")
        for name, file_percent, count in sorted(skipped):
            print(f"  {file_percent:6.1f}%  {count:<5d} lines  {name}")
    if ignored:
        print(
            f"\nignored (not the codebase under test; only {', '.join(roots)}/ counts):"
        )
        for name, count, why in sorted(ignored, key=lambda item: item[0]):
            print(f"  {count:<5d} lines  {name}  ({why})")

    if failures and args.mode == "enforce":
        print("\ncoverage gate failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    if failures:
        print("\ncoverage is below the floor (report mode, not failing)")
        for failure in failures:
            print(f"  - {failure}")
    else:
        print("\ncoverage gate passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

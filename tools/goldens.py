#!/usr/bin/env python3
"""Review, freeze, and integrity-check corpus source cases and candidate goldens.

Commands:
    python3 tools/goldens.py status
    python3 tools/goldens.py freeze GROUP --reviewer NAME [--scope TEXT]
    python3 tools/goldens.py unfreeze GROUP --reason TEXT
    python3 tools/goldens.py check [--require-all-frozen] [--require-phase2-frozen]
    python3 tools/goldens.py frozen-files   # machine interface for pgcorpus bless

A frozen group binds the exact .peg source cases to their .tokens and .filtered
outputs. The manifest is parsed and validated strictly. The `frozen-files`
command emits protected golden basenames only after the entire manifest and all
frozen source/golden hashes have passed validation. Consumers must fail closed.
"""
from __future__ import annotations

import datetime
import hashlib
import json
import os
import re
import sys
import tempfile
import tomllib
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
GOLDENS = ROOT / "tests" / "grammar" / "goldens"
CANDIDATES = GOLDENS / "candidates"
CASES = ROOT / "tests" / "grammar" / "cases"
REVIEW = GOLDENS / "REVIEW.toml"
# Review groups by milestone. `check --require-all-frozen` is the Phase 1 gate
# and requires exactly PHASE1_GROUPS, so adding Phase 2 groups never changes what
# Gate 2 means; `--require-phase2-frozen` is the separate parser-milestone gate.
PHASE1_GROUPS = ["phase0", "lexical", "newline", "precedence", "ambiguity", "items", "recovery"]
PHASE2_GROUPS = ["parser"]
GROUPS = PHASE1_GROUPS + PHASE2_GROUPS
HEX64 = re.compile(r"^[0-9a-f]{64}$")


class ManifestError(ValueError):
    """The review manifest is malformed or internally inconsistent."""


def source_files(group: str) -> list[str]:
    directory = CASES / group
    if not directory.is_dir():
        raise ManifestError(f"missing source-case directory for group {group}: {directory}")
    return sorted(p.name for p in directory.iterdir() if p.is_file() and p.suffix == ".peg")


def case_ids(group: str) -> list[str]:
    names = source_files(group)
    ids = [name.split("_", 1)[0].removesuffix(".peg") for name in names]
    counts: dict[str, int] = {}
    for case_id in ids:
        counts[case_id] = counts.get(case_id, 0) + 1
    duplicates = sorted(case_id for case_id, count in counts.items() if count > 1)
    if duplicates:
        raise ManifestError(f"{group}: duplicate case ID(s): {', '.join(duplicates)}")
    return ids


def golden_files(group: str) -> list[str]:
    return [f"{case_id}.{ext}" for case_id in case_ids(group) for ext in ("tokens", "filtered")]


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def default_data() -> dict[str, Any]:
    return {"groups": {g: {"status": "unreviewed"} for g in GROUPS}, "history": []}


def _require_string(table: dict[str, Any], key: str, context: str, *, nonempty: bool = False) -> str:
    value = table.get(key)
    if not isinstance(value, str) or (nonempty and not value.strip()):
        suffix = " (non-empty)" if nonempty else ""
        raise ManifestError(f"{context}: {key} must be a string{suffix}")
    return value


def _validate_hash_table(value: Any, context: str, *, suffixes: tuple[str, ...]) -> dict[str, str]:
    """Validate manifest-owned hash records without consulting live file inventory.

    File additions/removals are repository drift, not manifest syntax errors. They
    are reported by ``frozen_problems`` so that ``unfreeze`` remains available as
    the recovery path after a legitimate source-case rename or addition.
    """
    if not isinstance(value, dict):
        raise ManifestError(f"{context}: missing hash table")
    hashes: dict[str, str] = {}
    for name, digest in value.items():
        if (
            not isinstance(name, str)
            or Path(name).name != name
            or "/" in name
            or "\\" in name
            or name in ("", ".", "..")
            or not name.endswith(suffixes)
        ):
            raise ManifestError(f"{context}: unsafe or unsupported file name {name!r}")
        if not isinstance(digest, str) or not HEX64.fullmatch(digest):
            raise ManifestError(f"{context}: {name!r} must have a lowercase SHA-256 digest")
        hashes[name] = digest
    return hashes


def _case_id_from_source_name(name: str, context: str) -> str:
    if not name.endswith(".peg"):
        raise ManifestError(f"{context}: source hash entry {name!r} must end in .peg")
    stem = name[:-4]
    case_id, separator, description = stem.partition("_")
    if not separator or not case_id or not description:
        raise ManifestError(f"{context}: invalid source-case filename {name!r}")
    return case_id


def validate_manifest(data: dict[str, Any]) -> None:
    if set(data) != {"groups", "history"}:
        raise ManifestError("top-level keys must be exactly 'groups' and 'history'")
    groups = data["groups"]
    if not isinstance(groups, dict):
        raise ManifestError("'groups' must be a TOML table")
    unknown = sorted(set(groups) - set(GROUPS))
    missing = sorted(set(GROUPS) - set(groups))
    if unknown or missing:
        raise ManifestError(f"group table mismatch: missing={missing}, unknown={unknown}")

    # Check case-ID uniqueness using only the immutable manifest snapshot.
    # Live source inventory is intentionally checked by `check`, not here:
    # `unfreeze` must remain usable after a case is added, removed or renamed.
    frozen_case_ids: dict[str, str] = {}

    for group in GROUPS:
        entry = groups[group]
        context = f"groups.{group}"
        if not isinstance(entry, dict):
            raise ManifestError(f"{context} must be a table")
        status = _require_string(entry, "status", context, nonempty=True)
        if status == "unreviewed":
            if set(entry) != {"status"}:
                raise ManifestError(f"{context}: unreviewed groups may contain only status")
            continue
        if status != "frozen":
            raise ManifestError(f"{context}: unsupported status {status!r}")

        allowed = {"status", "reviewer", "date", "scope", "files", "hashes", "source_hashes"}
        if set(entry) != allowed:
            raise ManifestError(
                f"{context}: frozen entry keys must be {sorted(allowed)}; got {sorted(entry)}"
            )
        _require_string(entry, "reviewer", context, nonempty=True)
        _require_string(entry, "scope", context, nonempty=True)
        date = _require_string(entry, "date", context, nonempty=True)
        try:
            datetime.date.fromisoformat(date)
        except ValueError as exc:
            raise ManifestError(f"{context}: date must be YYYY-MM-DD") from exc

        hashes = _validate_hash_table(
            entry["hashes"], f"{context}.hashes", suffixes=(".tokens", ".filtered")
        )
        source_hashes = _validate_hash_table(
            entry["source_hashes"], f"{context}.source_hashes", suffixes=(".peg",)
        )
        if not source_hashes:
            raise ManifestError(f"{context}: a frozen group must record at least one source case")

        source_ids: set[str] = set()
        for source_name in source_hashes:
            case_id = _case_id_from_source_name(source_name, f"{context}.source_hashes")
            if case_id in source_ids:
                raise ManifestError(f"{context}: duplicate source case ID {case_id!r} in manifest")
            source_ids.add(case_id)
            previous = frozen_case_ids.get(case_id)
            if previous is not None:
                raise ManifestError(
                    f"manifest case ID {case_id!r} is duplicated across groups {previous!r} and {group!r}"
                )
            frozen_case_ids[case_id] = group

        expected_manifest_goldens = {
            f"{case_id}.{extension}"
            for case_id in source_ids
            for extension in ("tokens", "filtered")
        }
        if set(hashes) != expected_manifest_goldens:
            missing = sorted(expected_manifest_goldens - set(hashes))
            extra = sorted(set(hashes) - expected_manifest_goldens)
            raise ManifestError(
                f"{context}: manifest source/golden records disagree; "
                f"missing goldens={missing[:5]}, unexpected goldens={extra[:5]}"
            )

        files = entry["files"]
        if not isinstance(files, int) or isinstance(files, bool) or files != len(hashes):
            raise ManifestError(f"{context}: files must equal the number of golden hashes ({len(hashes)})")

    history = data["history"]
    if not isinstance(history, list):
        raise ManifestError("'history' must be an array of inline tables")
    latest_event: dict[str, dict[str, str]] = {}
    for i, event in enumerate(history):
        context = f"history[{i}]"
        if not isinstance(event, dict):
            raise ManifestError(f"{context} must be a table")
        if set(event) - {"date", "action", "group", "by", "reason"}:
            raise ManifestError(f"{context}: unexpected key(s)")
        action = _require_string(event, "action", context, nonempty=True)
        event_date = _require_string(event, "date", context, nonempty=True)
        try:
            datetime.date.fromisoformat(event_date)
        except ValueError as exc:
            raise ManifestError(f"{context}: date must be YYYY-MM-DD") from exc
        group = _require_string(event, "group", context, nonempty=True)
        if group not in GROUPS:
            raise ManifestError(f"{context}: unknown group {group!r}")
        latest_event[group] = event
        if action == "freeze":
            if set(event) != {"date", "action", "group", "by"}:
                raise ManifestError(f"{context}: freeze history entries require date, action, group and by")
            _require_string(event, "by", context, nonempty=True)
        elif action == "unfreeze":
            if set(event) != {"date", "action", "group", "reason"}:
                raise ManifestError(f"{context}: unfreeze history entries require date, action, group and reason")
            _require_string(event, "reason", context, nonempty=True)
        else:
            raise ManifestError(f"{context}: unsupported action {action!r}")

    for group in GROUPS:
        entry = groups[group]
        latest = latest_event.get(group)
        if entry["status"] == "frozen":
            if latest is None or latest["action"] != "freeze":
                raise ManifestError(f"groups.{group}: frozen status has no matching latest freeze history event")
            if latest["by"] != entry["reviewer"] or latest["date"] != entry["date"]:
                raise ManifestError(f"groups.{group}: reviewer/date do not match the latest freeze history event")
        elif latest is not None and latest["action"] == "freeze":
            raise ManifestError(f"groups.{group}: unreviewed status conflicts with the latest freeze history event")


def load() -> dict[str, Any]:
    if not REVIEW.is_file():
        raise ManifestError(
            f"review manifest is missing: {REVIEW}; refusing to assume that no files are frozen"
        )
    try:
        with open(REVIEW, "rb") as stream:
            data = tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise ManifestError(f"cannot parse {REVIEW}: {exc}") from exc
    validate_manifest(data)
    return data


def q(s: str) -> str:
    return json.dumps(s, ensure_ascii=False)


def save(data: dict[str, Any]) -> None:
    validate_manifest(data)
    out = [
        "# Review state for source cases and candidate golden files, one entry per corpus group.",
        "# Written by tools/goldens.py; edit only through its commands.",
        "# Frozen groups bind .peg source bytes and .tokens/.filtered golden bytes by SHA-256.",
        "history = [",
    ]
    for event in data["history"]:
        fields = ", ".join(
            f"{key} = {q(str(event[key]))}"
            for key in ("date", "action", "group", "by", "reason")
            if key in event
        )
        out.append(f"  {{ {fields} }},")
    out.extend(["]", ""])
    for group in GROUPS:
        entry = data["groups"][group]
        out.append(f"[groups.{group}]")
        out.append(f"status = {q(entry['status'])}")
        if entry["status"] == "frozen":
            out.append(f"reviewer = {q(entry['reviewer'])}")
            out.append(f"date = {q(entry['date'])}")
            out.append(f"scope = {q(entry['scope'])}")
            out.append(f"files = {entry['files']}")
            out.append("")
            out.append(f"[groups.{group}.hashes]")
            for name, digest in sorted(entry["hashes"].items()):
                out.append(f"{q(name)} = {q(digest)}")
            out.append("")
            out.append(f"[groups.{group}.source_hashes]")
            for name, digest in sorted(entry["source_hashes"].items()):
                out.append(f"{q(name)} = {q(digest)}")
        out.append("")
    REVIEW.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix="REVIEW.", suffix=".tmp", dir=REVIEW.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as stream:
            stream.write("\n".join(out))
        os.replace(tmp_name, REVIEW)
    except Exception:
        try:
            os.unlink(tmp_name)
        except OSError:
            pass
        raise


def arg(args: list[str], flag: str) -> str | None:
    if flag not in args:
        return None
    i = args.index(flag)
    if i + 1 >= len(args) or args[i + 1].startswith("--"):
        return None
    return args[i + 1]


def frozen_problems(data: dict[str, Any]) -> list[str]:
    """Report live repository drift against every frozen manifest snapshot.

    Inventory differences deliberately do not invalidate ``load``: they must
    block checks and bless, but must never block the explicit ``unfreeze`` path.
    """
    problems: list[str] = []
    for group in GROUPS:
        entry = data["groups"][group]
        if entry["status"] != "frozen":
            continue

        reviewed_sources = set(entry["source_hashes"])
        reviewed_goldens = set(entry["hashes"])
        try:
            current_sources = set(source_files(group))
            current_goldens = set(golden_files(group))
        except ManifestError as exc:
            problems.append(f"{group}: cannot inspect current case inventory: {exc}")
            continue

        for label, reviewed, current in (
            ("source-case", reviewed_sources, current_sources),
            ("golden", reviewed_goldens, current_goldens),
        ):
            added = sorted(current - reviewed)
            removed = sorted(reviewed - current)
            if added or removed:
                details = []
                if added:
                    details.append(f"added/unreviewed {added[:5]}")
                if removed:
                    details.append(f"removed/renamed {removed[:5]}")
                problems.append(f"{group}: frozen {label} inventory changed ({'; '.join(details)})")

        for name, expected in sorted(entry["hashes"].items()):
            path = CANDIDATES / name
            if not path.is_file():
                problems.append(f"{group}: frozen golden {name} is missing")
            elif sha256(path) != expected:
                problems.append(f"{group}: frozen golden {name} changed (hash mismatch)")
        for name, expected in sorted(entry["source_hashes"].items()):
            path = CASES / group / name
            if not path.is_file():
                problems.append(f"{group}: frozen source case {name} is missing")
            elif sha256(path) != expected:
                problems.append(f"{group}: frozen source case {name} changed (hash mismatch)")
    return problems


def cmd_status(data: dict[str, Any]) -> int:
    total_goldens = frozen_goldens = total_sources = frozen_sources = 0
    drift_by_group: dict[str, list[str]] = {}
    for problem in frozen_problems(data):
        group, _, detail = problem.partition(":")
        drift_by_group.setdefault(group, []).append(detail.strip() or problem)

    print(f"{'group':<12} {'cases':>5} {'files':>5}  status")
    for group in GROUPS:
        entry = data["groups"][group]
        sources = source_files(group)
        goldens = golden_files(group)
        total_goldens += len(goldens)
        total_sources += len(sources)
        if entry["status"] == "frozen":
            frozen_goldens += len(entry["hashes"])
            frozen_sources += len(entry["source_hashes"])
            if drift_by_group.get(group):
                who = f"FROZEN WITH DRIFT ({len(drift_by_group[group])} issue(s)); run check, then unfreeze"
            else:
                who = f"frozen by {entry['reviewer']} on {entry['date']}"
        else:
            who = "unreviewed"
        print(f"{group:<12} {len(sources):>5} {len(goldens):>5}  {who}")
    print(f"\n{frozen_goldens}/{total_goldens} golden files and {frozen_sources}/{total_sources} source cases frozen")
    return 0


def cmd_freeze(data: dict[str, Any], args: list[str]) -> int:
    if not args or args[0] not in GROUPS:
        raise ManifestError(f"usage: freeze GROUP --reviewer NAME [--scope TEXT]; groups: {', '.join(GROUPS)}")
    group = args[0]
    reviewer = arg(args, "--reviewer")
    if not reviewer or not reviewer.strip():
        raise ManifestError("freeze needs --reviewer NAME: the person who reviewed this group against the grammar")
    if data["groups"][group]["status"] == "frozen":
        raise ManifestError(f"{group} is already frozen; unfreeze it first, with a reason")

    sources = source_files(group)
    goldens = golden_files(group)
    if not sources:
        raise ManifestError(f"cannot freeze {group}: the group has no source cases")
    missing = [name for name in goldens if not (CANDIDATES / name).is_file()]
    if missing:
        raise ManifestError(f"cannot freeze {group}: missing candidate golden files {missing[:5]} (run pgcorpus bless)")
    today = datetime.date.today().isoformat()
    scope = arg(args, "--scope") or "all files reviewed against the grammar specification"
    if not scope.strip():
        raise ManifestError("--scope must not be empty")
    data["groups"][group] = {
        "status": "frozen",
        "reviewer": reviewer.strip(),
        "date": today,
        "scope": scope.strip(),
        "files": len(goldens),
        "hashes": {name: sha256(CANDIDATES / name) for name in goldens},
        "source_hashes": {name: sha256(CASES / group / name) for name in sources},
    }
    data["history"].append({"date": today, "action": "freeze", "group": group, "by": reviewer.strip()})
    save(data)
    print(f"froze {group}: {len(sources)} source cases + {len(goldens)} golden files, reviewer {reviewer.strip()}")
    return 0


def cmd_unfreeze(data: dict[str, Any], args: list[str]) -> int:
    if not args or args[0] not in GROUPS:
        raise ManifestError("usage: unfreeze GROUP --reason TEXT")
    group = args[0]
    reason = arg(args, "--reason")
    if not reason or not reason.strip():
        raise ManifestError("unfreeze needs --reason TEXT; it is kept in REVIEW.toml history")
    if data["groups"][group]["status"] != "frozen":
        raise ManifestError(f"{group} is not frozen")
    data["groups"][group] = {"status": "unreviewed"}
    data["history"].append({
        "date": datetime.date.today().isoformat(),
        "action": "unfreeze",
        "group": group,
        "reason": reason.strip(),
    })
    save(data)
    print(f"unfroze {group}; it needs a new review before it can be frozen again")
    return 0


def cmd_check(
    data: dict[str, Any],
    *,
    require_all_frozen: bool = False,
    require_phase2_frozen: bool = False,
    quiet: bool = False,
) -> int:
    problems = frozen_problems(data)
    frozen = [group for group in GROUPS if data["groups"][group]["status"] == "frozen"]
    required: list[str] = []
    if require_all_frozen:
        required += PHASE1_GROUPS
    if require_phase2_frozen:
        required += PHASE2_GROUPS
    problems.extend(f"{group}: review group is not frozen" for group in required if group not in frozen)
    if problems:
        if not quiet:
            print("\n".join(problems), file=sys.stderr)
            print(f"\n{len(problems)} problem(s) with golden review state", file=sys.stderr)
        return 1
    if not quiet:
        if required:
            print(f"OK: required groups frozen ({', '.join(required)}); all frozen source and golden hashes unchanged")
        else:
            print(f"OK: {len(frozen)} of {len(GROUPS)} groups frozen; all frozen source and golden hashes unchanged")
    return 0


def cmd_frozen_files(data: dict[str, Any]) -> int:
    """Machine interface: emit protected filenames only after fail-closed checks."""
    problems = frozen_problems(data)
    if problems:
        print("\n".join(problems), file=sys.stderr)
        print("pgcorpus bless refused: frozen source/golden integrity check failed", file=sys.stderr)
        return 1
    names = sorted(
        name
        for group in GROUPS
        if data["groups"][group]["status"] == "frozen"
        for name in data["groups"][group]["hashes"]
    )
    for name in names:
        print(name)
    return 0


def main() -> int:
    args = sys.argv[1:]
    command = args[0] if args else "status"
    try:
        data = load()
        if command == "status":
            return cmd_status(data)
        if command == "freeze":
            return cmd_freeze(data, args[1:])
        if command == "unfreeze":
            return cmd_unfreeze(data, args[1:])
        if command == "check":
            known = {"--require-all-frozen", "--require-phase2-frozen"}
            extra = [arg for arg in args[1:] if arg not in known]
            if extra:
                raise ManifestError(f"unknown check argument(s): {extra}")
            return cmd_check(
                data,
                require_all_frozen="--require-all-frozen" in args[1:],
                require_phase2_frozen="--require-phase2-frozen" in args[1:],
            )
        if command == "frozen-files":
            if len(args) != 1:
                raise ManifestError("frozen-files takes no arguments")
            return cmd_frozen_files(data)
        raise ManifestError(f"unknown command {command!r}")
    except (ManifestError, OSError) as exc:
        print(f"goldens.py: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())

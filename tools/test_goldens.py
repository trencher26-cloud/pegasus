#!/usr/bin/env python3
"""Adversarial tests for the golden review/freeze manager."""
from __future__ import annotations

import contextlib
import io
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import goldens


class GoldenReviewTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.cases = self.root / "tests" / "grammar" / "cases"
        self.candidates = self.root / "tests" / "grammar" / "goldens" / "candidates"
        self.goldens_dir = self.candidates.parent
        self.review = self.goldens_dir / "REVIEW.toml"
        self.candidates.mkdir(parents=True)

        case_prefix = {
            "phase0": "P0", "lexical": "L", "newline": "N",
            "precedence": "E", "ambiguity": "A", "items": "I", "recovery": "R", "parser": "S",
        }
        for group in goldens.GROUPS:
            directory = self.cases / group
            directory.mkdir(parents=True)
            case_id = f"{case_prefix[group]}-001"
            (directory / f"{case_id}_case.peg").write_bytes(b"fn t() = 1\n")
            (self.candidates / f"{case_id}.tokens").write_text("tokens\n", encoding="utf-8")
            (self.candidates / f"{case_id}.filtered").write_text("filtered\n", encoding="utf-8")

        self.patches = [
            patch.object(goldens, "ROOT", self.root),
            patch.object(goldens, "CASES", self.cases),
            patch.object(goldens, "GOLDENS", self.goldens_dir),
            patch.object(goldens, "CANDIDATES", self.candidates),
            patch.object(goldens, "REVIEW", self.review),
        ]
        for item in self.patches:
            item.start()
        self.addCleanup(self._stop_patches)
        self.data = goldens.default_data()
        goldens.save(self.data)

    def _stop_patches(self) -> None:
        for item in reversed(self.patches):
            item.stop()
        self.temp.cleanup()

    def freeze_recovery(self) -> dict:
        data = goldens.load()
        with contextlib.redirect_stdout(io.StringIO()):
            rc = goldens.cmd_freeze(
                data,
                ["recovery", "--reviewer", "Independent Reviewer", "--scope", "reviewed against grammar"],
            )
        self.assertEqual(rc, 0)
        return goldens.load()

    def freeze_groups(self, groups) -> dict:
        for group in groups:
            data = goldens.load()
            with contextlib.redirect_stdout(io.StringIO()):
                rc = goldens.cmd_freeze(data, [group, "--reviewer", "Independent Reviewer", "--scope", "reviewed"])
            self.assertEqual(rc, 0)
        return goldens.load()

    def test_phase1_gate_ignores_the_parser_group(self) -> None:
        # Gate 2 (`--require-all-frozen`) means the seven Phase 1 groups, however
        # many Phase 2 groups exist.
        data = self.freeze_groups(goldens.PHASE1_GROUPS)
        self.assertEqual(data["groups"]["parser"], {"status": "unreviewed"})
        self.assertEqual(goldens.cmd_check(data, require_all_frozen=True, quiet=True), 0)
        self.assertNotEqual(goldens.cmd_check(data, require_phase2_frozen=True, quiet=True), 0)

    def test_phase2_gate_needs_the_parser_group(self) -> None:
        data = self.freeze_groups(["parser"])
        self.assertEqual(goldens.cmd_check(data, require_phase2_frozen=True, quiet=True), 0)
        self.assertNotEqual(goldens.cmd_check(data, require_all_frozen=True, quiet=True), 0)

    def test_both_gates_together(self) -> None:
        data = self.freeze_groups(goldens.GROUPS)
        self.assertEqual(
            goldens.cmd_check(data, require_all_frozen=True, require_phase2_frozen=True, quiet=True), 0
        )

    def test_freeze_binds_source_and_goldens(self) -> None:
        data = self.freeze_recovery()
        entry = data["groups"]["recovery"]
        self.assertEqual(len(entry["hashes"]), 2)
        self.assertEqual(len(entry["source_hashes"]), 1)
        self.assertEqual(goldens.cmd_check(data, quiet=True), 0)

    def test_source_edit_is_detected(self) -> None:
        data = self.freeze_recovery()
        source = self.cases / "recovery" / "R-001_case.peg"
        source.write_bytes(b"fn t() = 2\n")
        problems = goldens.frozen_problems(data)
        self.assertTrue(any("source case" in problem and "hash mismatch" in problem for problem in problems))
        self.assertEqual(goldens.cmd_check(data, quiet=True), 1)

    def test_added_case_to_frozen_group_is_detected_and_can_be_unfrozen(self) -> None:
        data = self.freeze_recovery()
        new_source = self.cases / "recovery" / "R-002_new.peg"
        new_source.write_bytes(b"fn x() = 2\n")
        (self.candidates / "R-002.tokens").write_text("new tokens\n", encoding="utf-8")
        (self.candidates / "R-002.filtered").write_text("new filtered\n", encoding="utf-8")

        # The manifest remains structurally valid: live inventory drift belongs
        # to `check`, not `load`, so that the group can always be unfrozen.
        loaded = goldens.load()
        self.assertEqual(goldens.validate_manifest(loaded), None)
        self.assertEqual(goldens.cmd_check(loaded, quiet=True), 1)
        status = io.StringIO()
        with contextlib.redirect_stdout(status):
            self.assertEqual(goldens.cmd_status(loaded), 0)
        self.assertIn("FROZEN WITH DRIFT", status.getvalue())

        output = io.StringIO()
        errors = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            self.assertEqual(goldens.cmd_frozen_files(loaded), 1)
        self.assertEqual(output.getvalue(), "")
        self.assertIn("inventory changed", errors.getvalue())

        with patch.object(
            goldens.sys,
            "argv",
            [
                "goldens.py",
                "unfreeze",
                "recovery",
                "--reason",
                "added a new recovery case requiring review",
            ],
        ), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(goldens.main(), 0)
        unfrozen = goldens.load()
        self.assertEqual(unfrozen["groups"]["recovery"], {"status": "unreviewed"})
        self.assertEqual(goldens.cmd_check(unfrozen, quiet=True), 0)
        self.assertEqual(unfrozen["history"][-1]["action"], "unfreeze")

    def test_renamed_source_case_can_be_unfrozen(self) -> None:
        data = self.freeze_recovery()
        old_path = self.cases / "recovery" / "R-001_case.peg"
        new_path = self.cases / "recovery" / "R-001_renamed_case.peg"
        old_path.rename(new_path)

        loaded = goldens.load()
        self.assertEqual(goldens.cmd_check(loaded, quiet=True), 1)
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(
                goldens.cmd_unfreeze(
                    loaded,
                    ["recovery", "--reason", "renamed the case file"],
                ),
                0,
            )
        self.assertEqual(goldens.load()["groups"]["recovery"], {"status": "unreviewed"})

    def test_missing_manifest_fails_closed(self) -> None:
        self.review.unlink()
        with self.assertRaises(goldens.ManifestError):
            goldens.load()

    def test_malformed_hash_is_rejected(self) -> None:
        data = self.freeze_recovery()
        entry = data["groups"]["recovery"]
        digest = next(iter(entry["hashes"].values()))
        text = self.review.read_text(encoding="utf-8").replace(digest, "abc", 1)
        self.review.write_text(text, encoding="utf-8")
        with self.assertRaises(goldens.ManifestError):
            goldens.load()

    def test_missing_history_is_rejected(self) -> None:
        text = self.review.read_text(encoding="utf-8").replace("history = [\n]\n", "", 1)
        self.review.write_text(text, encoding="utf-8")
        with self.assertRaises(goldens.ManifestError):
            goldens.load()

    def test_malformed_toml_is_rejected(self) -> None:
        self.review.write_text('[groups.recovery\nstatus = "frozen"\n', encoding="utf-8")
        with self.assertRaises(goldens.ManifestError):
            goldens.load()

    def test_malformed_history_event_is_rejected(self) -> None:
        data = self.freeze_recovery()
        text = self.review.read_text(encoding="utf-8").replace('by = "Independent Reviewer"', 'reason = "not a valid freeze event"', 1)
        self.review.write_text(text, encoding="utf-8")
        with self.assertRaises(goldens.ManifestError):
            goldens.load()

    def test_status_must_match_review_history(self) -> None:
        data = self.freeze_recovery()
        data["groups"]["recovery"] = {"status": "unreviewed"}
        with self.assertRaises(goldens.ManifestError):
            goldens.validate_manifest(data)

    def test_unexpected_manifest_key_is_rejected(self) -> None:
        text = self.review.read_text(encoding="utf-8") + "\n[groups.recovery.extra]\nstatus = \"frozen\"\n"
        self.review.write_text(text, encoding="utf-8")
        with self.assertRaises(goldens.ManifestError):
            goldens.load()

    def test_require_all_frozen_gate(self) -> None:
        data = self.freeze_recovery()
        self.assertEqual(goldens.cmd_check(data, require_all_frozen=True, quiet=True), 1)
        for group in goldens.GROUPS:
            if group != "recovery":
                data = goldens.load()
                with contextlib.redirect_stdout(io.StringIO()):
                    goldens.cmd_freeze(data, [group, "--reviewer", "Independent Reviewer"])
        data = goldens.load()
        self.assertEqual(goldens.cmd_check(data, require_all_frozen=True, quiet=True), 0)

    def test_frozen_files_emits_inventory_only_when_integrity_passes(self) -> None:
        data = self.freeze_recovery()
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(goldens.cmd_frozen_files(data), 0)
        names = output.getvalue().splitlines()
        self.assertEqual(names, ["R-001.filtered", "R-001.tokens"])

        source = self.cases / "recovery" / "R-001_case.peg"
        source.write_bytes(b"changed\n")
        output = io.StringIO()
        errors = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            self.assertEqual(goldens.cmd_frozen_files(data), 1)
        self.assertIn("refused", errors.getvalue())
        self.assertEqual(output.getvalue(), "")

    def test_freeze_requires_all_candidate_goldens(self) -> None:
        (self.candidates / "R-001.filtered").unlink()
        with self.assertRaises(goldens.ManifestError):
            goldens.cmd_freeze(goldens.load(), ["recovery", "--reviewer", "Reviewer"])

    def test_unfreeze_requires_reason_and_preserves_history(self) -> None:
        data = self.freeze_recovery()
        with self.assertRaises(goldens.ManifestError):
            goldens.cmd_unfreeze(data, ["recovery"])
        with contextlib.redirect_stdout(io.StringIO()):
            goldens.cmd_unfreeze(data, ["recovery", "--reason", "corrected a grammar interpretation"])
        saved = goldens.load()
        self.assertEqual(saved["groups"]["recovery"]["status"], "unreviewed")
        self.assertEqual(saved["history"][-1]["reason"], "corrected a grammar interpretation")


if __name__ == "__main__":
    unittest.main(verbosity=2)

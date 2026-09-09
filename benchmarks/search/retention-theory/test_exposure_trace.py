#!/usr/bin/env python3
"""Trace witnesses distinguish selection bookkeeping from known continuation."""
import unittest
from analyze_exposure_trace import summarize, validate_header


def job(sequence, parent, decisions):
    return {"event": "job", "sequence": sequence, "parent_id": parent, "decisions": decisions}


def retained(identity):
    return {"decision": "retained", "id": identity}


class ExposureTrace(unittest.TestCase):
    def test_workload_specific_verified_formats_are_explicit(self):
        for game, version in [("metroid", 4), ("mm2", 1)]:
            validate_header({"format": f"{game}-quicknes-campaign-stream-v{version}",
                             "origin_kind": "genesis", "resume_actions": 0}, game)

    def test_a_new_format_or_imported_origin_is_not_silently_accepted(self):
        for format_, origin in [("mm2-quicknes-campaign-stream-v4", "genesis"),
                                ("mm2-quicknes-campaign-stream-v1", "archive")]:
            with self.subTest(format=format_, origin=origin), self.assertRaises(AssertionError):
                validate_header({"format": format_, "origin_kind": origin, "resume_actions": 0}, "mm2")

    def test_birth_continuation_and_skip_only_selection_are_distinct(self):
        result = summarize([job(1, 0, [retained(1), retained(2)]), job(2, 2, [retained(3)]),
                            {"event": "skip", "parent_id": 3}])
        self.assertEqual(result["created_entries_excluding_genesis"], 3)
        self.assertEqual(result["created_entries_never_referenced_as_parents"], 1)
        self.assertEqual(result["never_referenced_entries_proven_continued_in_birth_job"], 1)
        self.assertEqual(result["created_entries_referenced_only_by_skips"], 1)
        self.assertEqual(result["created_entries_never_job_parents"], 2)

    def test_later_parent_job_removes_the_end_of_stream_no_reference_classification(self):
        result = summarize([job(1, 0, [retained(1), retained(2)]), job(2, 1, [])])
        self.assertEqual(result["created_entries_never_referenced_as_parents"], 1)
        self.assertEqual(result["never_referenced_entries_proven_continued_in_birth_job"], 0)

    def test_a_preceding_victory_decision_does_not_prove_a_later_action(self):
        result = summarize([job(1, 0, [{"decision": "victory"}, retained(1)])])
        self.assertEqual(result["never_referenced_entries_proven_continued_in_birth_job"], 0)

    def test_corrupt_lineage_or_reused_birth_fails_closed(self):
        for records in [[job(1, 7, [])], [job(1, 0, [retained(1), retained(1)])]]:
            with self.subTest(records=records), self.assertRaises(AssertionError):
                summarize(records)


if __name__ == "__main__":
    unittest.main()

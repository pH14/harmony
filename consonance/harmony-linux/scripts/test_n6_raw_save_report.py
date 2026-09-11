# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "n6_raw_save_report", Path(__file__).with_name("n6-raw-save-report.py")
)
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)


def fixture(changes=None):
    changes = changes or {}
    lines, raw = [], []
    for ordinal, name in enumerate(report.NAMES, 1):
        if name == "XSAVES":
            result = "signal:4"
        else:
            data = bytearray(4096)
            for offset, value in changes.get(name, {}).items():
                data[offset] = value
            digest = report.fnv(data)
            result = f"value:0000000000000000:mem:{digest}"
            raw.append(f"N6_RAW_SAVE name={name} value=0000000000000000 hash={digest} bytes={data.hex()}")
        lines.append(f"N6_OPERATION arch=x86_64 row=x86-xsave-image operation={ordinal}/6 name={name} result={result}")
    return "\n".join(lines + raw + [
        "N6_GUEST_OK arch=x86_64 table_rows=10 exercised_rows=10 operations=174"
    ]) + "\n"


class RawSaveTests(unittest.TestCase):
    def test_hash_matches_the_existing_guest_seed(self):
        self.assertEqual(report.fnv(bytes(4096)), "b43a063055adc383")

    def test_equal_and_changed_bytes_cover_the_entire_save_area(self):
        with tempfile.TemporaryDirectory() as temporary:
            first, second = Path(temporary) / "first", Path(temporary) / "second"
            first.write_text(fixture())
            second.write_text(fixture())
            self.assertTrue(report.compare(first, second)["equal"])
            offsets = [0, 511, 512, 527, 4095]
            second.write_text(fixture({"XSAVEOPT": {offset: index + 1 for index, offset in enumerate(offsets)}}))
            result = report.compare(first, second)
            self.assertFalse(result["equal"])
            observed = result["operations"]["XSAVEOPT"]
            self.assertEqual(observed["first_byte_difference"], 0)
            self.assertEqual(observed["byte_differences"],
                             [[offset, 0, index + 1] for index, offset in enumerate(offsets)])
            self.assertTrue(result["operations"]["XSAVE"]["equal"])

    def test_live_collector_completion_prefix_is_a_valid_cut(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "capture"
            prefix = fixture().split("N6_GUEST_OK ")[0] + "N6_GUEST_OK arch=x86_64"
            path.write_text(prefix)
            self.assertEqual(len(report.read_capture(path)["captures"]), 5)
            path.write_text(prefix + " table_")
            self.assertEqual(len(report.read_capture(path)["captures"]), 5)

    def test_missing_duplicate_malformed_and_misbound_data_is_rejected(self):
        valid = fixture()
        raw = next(line for line in valid.splitlines() if line.startswith("N6_RAW_SAVE name=XSAVE "))
        variants = {
            "missing": valid.replace(raw + "\n", ""),
            "duplicate": valid + raw + "\n",
            "raw_hash": valid.replace(raw, raw[:-1] + "1"),
            "observation_hash": valid.replace("result=value:0000000000000000:mem:b43a063055adc383",
                                               "result=value:0000000000000000:mem:0000000000000000", 1),
            "wrong_value": valid.replace(raw, raw.replace("value=0000000000000000", "value=0000000000000001")),
            "short": valid.replace(raw, raw[:-2]),
            "no_completion": "\n".join(valid.splitlines()[:-1]),
            "extra_signal_capture": valid + raw.replace("name=XSAVE ", "name=XSAVES ") + "\n",
            "reordered": valid.replace("operation=3/6 name=XSAVE ", "operation=4/6 name=XSAVE "),
        }
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "capture"
            for name, text in variants.items():
                with self.subTest(name=name):
                    path.write_text(text)
                    with self.assertRaises(ValueError):
                        report.read_capture(path)


if __name__ == "__main__":
    unittest.main()

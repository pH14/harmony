# SPDX-License-Identifier: AGPL-3.0-or-later
import tempfile
import unittest
from pathlib import Path

from patch import edit_tmx
from source import read_level


class GeometryTests(unittest.TestCase):
    text = '''<map orientation="orthogonal" infinite="0" tilewidth="16" tileheight="16">
 <tileset firstgid="1" name="sprites"/><tileset firstgid="257" name="famidash"/>
 <layer id="1" width="3" height="1"><data encoding="csv">
336,257,0
</data></layer></map>'''

    def test_reordered_tilesets_use_the_same_ids_as_the_upstream_exporter(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "level.tmx"
            path.write_text(self.text)
            layout = read_level(path)
        self.assertEqual(layout["tiles"], [79, 0, 0])
        self.assertEqual(layout["top"], 896)

    def test_reviewed_edits_preserve_unrelated_text_and_require_expected_tiles(self):
        edit = {"column": 0, "row": 0, "from_gid": 336, "to_gid": 274}
        self.assertEqual(edit_tmx(self.text, [edit]), self.text.replace("336,257,0", "274,257,0"))
        with self.assertRaises(AssertionError):
            edit_tmx(self.text, [{**edit, "from_gid": 80}])
        with self.assertRaises(AssertionError):
            edit_tmx(self.text, [edit, edit])
        with self.assertRaises(AssertionError):
            edit_tmx(self.text, [{**edit, "column": 3}])


if __name__ == "__main__":
    unittest.main()

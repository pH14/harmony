# SPDX-License-Identifier: AGPL-3.0-or-later
import unittest
from transform import exported_state, instrument, parse, render

class TransformTests(unittest.TestCase):
    def test_ticks_cover_nested_loop_and_preserve_module_name(self):
        module = parse('(module $fixture (type $t (func)) (import "x" "y" (func $y)) (func $f (loop $again (br $again))))')
        counts = instrument(module)
        self.assertEqual(counts, {"functions": 1, "loops": 1})
        rendered = render(module)
        self.assertTrue(rendered.startswith('(module $fixture (import "harmony_v1"'))
        self.assertIn('(loop $again (call $harmony_tick) (br $again))', rendered)
        self.assertEqual(render(parse(rendered)), rendered)

    def test_exports_include_imported_function_and_mutable_state(self):
        module = parse('(module (import "x" "y" (func $y)) (func $f) (global $g (mut i32) (i32.const 0)) (table $t 1 1 funcref))')
        exported_state(module)
        source = render(module)
        self.assertIn('(export "__harmony_func_0" (func 0))', source)
        self.assertIn('(export "__harmony_func_1" (func 1))', source)
        self.assertIn('(export "__harmony_global_0" (global 0))', source)
        self.assertIn('(export "__harmony_table_0" (table 0))', source)

    def test_start_is_rejected_before_instrumentation(self):
        module = parse('(module (func $start) (start $start))')
        with self.assertRaisesRegex(ValueError, "automatic start"):
            instrument(module)

    def test_quoted_bytes_survive_round_trip(self):
        source = '(module (memory 1) (data (i32.const 0) "a\\00\\22\\5c"))'
        self.assertEqual(render(parse(source)), source)

if __name__ == "__main__":
    unittest.main()

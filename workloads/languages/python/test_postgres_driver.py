# SPDX-License-Identifier: AGPL-3.0-or-later
import unittest
from types import SimpleNamespace
from unittest.mock import patch

import postgres_driver as driver


class PostgresOracleTest(unittest.TestCase):
    def verdict(self, status, output, ready=0):
        assertions = []
        reached = []
        responses = [SimpleNamespace(returncode=ready, stdout="")]
        if ready == 0:
            responses.append(SimpleNamespace(returncode=status, stdout=output))
        with patch.object(driver, "run", side_effect=responses), \
             patch.object(driver, "always", side_effect=lambda condition, message: assertions.append(condition)), \
             patch.object(driver, "reachable", side_effect=reached.append):
            self.assertEqual(driver.check(), 0)
        return assertions, reached

    def test_only_missing_heap_tuple_is_a_violation(self):
        assertions, reached = self.verdict(1, "heap tuple (12,3) lacks matching index tuple")
        self.assertEqual(assertions, [False])
        self.assertIn("amcheck compared the index", reached)
        self.assertEqual(self.verdict(0, "")[0], [True])

    def test_unavailable_or_inconclusive_comparison_stays_silent(self):
        self.assertEqual(self.verdict(1, "connection lost")[0], [])
        self.assertEqual(self.verdict(1, "index does not exist")[0], [])
        assertions, reached = self.verdict(1, "", ready=1)
        self.assertEqual(assertions, [])
        self.assertEqual(reached, ["amcheck found the server down"])


if __name__ == "__main__":
    unittest.main()

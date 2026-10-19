import unittest

from tallysheet import matches, status_line, total


class TestTotalExactValues(unittest.TestCase):
    def test_issue_regression_guards(self):
        # Exact values quoted in ISSUE.md as already working.
        self.assertEqual(total(["19.99", "0.01"]), "20.00")
        self.assertEqual(total(["2.675", "0.005"]), "2.68")

    def test_issue_rounding_case(self):
        # Exact value quoted in ISSUE.md as wrong today.
        self.assertEqual(total(["1.005"]), "1.01")

    def test_float_trap_sum(self):
        self.assertEqual(total(["0.1", "0.2"]), "0.30")
        self.assertEqual(total(["0.1", "0.1", "0.1"]), "0.30")

    def test_simple_sums(self):
        self.assertEqual(total(["10.00", "5.00"]), "15.00")
        self.assertEqual(total([]), "0.00")


class TestMatchesExactValues(unittest.TestCase):
    def test_issue_reproduction(self):
        # Exact sheet quoted in ISSUE.md.
        self.assertTrue(matches(["0.1", "0.2"], "0.30"))

    def test_balanced_after_rounding(self):
        self.assertTrue(matches(["2.675", "0.005"], "2.68"))

    def test_real_mismatch_still_flags(self):
        self.assertFalse(matches(["0.1", "0.2"], "0.31"))

    def test_status_line_ok_for_issue_sheet(self):
        self.assertEqual(
            status_line(["0.1", "0.2"], "0.30"),
            "OK: total 0.30, expected 0.30",
        )


if __name__ == "__main__":
    unittest.main()

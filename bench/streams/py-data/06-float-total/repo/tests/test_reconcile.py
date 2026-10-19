import unittest

from tallysheet import matches, status_line, total


class TestTotal(unittest.TestCase):
    def test_simple_sum(self):
        self.assertEqual(total(["10.00", "5.00"]), "15.00")

    def test_empty_sheet(self):
        self.assertEqual(total([]), "0.00")

    def test_cents_carried(self):
        self.assertEqual(total(["1.50", "2.75"]), "4.25")


class TestMatches(unittest.TestCase):
    def test_matching_sheet(self):
        self.assertTrue(matches(["10.00", "5.00"], "15.00"))

    def test_mismatched_sheet(self):
        self.assertFalse(matches(["10.00", "5.00"], "14.00"))


class TestStatusLine(unittest.TestCase):
    def test_ok(self):
        self.assertEqual(
            status_line(["10.00", "5.00"], "15.00"),
            "OK: total 15.00, expected 15.00",
        )

    def test_mismatch(self):
        self.assertEqual(
            status_line(["10.00"], "11.00"),
            "MISMATCH: total 10.00, expected 11.00",
        )


if __name__ == "__main__":
    unittest.main()

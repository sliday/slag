import os
import tempfile
import unittest

from ledgercsv import read_rows
from ledgercsv.summary import count_by_description, total_amount

CSV_TEXT = (
    "date,description,amount\n"
    "2024-01-05,Office supplies,42.50\n"
    "2024-01-06,Client lunch,18.20\n"
)


class TestReadRows(unittest.TestCase):
    def _write(self, text):
        fd, path = tempfile.mkstemp(suffix=".csv")
        with os.fdopen(fd, "w") as f:
            f.write(text)
        self.addCleanup(os.unlink, path)
        return path

    def test_parses_header_and_rows(self):
        rows = read_rows(self._write(CSV_TEXT))
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["date"], "2024-01-05")
        self.assertEqual(rows[0]["description"], "Office supplies")
        self.assertEqual(rows[1]["amount"], "18.20")


class TestSummary(unittest.TestCase):
    def _write(self, text):
        fd, path = tempfile.mkstemp(suffix=".csv")
        with os.fdopen(fd, "w") as f:
            f.write(text)
        self.addCleanup(os.unlink, path)
        return path

    def test_total_amount(self):
        rows = read_rows(self._write(CSV_TEXT))
        self.assertEqual(str(total_amount(rows)), "60.70")

    def test_count_by_description(self):
        rows = read_rows(self._write(CSV_TEXT))
        self.assertEqual(
            count_by_description(rows),
            {"Office supplies": 1, "Client lunch": 1},
        )


if __name__ == "__main__":
    unittest.main()

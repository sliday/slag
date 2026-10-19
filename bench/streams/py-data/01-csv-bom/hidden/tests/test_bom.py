import os
import tempfile
import unittest

from ledgercsv import read_rows
from ledgercsv.summary import total_amount

# Exact bytes quoted in ISSUE.md: UTF-8 BOM, then the CSV text.
CSV_BYTES = (
    b"\xef\xbb\xbfdate,description,amount\n"
    b"2024-03-04,Office supplies,42.50\n"
    b"2024-03-05,Client lunch,18.20\n"
)


class TestReadRowsWithBom(unittest.TestCase):
    def _write(self, data):
        fd, path = tempfile.mkstemp(suffix=".csv")
        with os.fdopen(fd, "wb") as f:
            f.write(data)
        self.addCleanup(os.unlink, path)
        return path

    def test_first_header_is_date(self):
        rows = read_rows(self._write(CSV_BYTES))
        self.assertEqual(list(rows[0].keys()), ["date", "description", "amount"])

    def test_rows_parse(self):
        rows = read_rows(self._write(CSV_BYTES))
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["date"], "2024-03-04")
        self.assertEqual(rows[0]["description"], "Office supplies")
        self.assertEqual(rows[1]["amount"], "18.20")

    def test_summary_still_works(self):
        rows = read_rows(self._write(CSV_BYTES))
        self.assertEqual(str(total_amount(rows)), "60.70")


if __name__ == "__main__":
    unittest.main()

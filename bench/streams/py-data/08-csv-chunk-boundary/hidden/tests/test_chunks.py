import os
import tempfile
import unittest

from streamcsv import count_rows, iter_rows

# Exact bytes quoted in ISSUE.md, including the trailing newline.
CSV_BYTES = (
    b"id,title,body\n"
    b"1,hello,\"first line\n"
    b"second line\"\n"
    b"2,plain,no quotes\n"
    b"3,\"comma, inside\",ok\n"
    b"4,last,\"multi\n"
    b"line\n"
    b"field\"\n"
)

EXPECTED = [
    ["id", "title", "body"],
    ["1", "hello", "first line\nsecond line"],
    ["2", "plain", "no quotes"],
    ["3", "comma, inside", "ok"],
    ["4", "last", "multi\nline\nfield"],
]


class TestIterRowsChunkBoundary(unittest.TestCase):
    def _write(self, data):
        fd, path = tempfile.mkstemp(suffix=".csv")
        with os.fdopen(fd, "wb") as f:
            f.write(data)
        self.addCleanup(os.unlink, path)
        return path

    def test_issue_reproduction_chunk_64(self):
        rows = list(iter_rows(self._write(CSV_BYTES), chunk_size=64))
        self.assertEqual(rows, EXPECTED)

    def test_chunk_size_does_not_change_result(self):
        path = self._write(CSV_BYTES)
        for size in (1, 7, 16, 64, 4096):
            with self.subTest(chunk_size=size):
                self.assertEqual(list(iter_rows(path, chunk_size=size)), EXPECTED)

    def test_count_rows(self):
        self.assertEqual(count_rows(self._write(CSV_BYTES), chunk_size=64), 5)

    def test_quoted_fields_intact(self):
        rows = list(iter_rows(self._write(CSV_BYTES), chunk_size=16))
        self.assertEqual(rows[1][2], "first line\nsecond line")
        self.assertEqual(rows[3][1], "comma, inside")
        self.assertEqual(rows[4][2], "multi\nline\nfield")


if __name__ == "__main__":
    unittest.main()

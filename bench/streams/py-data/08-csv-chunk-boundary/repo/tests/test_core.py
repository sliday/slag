import os
import tempfile
import unittest

from streamcsv import count_rows, iter_rows

CSV_TEXT = "id,name,score\n1,Ana,10\n2,Bob,20\n"


class TestIterRows(unittest.TestCase):
    def _write(self, text):
        fd, path = tempfile.mkstemp(suffix=".csv")
        with os.fdopen(fd, "w") as f:
            f.write(text)
        self.addCleanup(os.unlink, path)
        return path

    def test_reads_rows(self):
        rows = list(iter_rows(self._write(CSV_TEXT)))
        self.assertEqual(
            rows,
            [["id", "name", "score"], ["1", "Ana", "10"], ["2", "Bob", "20"]],
        )

    def test_small_chunks_same_result(self):
        path = self._write(CSV_TEXT)
        whole = list(iter_rows(path, chunk_size=4096))
        tiny = list(iter_rows(path, chunk_size=4))
        self.assertEqual(tiny, whole)

    def test_trailing_newline_ignored(self):
        self.assertEqual(
            list(iter_rows(self._write("a,b\n1,2\n"))), [["a", "b"], ["1", "2"]]
        )


class TestCountRows(unittest.TestCase):
    def _write(self, text):
        fd, path = tempfile.mkstemp(suffix=".csv")
        with os.fdopen(fd, "w") as f:
            f.write(text)
        self.addCleanup(os.unlink, path)
        return path

    def test_count(self):
        self.assertEqual(count_rows(self._write(CSV_TEXT)), 3)


if __name__ == "__main__":
    unittest.main()

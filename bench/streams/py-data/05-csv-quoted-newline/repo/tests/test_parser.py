import unittest

from csvsplit import column, parse, row_count

CSV_TEXT = "name,age,city\nAna,31,Lisbon\nBob,25,Porto\n"


class TestParse(unittest.TestCase):
    def test_parses_rows_and_fields(self):
        rows = parse(CSV_TEXT)
        self.assertEqual(
            rows,
            [
                ["name", "age", "city"],
                ["Ana", "31", "Lisbon"],
                ["Bob", "25", "Porto"],
            ],
        )

    def test_trailing_newline_ignored(self):
        self.assertEqual(parse("a,b\n1,2\n"), [["a", "b"], ["1", "2"]])

    def test_single_column(self):
        self.assertEqual(parse("a\n1\n"), [["a"], ["1"]])


class TestReport(unittest.TestCase):
    def test_column(self):
        rows = parse(CSV_TEXT)
        self.assertEqual(column(rows, 0), ["name", "Ana", "Bob"])

    def test_row_count(self):
        self.assertEqual(row_count(parse(CSV_TEXT)), 3)


if __name__ == "__main__":
    unittest.main()

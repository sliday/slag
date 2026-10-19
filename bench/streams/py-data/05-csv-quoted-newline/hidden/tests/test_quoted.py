import unittest

from csvsplit import parse

# Exact CSV text quoted in ISSUE.md, including the trailing newline.
CSV_TEXT = (
    "name,address,note\n"
    "\"Smith, John\",\"12 Main St\n"
    "Apt 4\",vip\n"
    "\"Doe, Jane\",5 Oak Ave,regular\n"
    "Brown,9 Pine Rd,new\n"
    "\"Adams, Amy\",\"77 Lake View\n"
    "Suite 2\",vip\n"
)

EXPECTED = [
    ["name", "address", "note"],
    ["Smith, John", "12 Main St\nApt 4", "vip"],
    ["Doe, Jane", "5 Oak Ave", "regular"],
    ["Brown", "9 Pine Rd", "new"],
    ["Adams, Amy", "77 Lake View\nSuite 2", "vip"],
]


class TestParseQuotedFields(unittest.TestCase):
    def test_issue_reproduction(self):
        self.assertEqual(parse(CSV_TEXT), EXPECTED)

    def test_record_count(self):
        self.assertEqual(len(parse(CSV_TEXT)), 5)

    def test_quoted_comma_and_newline_fields_intact(self):
        rows = parse(CSV_TEXT)
        self.assertEqual(rows[1][0], "Smith, John")
        self.assertEqual(rows[1][1], "12 Main St\nApt 4")
        self.assertEqual(rows[4][1], "77 Lake View\nSuite 2")

    def test_plain_csv_still_works(self):
        self.assertEqual(
            parse("a,b,c\n1,2,3\n"), [["a", "b", "c"], ["1", "2", "3"]]
        )


if __name__ == "__main__":
    unittest.main()

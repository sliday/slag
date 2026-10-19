import unittest

from feedsift import recent_records

# Exact cutoff and timestamps quoted in ISSUE.md.
CUTOFF = "2024-03-01T12:00:00Z"
RECORDS = [
    {"id": "before", "timestamp": "2024-03-01T11:59:59Z"},
    {"id": "at", "timestamp": "2024-03-01T12:00:00Z"},
    {"id": "after", "timestamp": "2024-03-01T12:00:01Z"},
    {"id": "offset-after", "timestamp": "2024-03-01T14:00:01+02:00"},
    {"id": "offset-at", "timestamp": "2024-03-01T14:00:00+02:00"},
    {"id": "offset-before", "timestamp": "2024-03-01T13:59:59+02:00"},
]


def ids(records):
    return [r["id"] for r in records]


class TestRecentRecordsWithOffsets(unittest.TestCase):
    def test_issue_reproduction(self):
        result = recent_records(RECORDS, CUTOFF)
        self.assertEqual(ids(result), ["after", "offset-after"])

    def test_cutoff_exactly_at_record_excludes_it(self):
        result = recent_records(
            [{"id": "x", "timestamp": "2024-03-01T14:00:00+02:00"}], CUTOFF
        )
        self.assertEqual(result, [])

    def test_negative_offset(self):
        # 2024-03-01T07:00:01-05:00 is the same instant as 12:00:01Z.
        result = recent_records(
            [
                {"id": "a", "timestamp": "2024-03-01T07:00:01-05:00"},
                {"id": "b", "timestamp": "2024-03-01T07:00:00-05:00"},
            ],
            CUTOFF,
        )
        self.assertEqual(ids(result), ["a"])

    def test_naive_timestamps_still_work(self):
        records = [
            {"id": 1, "timestamp": "2024-03-01T10:00:00"},
            {"id": 2, "timestamp": "2024-03-01T13:00:00"},
        ]
        result = recent_records(records, "2024-03-01T12:00:00")
        self.assertEqual(ids(result), [2])


if __name__ == "__main__":
    unittest.main()

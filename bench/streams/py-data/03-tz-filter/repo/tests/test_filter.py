import unittest

from feedsift import recent_records

RECORDS = [
    {"id": 1, "timestamp": "2024-03-01T10:00:00"},
    {"id": 2, "timestamp": "2024-03-01T12:00:00"},
    {"id": 3, "timestamp": "2024-03-01T13:00:00"},
]


class TestRecentRecords(unittest.TestCase):
    def test_keeps_newer_records(self):
        result = recent_records(RECORDS, "2024-03-01T12:00:00")
        self.assertEqual([r["id"] for r in result], [3])

    def test_keeps_input_order(self):
        result = recent_records(RECORDS, "2024-03-01T09:00:00")
        self.assertEqual([r["id"] for r in result], [1, 2, 3])

    def test_nothing_newer_returns_empty(self):
        self.assertEqual(recent_records(RECORDS, "2024-03-02T00:00:00"), [])

    def test_empty_input(self):
        self.assertEqual(recent_records([], "2024-03-01T00:00:00"), [])


if __name__ == "__main__":
    unittest.main()

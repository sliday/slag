import unittest

from nameset import initials, sort_key, unique_names


class TestUniqueNames(unittest.TestCase):
    def test_removes_exact_duplicates(self):
        self.assertEqual(unique_names(["Ana", "Bob", "Ana"]), ["Ana", "Bob"])

    def test_keeps_first_occurrence_order(self):
        self.assertEqual(
            unique_names(["Zoe", "Ana", "Max", "Ana"]), ["Zoe", "Ana", "Max"]
        )

    def test_no_duplicates(self):
        self.assertEqual(unique_names(["Ana", "Bob"]), ["Ana", "Bob"])

    def test_empty(self):
        self.assertEqual(unique_names([]), [])

    def test_distinct_names_stay(self):
        self.assertEqual(unique_names(["cafe", "cafeteria"]), ["cafe", "cafeteria"])


class TestDisplay(unittest.TestCase):
    def test_initials(self):
        self.assertEqual(initials("Ada Lovelace"), "AL")
        self.assertEqual(initials("Grace"), "G")

    def test_sort_key(self):
        self.assertEqual(sort_key("Bob"), "bob")
        self.assertEqual(sorted(["bob", "Ana"], key=sort_key), ["Ana", "bob"])


if __name__ == "__main__":
    unittest.main()

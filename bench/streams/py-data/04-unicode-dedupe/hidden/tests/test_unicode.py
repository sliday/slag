import unicodedata
import unittest

from nameset import unique_names

# Exact strings quoted in ISSUE.md: NFC and NFD forms of the same name.
NFC = "caf\u00e9"
NFD = "cafe\u0301"


class TestUniqueNamesUnicode(unittest.TestCase):
    def test_nfc_and_nfd_dedupe_to_first_occurrence(self):
        result = unique_names([NFC, NFD])
        self.assertEqual(result, [NFC])

    def test_reversed_order_still_dedupes(self):
        result = unique_names([NFD, NFC])
        self.assertEqual(len(result), 1)

    def test_mixed_list(self):
        result = unique_names(["bob", NFC, "alice", NFD, "bob"])
        self.assertEqual(result, ["bob", NFC, "alice"])

    def test_sanity_check_forms_differ_as_strings(self):
        # Guards the test data itself: the two forms must differ as
        # plain strings but agree once normalized.
        self.assertNotEqual(NFC, NFD)
        self.assertEqual(unicodedata.normalize("NFC", NFD), NFC)

    def test_exact_duplicates_still_dedupe(self):
        self.assertEqual(unique_names(["Ana", "Bob", "Ana"]), ["Ana", "Bob"])


if __name__ == "__main__":
    unittest.main()

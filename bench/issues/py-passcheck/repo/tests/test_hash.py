import unittest

from passcheck.core import hash_password, strength_score


class TestHashPassword(unittest.TestCase):
    def test_deterministic(self):
        self.assertEqual(
            hash_password("hunter2", "NaCl"), hash_password("hunter2", "NaCl")
        )

    def test_salt_changes_digest(self):
        self.assertNotEqual(
            hash_password("hunter2", "NaCl"), hash_password("hunter2", "KCl")
        )

    def test_hex_digest_length(self):
        self.assertEqual(len(hash_password("hunter2", "NaCl")), 64)


class TestStrengthScore(unittest.TestCase):
    def test_empty_scores_zero(self):
        self.assertEqual(strength_score(""), 0)

    def test_full_marks(self):
        self.assertEqual(strength_score("Abcdefg1!"), 4)

    def test_score_within_range(self):
        for pw in ("a", "abcdefgh", "Abcdefgh", "Abcdefg1", "Abcdefg1!"):
            self.assertTrue(0 <= strength_score(pw) <= 4)


if __name__ == "__main__":
    unittest.main()

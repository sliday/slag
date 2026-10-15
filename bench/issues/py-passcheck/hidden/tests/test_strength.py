import unittest

from passcheck.core import is_strong


class TestIsStrongPolicy(unittest.TestCase):
    def test_meeting_all_rules_is_strong(self):
        self.assertTrue(is_strong("Abcdef12"))
        self.assertTrue(is_strong("Xy9zzzzz"))
        self.assertTrue(is_strong("P@ssw0rd"))

    def test_no_uppercase_is_weak(self):
        self.assertFalse(is_strong("abcdef12"))

    def test_no_digit_is_weak(self):
        self.assertFalse(is_strong("Abcdefgh"))

    def test_all_lowercase_no_digit_is_weak(self):
        self.assertFalse(is_strong("sunshine"))

    def test_short_is_weak_even_with_upper_and_digit(self):
        self.assertFalse(is_strong("Ab1"))

    def test_exactly_min_length_with_all_rules_is_strong(self):
        self.assertTrue(is_strong("Abcdefg1"))


if __name__ == "__main__":
    unittest.main()

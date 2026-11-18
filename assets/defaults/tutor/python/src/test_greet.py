import unittest

from greet import hello


class GreetTest(unittest.TestCase):
    def test_hello_greets_by_name(self):
        self.assertEqual(hello("Dusty"), "Hello, Dusty!")


if __name__ == "__main__":
    unittest.main()

import unittest
from validate_release import validate


class ReleasePolicyTests(unittest.TestCase):
    def test_channel_cannot_regress(self):
        with self.assertRaises(ValueError):
            validate('v0.2.0', [{'tagName': 'v0.3.0', 'isDraft': False}])
        validate('v0.3.0', [{'tagName': 'v0.3.0', 'isDraft': False}])
        validate('v0.2.0', [{'tagName': 'v0.3.0', 'isDraft': True}])

    def test_stable_channel_rejects_prerelease(self):
        with self.assertRaises(ValueError):
            validate('v0.2.0-beta.1', [])

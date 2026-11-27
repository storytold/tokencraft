"""Verify the release smoke check withholds the project key until readiness passes."""

from contextlib import redirect_stderr
from http.client import HTTPMessage
from io import StringIO
import os
import sys
import unittest
from unittest.mock import patch

import smoke_authenticated_gateway as smoke


REVISION = "a" * 40


def readiness(*, revision=REVISION, cache_control="no-store", age=None):
    headers = HTTPMessage()
    headers["TokenCraft-Source-Revision"] = revision
    if cache_control is not None:
        headers["Cache-Control"] = cache_control
    if age is not None:
        headers["Age"] = age
    return {"ready": True}, headers


class RevisionPreflightTest(unittest.TestCase):
    def test_an_unverified_peer_stops_the_check_before_a_key_is_used(self):
        peers = (readiness(revision="b" * 40), readiness(age="19"), readiness(cache_control=None))
        args = ["smoke_authenticated_gateway.py", "--api", "https://api.example.test",
                "--peer", "https://console.example.test", "--revision", REVISION, "--request-id", "req_Fixture"]
        with (
            patch.object(sys, "argv", args),
            patch.dict(os.environ, TOKENCRAFT_SMOKE_API_KEY="synthetic-fixture-token"),
            patch.object(smoke.ssl, "create_default_context"),
            patch.object(smoke, "Gateway") as gateway,
            patch.object(smoke, "verify_saved_receipt") as verify_receipt,
            redirect_stderr(StringIO()),
        ):
            for peer in peers:
                with self.subTest(headers=peer[1].items()):
                    gateway.reset_mock()
                    gateway.return_value.json.side_effect = [readiness(), peer]
                    with self.assertRaises(SystemExit) as failure:
                        smoke.main()
                    self.assertEqual(failure.exception.code, 1)
                    self.assertEqual([call.args[1] for call in gateway.call_args_list], [None, None])
                    verify_receipt.assert_not_called()

    def test_a_fresh_revision_can_be_served_without_an_age_or_at_age_zero(self):
        with patch.object(smoke, "Gateway") as gateway:
            gateway.return_value.json.side_effect = [readiness(), readiness(age="0")]
            address = smoke.origin("https://api.example.test")
            smoke.verify_revision(address, None, REVISION)
            smoke.verify_revision(address, None, REVISION)
            self.assertEqual([call.args[1] for call in gateway.call_args_list], [None, None])


if __name__ == "__main__":
    unittest.main()

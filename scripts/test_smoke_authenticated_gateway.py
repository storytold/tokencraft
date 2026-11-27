"""Verify the release smoke check withholds the project key until readiness passes."""

from contextlib import redirect_stderr
from http.client import HTTPMessage
from io import StringIO
import os
import sys
import unittest
from unittest.mock import Mock, patch

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


class ScopedResponseFreshnessTest(unittest.TestCase):
    def test_versioned_check_rejects_mismatched_and_ambiguous_gateway_responses(self):
        gateway = smoke.Gateway(smoke.origin("http://localhost:4000"), "synthetic-fixture-token", None,
                                private_api=True, revision=REVISION)
        for status in (200, 401):
            for revisions in ((), ("b" * 40,), (REVISION, REVISION)):
                with self.subTest(status=status, revisions=revisions):
                    headers = HTTPMessage()
                    headers["Cache-Control"] = "private, no-store"
                    for revision in revisions:
                        headers["TokenCraft-Source-Revision"] = revision
                    response = Mock(status=status, headers=headers)
                    with patch.object(smoke, "HTTPConnection") as connection:
                        connection.return_value.getresponse.return_value = response
                        with self.assertRaisesRegex(RuntimeError, "expected gateway source revision"):
                            gateway.request("/v1/usage", status)
                        response.read.assert_not_called()
                        connection.return_value.close.assert_called_once()

    def test_rejects_aged_or_ambiguous_scoped_responses_before_reading_the_body(self):
        path = "/v1/requests/req_Fixture"
        gateway = smoke.Gateway(smoke.origin("http://localhost:4000"), "synthetic-fixture-token", None,
                                private_api=True)
        for status in (200, 401):
            for ages in (("19",), ("-1",), ("0", "0")):
                with self.subTest(status=status, ages=ages):
                    headers = HTTPMessage()
                    headers["Cache-Control"] = "private, no-store"
                    for age in ages:
                        headers["Age"] = age
                    response = Mock(status=status, headers=headers)
                    response.read.return_value = b"{}"
                    with patch.object(smoke, "HTTPConnection") as connection:
                        connection.return_value.getresponse.return_value = response
                        with self.assertRaisesRegex(RuntimeError, "fresh, private, non-storable"):
                            gateway.request(path, status)
                        response.read.assert_not_called()
                        connection.return_value.close.assert_called_once()

    def test_accepts_private_responses_with_age_zero_or_no_age(self):
        gateway = smoke.Gateway(smoke.origin("http://localhost:4000"), None, None, private_api=True)
        for age in (None, "0"):
            with self.subTest(age=age):
                headers = HTTPMessage()
                headers["Cache-Control"] = "private, no-store"
                if age is not None:
                    headers["Age"] = age
                response = Mock(status=200, headers=headers)
                response.read.return_value = b"{}"
                with patch.object(smoke, "HTTPConnection") as connection:
                    connection.return_value.getresponse.return_value = response
                    self.assertEqual(gateway.request("/v1/usage"), (b"{}", headers))


if __name__ == "__main__":
    unittest.main()

"""Verify one authenticated image request against the bundled TokenCraft connector."""

import argparse
from hashlib import sha256
from http.client import HTTPConnection, HTTPSConnection
import json
import os
import re
from secrets import token_hex
import ssl
from time import monotonic, sleep
from urllib.parse import urlsplit


OFFER = "example_image"
UPSTREAM_MODEL = "example-image"
MAX_RESPONSE = 2 * 1024 * 1024


def origin(value):
    try:
        parsed = urlsplit(value)
        parsed.port
    except ValueError as error:
        raise argparse.ArgumentTypeError("use a valid gateway origin") from error
    loopback = parsed.scheme == "http" and parsed.hostname in {"localhost", "127.0.0.1", "::1"}
    if ((parsed.scheme != "https" and not loopback) or not parsed.hostname or parsed.username is not None
            or parsed.path not in {"", "/"} or parsed.query or parsed.fragment):
        raise argparse.ArgumentTypeError("use an HTTPS origin or a loopback HTTP origin")
    return parsed


class Gateway:
    def __init__(self, address, key, tls):
        self.address = address
        self.key = key
        self.tls = tls

    def request(self, path, expected=200, *, body=None, idempotency_key=None):
        host, port = self.address.hostname, self.address.port
        if self.address.scheme == "https":
            connection = HTTPSConnection(host, port, timeout=8, context=self.tls)
        else:
            connection = HTTPConnection(host, port, timeout=8)
        headers = {"Authorization": f"Bearer {self.key}"}
        if body is not None:
            headers["Content-Type"] = "application/json"
            body = json.dumps(body).encode()
        if idempotency_key:
            headers["Idempotency-Key"] = idempotency_key
        try:
            connection.request("GET" if body is None else "POST", path, body, headers)
            response = connection.getresponse()
            if response.status != expected:
                raise RuntimeError(f"{path.split('?')[0]} returned HTTP {response.status}, expected {expected}")
            raw = response.read(MAX_RESPONSE + 1)
            if len(raw) > MAX_RESPONSE:
                raise RuntimeError("Gateway response exceeded the smoke check size limit")
            return raw, response.headers
        finally:
            connection.close()

    def json(self, path, *args, **kwargs):
        data, headers = self.request(path, *args, **kwargs)
        require(headers.get_content_type() == "application/json", f"{path.split('?')[0]} did not return JSON")
        return json.loads(data), headers


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def admit(first, peer, run_id):
    query = f"?app_ref={run_id}"
    usage, _ = peer.json("/v1/usage" + query)
    require(usage["totals"]["requests"] == 0, "The usage filter did not isolate the new smoke run")
    catalog, _ = first.json("/v1/catalog/offers")
    require(any((offer["id"], offer["provider"], offer["adapter"], offer.get("upstream_model"))
                == (OFFER, "self_hosted", "connector_queue", UPSTREAM_MODEL)
                for offer in catalog["offers"]), "The gateway must expose the bundled image connector")
    quote, _ = first.json(f"/v1/quotes/scoped?pinned_offer_id={OFFER}", body={
        "model": OFFER,
        "provider": "self_hosted",
        "max_total_usd_nanos": 0,
        "workload": {"kind": "image", "count": 1, "aspect_ratio": "4:3"},
    })
    require(quote["selected_offer_id"] == quote["pinned_offer_id"] == OFFER, "The bundled offer is unavailable")
    require(quote["input"]["max_total_usd_nanos"] == 0 and any(
        candidate["offer_id"] == OFFER and candidate["provider"] == "self_hosted"
        and candidate["estimated_usd_nanos"] == 0 for candidate in quote["candidates"]
    ), "The bundled offer must execute at zero estimated cost")
    submission = {
        "quote_id": quote["id"], "pinned_offer_id": OFFER, "input": quote["input"],
        "app_ref": run_id, "image_prompt": "Synthetic TokenCraft release check",
    }
    receipt, headers = first.json("/v1/requests", 202, body=submission, idempotency_key=run_id)
    request_id = receipt["id"]
    require(re.fullmatch(r"req_[A-Za-z0-9]+", request_id), "Unexpected request ID")
    path = f"/v1/requests/{request_id}"
    require(receipt["status_url"] == path, "Unexpected receipt path")
    print(f"Accepted {request_id}", flush=True)
    replay, replay_headers = peer.json("/v1/requests", 200, body=submission, idempotency_key=run_id)
    trace = headers.get("TokenCraft-Trace-Id")
    require(trace and trace == replay_headers.get("TokenCraft-Trace-Id") and replay == receipt,
            "The replay did not retain the accepted receipt and trace ID")
    return request_id, path, trace


def wait_for_result(gateway, path):
    deadline = monotonic() + 40
    while monotonic() < deadline:
        state, _ = gateway.json(path)
        if state["status"] == "succeeded":
            return
        require(state["status"] in {"queued", "running"}, "The smoke request reached an unsuccessful terminal state")
        sleep(0.3)
    raise RuntimeError("The smoke request did not complete before the deadline")


def verify_artifact(gateway, path):
    result, _ = gateway.json(path + "/result")
    (image,) = result["result"]["images"]
    artifact_path = path + "/artifacts/0"
    require(image["url"] == artifact_path, "Unexpected artifact path")
    data, headers = gateway.request(artifact_path)
    require(headers.get_content_type() == "image/png" and data.startswith(b"\x89PNG\r\n\x1a\n"),
            "The result is not a PNG")
    require(sha256(data).hexdigest() == image["artifact"]["sha256"], "The artifact digest did not match")


def verify_evidence(gateway, request_id, path, run_id, trace_id):
    trace, _ = gateway.json(path + "/trace")
    require(trace["correlation"]["trace_id"] == trace_id, "The trace did not retain the receipt correlation ID")
    route = trace["provider_path"]
    attempts = [
        (row["offer_id"], row["provider"], row["state"], row["adapter"], row["requested_model"])
        for row in route["attempts"]
    ]
    require(route["admitted_offer_id"] == OFFER
            and attempts == [(OFFER, "self_hosted", "succeeded", "connector_queue", UPSTREAM_MODEL)],
            "The trace did not record one successful bundled connector attempt")
    query = f"?app_ref={run_id}"
    history, _ = gateway.json("/v1/requests" + query)
    require([row["id"] for row in history["requests"]] == [request_id], "History did not contain exactly one request")
    usage, _ = gateway.json("/v1/usage" + query)
    totals = usage["totals"]
    require(usage["app_ref"] == run_id and totals["estimated_usd_nanos"] == "0"
            and all(totals[name] == 1 for name in ("requests", "succeeded", "completed_outputs", "provider_requests")),
            "Usage did not record one successful, zero-cost request and one output")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", required=True, type=origin)
    parser.add_argument("--peer", type=origin, help="Second trusted origin for replay and reads")
    parser.add_argument("--ca", help="PEM certificate bundle for a private HTTPS ingress")
    args = parser.parse_args()
    key = os.environ.get("TOKENCRAFT_SMOKE_API_KEY", "")
    if not re.fullmatch(r"[A-Za-z0-9._~-]{16,256}", key):
        parser.error("set TOKENCRAFT_SMOKE_API_KEY to a scoped project API key")
    run_id = "release-smoke-" + token_hex(12)
    try:
        tls = ssl.create_default_context(cafile=args.ca)
        first = Gateway(args.api, key, tls)
        peer = Gateway(args.peer, key, tls) if args.peer else first
        request_id, path, trace_id = admit(first, peer, run_id)
        wait_for_result(peer, path)
        verify_artifact(peer, path)
        verify_evidence(peer, request_id, path, run_id, trace_id)
    except (OSError, RuntimeError, ValueError, KeyError) as error:
        parser.exit(1, f"Smoke check failed: {error}\n")
    print("Receipt replay, PNG digest, connector trace, request history, and usage passed.")


if __name__ == "__main__":
    main()

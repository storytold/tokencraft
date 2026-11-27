"""Verify a saved TokenCraft receipt or run one bundled image request."""

import argparse
from datetime import datetime, timedelta, timezone
from hashlib import sha256
from http.client import HTTPConnection, HTTPSConnection
import json
import os
import re
from secrets import token_hex
import ssl
from time import monotonic, sleep
from urllib.parse import urlencode, urlsplit


OFFER = "example_image"
UPSTREAM_MODEL = "example-image"
MAX_RESPONSE = 2 * 1024 * 1024
MEDIA_TYPES = {"images": "image/png", "video": "video/mp4", "audio": "audio/wav", "mesh": "model/gltf-binary"}


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
    def __init__(self, address, key, tls, *, private_api=False, revision=None):
        self.address = address
        self.key = key
        self.tls = tls
        self.private_api = private_api
        self.revision = revision

    def request(self, path, expected=200, *, body=None, idempotency_key=None, max_bytes=MAX_RESPONSE):
        host, port = self.address.hostname, self.address.port
        if self.address.scheme == "https":
            connection = HTTPSConnection(host, port, timeout=8, context=self.tls)
        else:
            connection = HTTPConnection(host, port, timeout=8)
        headers = {}
        if self.key is not None:
            headers["Authorization"] = f"Bearer {self.key}"
        if body is not None:
            headers["Content-Type"] = "application/json"
            body = json.dumps(body).encode()
        if idempotency_key:
            headers["Idempotency-Key"] = idempotency_key
        try:
            connection.request("GET" if body is None else "POST", path, body, headers)
            response = connection.getresponse()
            route = path.split("?")[0]
            if response.status != expected:
                raise RuntimeError(f"{route} returned HTTP {response.status}, expected {expected}")
            if self.revision is not None:
                require(response.headers.get_all("TokenCraft-Source-Revision") == [self.revision],
                        f"{self.address.netloc}{route} did not report the expected gateway source revision")
            if self.private_api and path.startswith("/v1/"):
                require(fresh_no_store(response.headers, private=True),
                        f"{route} did not return a fresh, private, non-storable response")
            raw = response.read(max_bytes + 1)
            if len(raw) > max_bytes:
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


def fresh_no_store(headers, *, private=False):
    policies = headers.get_all("Cache-Control", [])
    if len(policies) != 1:
        return False
    directives = {part.strip().lower() for part in policies[0].split(",")}
    required = {"private", "no-store"} if private else {"no-store"}
    ages = headers.get_all("Age", [])
    return (required <= directives and len(ages) <= 1
            and all(re.fullmatch(r"0+", age.strip()) for age in ages))


def verify_revision(address, tls, revision):
    state, headers = Gateway(address, None, tls).json("/ready")
    require(
        isinstance(state, dict) and state.get("ready") is True
        and headers.get_all("TokenCraft-Source-Revision") == [revision],
        f"{address.netloc} did not report the expected gateway source revision",
    )
    require(
        fresh_no_store(headers),
        f"{address.netloc} did not return a fresh, non-storable readiness response",
    )


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


def read_artifact(gateway, path, output, slot, media_type):
    artifact_path = f"{path}/artifacts/{slot}"
    require(output["url"] == artifact_path and output["content_type"] == media_type,
            "Unexpected artifact path or media type")
    artifact = output["artifact"]
    limit = MAX_RESPONSE if media_type == "image/png" else 8 * 1024 * 1024
    size = artifact["byte_length"]
    require(artifact["slot"] == slot and isinstance(size, int) and 0 < size <= limit,
            "Invalid artifact slot or size")
    data, headers = gateway.request(artifact_path, max_bytes=size)
    require(headers.get_content_type() == media_type and len(data) == size,
            "The artifact response does not match its recorded media type and size")
    digest = artifact["sha256"]
    require(headers.get_all("X-TokenCraft-Content-Sha256") == [digest],
            "The artifact integrity header did not match the stored digest")
    require(sha256(data).hexdigest() == digest, "The artifact digest did not match")
    return data


def verify_artifact(gateway, path):
    result, _ = gateway.json(path + "/result")
    (image,) = result["result"]["images"]
    data = read_artifact(gateway, path, image, 0, MEDIA_TYPES["images"])
    require(data.startswith(b"\x89PNG\r\n\x1a\n"), "The result is not a PNG")


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


def verify_saved_result(gateway, path):
    document, _ = gateway.json(path + "/result")
    result = document["result"]
    artifacts = []
    if "text" in result:
        require(isinstance(result["text"], str), "The saved chat result does not contain text")
    else:
        kinds = [kind for kind in MEDIA_TYPES if kind in result]
        require(len(kinds) == 1, "The saved result has an unknown media format")
        kind = kinds[0]
        outputs = result[kind] if kind == "images" else [result[kind]]
        require(isinstance(outputs, list) and 1 <= len(outputs) <= 8, "The saved result has an invalid output count")
        for slot, output in enumerate(outputs):
            read_artifact(gateway, path, output, slot, MEDIA_TYPES[kind])
            artifacts.append(f"{path}/artifacts/{slot}")
    fingerprint = sha256(json.dumps(document, sort_keys=True).encode()).hexdigest()
    return fingerprint, artifacts


def verify_saved_receipt(gateway, request_id):
    path = f"/v1/requests/{request_id}"
    state, _ = gateway.json(path)
    require(state["id"] == request_id and state["status"] == "succeeded",
            "Choose an existing successful request with an available provider result")
    trace_id = (state.get("correlation") or {}).get("trace_id")
    require(isinstance(trace_id, str) and re.fullmatch(r"[0-9a-f]{32}", trace_id),
            "The saved request has no gateway trace ID")
    trace_path = path + "/trace"
    trace, _ = gateway.json(trace_path)
    require(trace["correlation"]["trace_id"] == trace_id
            and trace["provider_path"]["admitted_offer_id"] == state["quote"]["selected_offer_id"],
            "The saved trace does not match the request and route")
    history_path = "/v1/requests?" + urlencode({"trace_id": trace_id, "status": "succeeded"})
    history, _ = gateway.json(history_path)
    require([item["id"] for item in history["requests"]] == [request_id],
            "Scoped history did not return the saved request")

    accepted = next((event["at"] for event in state["events"] if event["kind"] == "accepted"), None)
    require(accepted is not None, "The saved request has no admission timestamp")
    day = datetime.fromisoformat(accepted).astimezone(timezone.utc).date()
    window = {"from": day.isoformat(), "to": (day + timedelta(days=1)).isoformat()}
    scope = {field: state[field] for field in ("app_ref", "client_ref") if state.get(field) is not None}
    usage_path = "/v1/usage?" + urlencode({**window, **scope})
    usage, _ = gateway.json(usage_path)
    require(usage["project_id"] == state["project_id"] and usage["window"] == window
            and all(usage.get(field) == value for field, value in scope.items()),
            "Usage did not preserve the saved request scope and date filters")
    require(usage["totals"]["succeeded"] >= 1, "Usage did not include successful work in the saved request scope")
    fingerprint, artifacts = verify_saved_result(gateway, path)

    anonymous = Gateway(gateway.address, None, gateway.tls, private_api=True, revision=gateway.revision)
    for route in (path, trace_path, path + "/result", history_path, usage_path, *artifacts):
        error, _ = anonymous.json(route, 401)
        require(error["error"]["code"] == "unauthorized", "An anonymous read did not return a gateway denial")
    return state["project_id"], trace_id, fingerprint


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", required=True, type=origin)
    parser.add_argument("--peer", type=origin, help="Second trusted origin for replay or read-only verification")
    parser.add_argument("--request-id", help="Verify an existing successful receipt using only GET requests")
    parser.add_argument("--ca", help="PEM certificate bundle for a private HTTPS ingress")
    parser.add_argument("--revision", help="Verify this Git SHA before sending a scoped key and on every response")
    args = parser.parse_args()
    if args.revision is not None and not re.fullmatch(r"[0-9a-f]{40}", args.revision):
        parser.error("--revision must be a full lowercase Git SHA")
    if args.request_id is not None and not re.fullmatch(r"req_[A-Za-z0-9]{1,124}", args.request_id):
        parser.error("--request-id must be a native TokenCraft request ID")
    key = os.environ.get("TOKENCRAFT_SMOKE_API_KEY", "")
    if not re.fullmatch(r"[A-Za-z0-9._~-]{16,256}", key):
        parser.error("set TOKENCRAFT_SMOKE_API_KEY to a scoped project API key")
    try:
        tls = ssl.create_default_context(cafile=args.ca)
        if args.revision is not None:
            verify_revision(args.api, tls, args.revision)
            if args.peer is not None and args.peer != args.api:
                verify_revision(args.peer, tls, args.revision)
        private_api = args.revision is not None or args.request_id is not None
        first = Gateway(args.api, key, tls, private_api=private_api, revision=args.revision)
        peer = first
        if args.peer and args.peer != args.api:
            peer = Gateway(args.peer, key, tls, private_api=private_api, revision=args.revision)
        if args.request_id is not None:
            found = verify_saved_receipt(first, args.request_id)
            if peer is not first:
                require(found == verify_saved_receipt(peer, args.request_id),
                        "The origins returned different saved receipts or results")
            print("Saved receipt, result, trace, scoped history, usage, and anonymous denials passed.")
            return
        run_id = "release-smoke-" + token_hex(12)
        request_id, path, trace_id = admit(first, peer, run_id)
        wait_for_result(peer, path)
        verify_artifact(peer, path)
        verify_evidence(peer, request_id, path, run_id, trace_id)
    except (OSError, RuntimeError, ValueError, KeyError, TypeError, AttributeError) as error:
        parser.exit(1, f"Smoke check failed: {error}\n")
    print("Receipt replay, PNG digest, connector trace, request history, and usage passed.")


if __name__ == "__main__":
    main()

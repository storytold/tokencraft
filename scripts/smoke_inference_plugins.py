"""Route requests through the released inference plugins and synthetic loopback vendors."""

import argparse
from contextlib import ExitStack
from hashlib import sha256
import json
from pathlib import Path
from secrets import token_hex
import struct
from tempfile import TemporaryDirectory
from urllib.parse import urlsplit
import wave

from smoke_connector_recovery import fixture_png, wait_for_status
from smoke_workflow_contract import (
    CALLER_CONTEXT, ROOT, call, command, loopback_port, ready, rehearse, service,
)


PLUGINS = ROOT / "plugins/providers/inference"
WORKLOAD = {"kind": "video", "duration_seconds": 5, "aspect_ratio": "16:9", "resolution": "720p"}


def make_media_fixtures(directory, env):
    command("ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi", "-i", "color=black:s=1280x720:r=2:d=5",
            "-an", "-c:v", "mpeg4", "-threads", "1", "fixture.mp4", cwd=directory, env=env)
    (directory / "fixture.png").write_bytes(fixture_png((35, 95, 210), 1024, 1024))
    with wave.open(str(directory / "fixture.wav"), "wb") as audio:
        audio.setparams((1, 2, 8_000, 0, "NONE", "not compressed"))
        audio.writeframes(bytes(5 * 8_000 * 2))
    positions = struct.pack("<9f", 0, 0, 0, 1, 0, 0, 0, 1, 0)
    document = {
        "asset": {"version": "2.0"},
        "buffers": [{"byteLength": len(positions)}],
        "bufferViews": [{"buffer": 0, "byteLength": len(positions), "target": 34962}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                       "min": [0, 0, 0], "max": [1, 1, 0]}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "mode": 4}]}],
        "nodes": [{"mesh": 0}], "scenes": [{"nodes": [0]}], "scene": 0,
    }
    encoded = json.dumps(document, separators=(",", ":")).encode()
    encoded += b" " * (-len(encoded) % 4)
    body = struct.pack("<I4s", len(encoded), b"JSON") + encoded
    body += struct.pack("<I4s", len(positions), b"BIN\0") + positions
    mesh = struct.pack("<4sII", b"glTF", 2, len(body) + 12) + body
    (directory / "fixture.glb").write_bytes(mesh)
    (directory / "broken.glb").write_bytes(mesh[:-1])


def plugin_config(name, file):
    return json.loads((PLUGINS / name / file).read_bytes())


def configure(directory, settings, sites):
    def write(file, data):
        output = directory / file
        output.write_text(json.dumps(data))
        return str(output)

    bindings = []
    offers = []
    for name, offer_id, rate in (("fal", "fal_pixverse_v6", 20_000_000), ("kinovi", "kinovi_sample", 10_000_000)):
        catalog = plugin_config(name, "catalog.example.json")["offers"]
        offer = next(row for row in catalog if row["id"] == offer_id)
        offer["model"] = "fixture_video"
        offer["pricing"]["usd_nanos_per_second"] = rate
        offers.append(offer)
        if name == "fal":
            offers.extend(row for row in catalog if row["id"] in {"fal_sample", "fal_stable_audio_25", "fal_meshy_v6_preview"})
        binding, = plugin_config(name, "bindings.example.json")["bindings"]
        bindings.append({**binding, "origin": sites[name] + "/"})
    text, = plugin_config("openrouter", "catalog.example.json")["offers"]
    chat, = plugin_config("openrouter", "bindings.example.json")["bindings"]
    chat["endpoint"] = sites["openrouter"] + "/v1/chat/completions"
    return dict(settings, TOKENCRAFT_CORE_CATALOG=write("catalog.json", {"version": 1, "offers": [*offers, text]}),
                TOKENCRAFT_CONNECTORS=write("bindings.json", {"version": 1, "bindings": bindings}),
                TOKENCRAFT_LOCAL_CHAT_BINDINGS=write("chat.json", {"version": 1, "bindings": [chat]}))


def verify_fal_media(port, directory, shared, other_client):
    def submit(path, body, key, prompt="Fixture media"):
        return call(port, path, 202, method="POST",
                    headers={**shared, **CALLER_CONTEXT, "x-tokencraft-client-ref": "studio_a", "Idempotency-Key": key},
                    body={**body, "prompt": prompt})[0]

    mesh_request = {"model": "meshy_v6_preview", "task": "text_to_3d"}
    media = [
        ("/v1/image", {"model": "fal_sample", "task": "text_to_image"}, "fixture.png", "image/png",
         "images", {"width": 1024, "height": 1024}),
        ("/v1/audio", {"model": "stable_audio_25", "task": "text_to_music", "duration_seconds": 5},
         "fixture.wav", "audio/wav", "audio", {"duration_ms": 5000, "sample_rate_hz": 8000, "channels": 1}),
        ("/v1/3d", mesh_request, "fixture.glb", "model/gltf-binary",
         "mesh", {"mesh_count": 1, "primitive_count": 1}),
    ]
    pending = [(submit(path, body, "inference-" + field), filename, content_type, field, metadata)
               for path, body, filename, content_type, field, metadata in media]
    for receipt, filename, content_type, field, metadata in pending:
        wait_for_status(port, receipt, shared, "succeeded", 15)
        result, _ = call(port, receipt["status_url"] + "/result", 200, headers=shared)
        output = result["result"][field]
        if field == "images":
            output, = output
        expected = (directory / filename).read_bytes()
        assert {name: output[name] for name in metadata} == metadata
        assert output["content_type"] == content_type
        assert output["artifact"] == {"slot": 0, "byte_length": len(expected), "sha256": sha256(expected).hexdigest()}
        body, headers = call(port, output["url"], 200, headers=shared)
        assert body == expected and headers.get_content_type() == content_type
        assert headers["X-TokenCraft-Content-Sha256"] == output["artifact"]["sha256"]
        call(port, output["url"], 404, headers=other_client)
        trace, _ = call(port, f"/v1/requests/{receipt['id']}/trace", 200, headers=shared)
        attempts = trace["evidence"]["routing"]["attempts"]
        assert [(attempt["provider"], attempt["state"]) for attempt in attempts] == [("fal", "succeeded")]
    print("PASS fal published scoped PNG, PCM WAV, and embedded GLB with the exact bytes and metadata")

    corrupt = submit("/v1/3d", mesh_request, "inference-broken-mesh", prompt="Corrupt mesh fixture")
    request = f"/v1/requests/{corrupt['id']}"
    failed = wait_for_status(port, {"status_url": request}, shared, "failed", 15)
    upstream = failed["upstream"]
    assert upstream["error_code"] == "provider_artifact_invalid"
    assert upstream["queue_state"] == "completed" and upstream["result_observed_at"]
    call(port, request + "/result", 404, headers=shared)
    call(port, request + "/artifacts/0", 404, headers=shared)
    print("PASS corrupted GLB failed validation before any artifact was published")


def verify(gateway, directory, settings, session, account, project):
    port, worker_port = int(settings["API_GATEWAY_PORT"]), loopback_port()
    plugin_env = {key: settings[key] for key in ("PATH", "LANG")}
    tokens = {name: token_hex(32) for name in (
        "TOKENCRAFT_CONNECTOR_TOKEN_FAL", "TOKENCRAFT_CONNECTOR_TOKEN_KINOVI", "TOKENCRAFT_LOCAL_CHAT_KEY_OPENROUTER",
    )}
    make_media_fixtures(directory, plugin_env)
    video = (directory / "fixture.mp4").read_bytes()
    addresses = directory / "addresses.json"
    with ExitStack() as apps:
        apps.enter_context(service("plugins", ["node", ROOT / "scripts/inference_plugin_fixture.mjs"],
                                   directory, {**plugin_env, **tokens}, addresses.is_file))
        sites = json.loads(addresses.read_bytes())
        supplier_port = urlsplit(sites["supplier"]).port
        settings = configure(directory, {**settings, **tokens}, sites)
        worker = dict(settings, API_GATEWAY_PORT=str(worker_port), TOKENCRAFT_PROCESS_ROLE="worker")
        apps.enter_context(service("plugin-api", [gateway], directory, settings, lambda: ready(port)))
        apps.enter_context(service("plugin-worker", [gateway], directory, worker, lambda: ready(worker_port)))

        def create_key(client=None):
            body = {"name": "Inference fixture", "scopes": ["requests:read", "usage:read"]}
            if client is None:
                body["scopes"].append("requests:submit")
            else:
                body["client_ref"] = client
            key, _ = call(port, f"/v1/projects/{project}/service-principals?account_id={account}", 201,
                          headers=session, method="POST", body=body)
            return {"Authorization": "Bearer " + key["token"]}

        shared, client_b = create_key(), create_key("studio_b")

        def quote(client, providers=None):
            body = {"model": "fixture_video", "workload": WORKLOAD}
            if providers is not None:
                body["preferred_providers"] = providers
            return call(port, f"/v1/quotes/scoped?client_ref={client}", 200, method="POST",
                        body=body, headers=shared)[0]

        def submit(quoted, client, key, *, prompt="Synthetic provider selection", expected=202):
            return call(port, "/v1/requests", expected, method="POST",
                        headers={**shared, **CALLER_CONTEXT, "Idempotency-Key": key},
                        body={"quote_id": quoted["id"], "input": quoted["input"],
                              "client_ref": client, "video_prompt": prompt})[0]

        def read_routing(status_url):
            trace, _ = call(port, status_url + "/trace", 200, headers=shared)
            return trace["evidence"]["routing"]

        def check_video(receipt, provider, offer):
            wait_for_status(port, receipt, shared, "succeeded", 15)
            status_url = receipt["status_url"]
            result, _ = call(port, status_url + "/result", 200, headers=shared)
            output = result["result"].get("video") or result["result"]["videos"][0]
            assert call(port, output["url"], 200, headers=shared)[0] == video
            routing = read_routing(status_url)
            assert routing["admitted_offer_id"] == offer
            assert [(entry["provider"], entry["state"]) for entry in routing["attempts"]] == [(provider, "succeeded")]

        lowest = quote("studio_a")
        assert lowest["selected_offer_id"] == "kinovi_sample"
        assert {row["provider"]: row["estimated_usd_nanos"] for row in lowest["candidates"]} == {
            "kinovi": 50_000_000, "fal": 100_000_000,
        }
        first = submit(lowest, "studio_a", "inference-kinovi")
        check_video(first, "kinovi", "kinovi_sample")
        preferred = quote("studio_b", ["fal"])
        assert preferred["selected_offer_id"] == "fal_pixverse_v6"
        second = submit(preferred, "studio_b", "inference-fal")
        check_video(second, "fal", "fal_pixverse_v6")
        assert submit(lowest, "studio_a", "inference-kinovi", expected=200)["id"] == first["id"]
        print("PASS lowest price, provider preference, receipt replay, exact video bytes, and pinned traces")

        page, _ = call(port, "/v1/requests?client_ref=studio_a", 200, headers=shared)
        assert [request["id"] for request in page["requests"]] == [first["id"]]
        call(port, first["status_url"] + "/trace", 404, headers=client_b)
        call(port, second["status_url"] + "/trace", 200, headers=client_b)
        usage, _ = call(port, "/v1/usage?client_ref=studio_a", 200, headers=shared)
        assert usage["totals"]["estimated_usd_nanos"] == "50000000"
        assert usage["totals"]["provider_charges"] == [
            {"provider": "kinovi", "unit": "credit", "quantity": "2.17", "observations": 1},
        ]
        def chat(message, key, *, stream=False):
            return call(port, "/v1/chat/completions", 200, method="POST",
                        headers={**shared, **CALLER_CONTEXT, "Idempotency-Key": key},
                        body={"model": "openrouter_sample", "max_tokens": 32, "stream": stream,
                              "messages": [{"role": "user", "content": message}]})

        reply, headers = chat("Fixture text", "inference-openrouter")
        assert reply["choices"][0]["message"]["content"] == "Synthetic chat result"
        assert (reply["usage"]["prompt_tokens"], reply["usage"]["completion_tokens"]) == (12, 5)
        routing = read_routing(f"/v1/requests/{headers['TokenCraft-Request-Id']}")
        assert routing["admitted_offer_id"] == "openrouter_sample"
        assert routing["downstream"] == {"status": "reported", "provider": "fixture upstream"}
        reply, headers = chat("Unstructured supplier metadata", "inference-openrouter-metadata")
        assert reply["choices"][0]["message"]["content"] == "Synthetic chat result"
        assert "downstream" not in read_routing(f"/v1/requests/{headers['TokenCraft-Request-Id']}")
        stream, headers = chat("Fixture stream", "inference-openrouter-stream", stream=True)
        assert b"Synthetic chat result" in stream and b"data: [DONE]" in stream
        status_url = f"/v1/requests/{headers['TokenCraft-Request-Id']}"
        result = wait_for_status(port, {"status_url": status_url}, shared, "succeeded", 10)
        assert result["upstream"]["reported_provider"] == "fixture upstream"
        assert read_routing(status_url)["downstream"] == routing["downstream"]
        assert call(supplier_port, "/counts", 200)[0] == {"fal": 1, "kinovi": 1, "openrouter": 3, "kinovi_throttles": 0}
        print("PASS client-scoped usage, Kinovi credits, and validated OpenRouter provider lineage and chat usage")

        throttled = submit(quote("studio_b"), "studio_b", "inference-throttle", prompt="Throttle fixture")
        failed = wait_for_status(port, throttled, shared, "failed", 15)
        assert failed["upstream"]["error_code"] == "provider_rate_limited"
        available = quote("studio_b")
        assert available["selected_offer_id"] == "fal_pixverse_v6"
        attempts = read_routing(throttled["status_url"])["attempts"]
        assert [(entry["offer_id"], entry["state"]) for entry in attempts] == [
            ("kinovi_sample", "rate_limited"), ("kinovi_sample", "failed"),
        ]
        check_video(submit(available, "studio_b", "inference-failover"), "fal", "fal_pixverse_v6")
        assert call(supplier_port, "/counts", 200)[0] == {"fal": 2, "kinovi": 1, "openrouter": 3, "kinovi_throttles": 2}
        print("PASS Kinovi retried once before acceptance and a fresh quote routed to fal during cooldown")
        verify_fal_media(port, directory, shared, client_b)
        assert call(supplier_port, "/counts", 200)[0] == {"fal": 6, "kinovi": 1, "openrouter": 3, "kinovi_throttles": 2}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-inference-plugins-") as directory:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(directory), verification=verify)


if __name__ == "__main__":
    main()

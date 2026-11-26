"""Publish and run a trim and narration workflow against two private CPU runners."""

import argparse
from array import array
from contextlib import ExitStack
from hashlib import sha256
import json
from pathlib import Path
from secrets import token_hex
import subprocess
import sys
from tempfile import TemporaryDirectory

from smoke_workflow_contract import HOST, MEDIA, call, command, loopback_port, ready, rehearse, service, wait_for_run


def runner_binding(stack, directory, settings, manifest, operation, revision, account, project):
    profile = directory / f"{operation}.json"
    tool, = (tool for tool in manifest["tools"] if (tool["id"], tool["revision"]) == (operation, revision))
    profile.write_text(json.dumps({**manifest, "tools": [tool]}))
    port = loopback_port()
    token = token_hex(32)
    suffix = operation.removeprefix("video.").replace("_", "-")
    name = f"cpu-{suffix}"
    token_env = "TOKENCRAFT_CONNECTOR_TOKEN_" + name.upper().replace("-", "_")
    settings[token_env] = token
    stack.enter_context(service(name, [
        sys.executable, "-B", MEDIA / "server.py", "--host", HOST, "--port", str(port),
        "--manifest", profile, "--state", directory / name,
    ], directory, {"PATH": settings["PATH"], "TOKENCRAFT_MEDIA_SERVICE_TOKEN": token}, lambda: ready(port)))
    return {
        "name": name, "revision": "v1", "origin": f"http://{HOST}:{port}/", "token_env": token_env,
        "manifest": str(profile), "account": account, "projects": [project],
    }


def upload(port, endpoint, auth, path, mime):
    data = path.read_bytes()
    response, _ = call(port, endpoint, 201, method="POST", body=data, headers={
        **auth, "Content-Type": mime, "X-TokenCraft-Content-Sha256": sha256(data).hexdigest(),
    })
    return {"upload_id": response["upload_id"]}


def verify_media(gateway, directory, settings, session, account, project):
    def ffmpeg(*args):
        return command("ffmpeg", "-nostdin", "-v", "error", *args, cwd=directory, env=settings)

    source = directory / "source.mp4"
    narration = directory / "narration.wav"
    ffmpeg("-f", "lavfi", "-i", "testsrc2=s=96x64:r=24:d=2", "-an", "-g", "48",
           "-c:v", "libx264", "-pix_fmt", "yuv420p", source)
    ffmpeg("-f", "lavfi", "-i", "sine=frequency=880:d=0.25", "-c:a", "pcm_s16le", narration)

    manifest = json.loads((MEDIA / "manifest.json").read_bytes())
    port = int(settings["API_GATEWAY_PORT"])
    query = f"?account_id={account}"
    project_path = f"/v1/projects/{project}"
    with ExitStack() as stack:
        bindings = [
            runner_binding(stack, directory, settings, manifest, operation, revision, account, project)
            for operation, revision in (("video.trim", 1), ("video.attach_audio", 2))
        ]
        registry = directory / "finishing.json"
        registry.write_text(json.dumps({"version": 1, "bindings": bindings}))
        settings["TOKENCRAFT_WORKFLOW_TOOL_BINDINGS"] = str(registry)
        stack.enter_context(service("finishing-api", [gateway], directory, settings, lambda: ready(port)))
        worker_port = loopback_port()
        stack.enter_context(service("finishing-worker", [gateway], directory,
            dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port)),
            lambda: ready(worker_port)))
        operation_pins = {}
        operations = {}
        for binding in bindings:
            reviewed, _ = call(port, project_path + "/workflow-tools" + query + "&verify=" + binding["name"],
                               200, headers=session)
            selected = next(entry for entry in reviewed if entry["name"] == binding["name"])
            assert selected["reachable"] is True
            tool, = selected["manifest"]["tools"]
            operation_pins[tool["id"]] = selected["revision"], tool["revision"]
            operations[tool["id"]] = tool

        def tool_step(id_, title, operation, inputs, parameters, sources):
            binding, revision = operation_pins[operation]
            return {
                "id": id_, "title": title, "kind": "tool", "model": operation, "max_usd_nanos": 0,
                "settings": sources,
                "tool": {"binding_revision": binding, "revision": revision, "inputs": inputs,
                         "output": "video", "parameters": parameters},
            }

        offset_constraint = operations["video.attach_audio"]["parameters"]["properties"]["offset_ms"]
        graph = {
            "schema": 1, "title": "Clip finishing", "max_usd_nanos": 0,
            "inputs": {"video": {"title": "Source video", "kind": "video"},
                       "narration": {"title": "Narration", "kind": "audio"},
                       "delay": {"title": "Narration delay", "kind": "parameter",
                                 "parameter": offset_constraint, "default": 0}},
            "outputs": {"trimmed": "trim", "master": "soundtrack"},
            "nodes": [
                tool_step("trim", "Trim clip", "video.trim", {"video": "video"},
                          {"start_ms": 250, "duration_ms": 1000}, {"video": {"source": "request", "field": "video"}}),
                tool_step("soundtrack", "Replace soundtrack", "video.attach_audio", {"video": "video", "audio": "audio"},
                          {"video_mode": "copy_h264"},
                          {"video": {"source": "step", "step": "trim"},
                           "audio": {"source": "request", "field": "narration"}}),
            ],
        }
        graph["nodes"][1]["tool"]["parameter_inputs"] = {"offset_ms": "delay"}
        draft, _ = call(port, project_path + "/workflows" + query, 201, method="POST", headers=session, body=graph)
        manage = project_path + f"/workflows/{draft['id']}"
        endpoint = project_path + f"/workflow-endpoints/{draft['id']}"
        files = (("video", source, "video/mp4"), ("narration", narration, "audio/wav"))
        draft_inputs = {key: upload(port, endpoint + "/uploads" + query, session, file, mime)
                        for key, file, mime in files}
        trial, _ = call(port, manage + "/test-runs" + query, 202, method="POST",
                        headers={**session, "Idempotency-Key": token_hex(16)},
                        body={"revision": draft["revision"], "inputs": draft_inputs})
        wait_for_run(port, trial, session)
        call(port, manage + "/versions" + query, 200, method="POST", headers=session,
             body={"revision": draft["revision"], "active_version": None})
        app, _ = call(port, manage + "/apps" + query, 201, method="POST", headers=session, body={
            "name": "Clip finishing fixture", "days": 1, "scopes": ["runs:submit", "runs:read", "outputs:read"],
        })
        auth = {"Authorization": "Bearer " + app["secret"]}
        contract, _ = call(port, endpoint + "/openapi.json", 200, headers=auth)
        invocation = contract["paths"][endpoint + "/runs"]["post"]["requestBody"]["content"]["application/json"]
        fields = invocation["schema"]["properties"]["inputs"]
        assert fields["properties"]["delay"] == offset_constraint
        assert "delay" not in fields["required"]
        inputs = {key: upload(port, endpoint + "/uploads", auth, file, mime) for key, file, mime in files}
        call(port, endpoint + "/runs", 400, method="POST", body={"inputs": {**inputs, "delay": -1}},
             headers={**auth, "Idempotency-Key": token_hex(16)})
        delayed = {"inputs": {**inputs, "delay": 300}}
        submit = {**auth, "Idempotency-Key": token_hex(16)}
        receipt, _ = call(port, endpoint + "/runs", 202, method="POST", body=delayed, headers=submit)
        replay, _ = call(port, endpoint + "/runs", 200, method="POST", body=delayed, headers=submit)
        assert replay["id"] == receipt["id"]
        call(port, endpoint + "/runs", 409, method="POST", body={"inputs": inputs}, headers=submit)
        result, _ = wait_for_run(port, receipt, auth)

        outputs = {}
        for name in ("trimmed", "master"):
            download = result["outputs"][name]["download_url"]
            call(port, download, 401)
            data, headers = call(port, download, 200, headers=auth)
            assert headers.get_content_type() == "video/mp4"
            outputs[name] = directory / f"{name}.mp4"
            outputs[name].write_bytes(data)

        def video_hash(path):
            return ffmpeg("-i", path, "-map", "0:v:0", "-c:v", "copy", "-f", "streamhash", "-")

        assert video_hash(outputs["trimmed"]) == video_hash(outputs["master"])
        info = json.loads(command("ffprobe", "-v", "error", "-show_entries", "format=duration:stream=codec_type",
                                  "-of", "json", outputs["master"], cwd=directory, env=settings))
        assert 0.95 <= float(info["format"]["duration"]) <= 1.05
        assert [stream["codec_type"] for stream in info["streams"]] == ["video", "audio"]

        origin = {**auth, "Idempotency-Key": token_hex(16)}
        zero_delay, _ = call(port, endpoint + "/runs", 202, method="POST", body={"inputs": inputs}, headers=origin)
        immediate, _ = wait_for_run(port, zero_delay, auth)
        media, _ = call(port, immediate["outputs"]["master"]["download_url"], 200, headers=auth)
        starting = directory / "no-delay.mp4"
        starting.write_bytes(media)
        assert video_hash(starting) == video_hash(outputs["trimmed"])

        def peak(path, start_ms, end_ms):
            pcm = subprocess.run(["ffmpeg", "-nostdin", "-v", "error", "-i", path, "-map", "0:a:0",
                                  "-ac", "1", "-ar", "8000", "-f", "s16le", "-"],
                                 cwd=directory, env=settings, capture_output=True, check=True, timeout=15)
            samples = array("h")
            samples.frombytes(pcm.stdout)
            return max(abs(sample) for sample in samples[start_ms * 8:end_ms * 8])

        assert peak(starting, 100, 175) > 1000
        assert peak(outputs["master"], 100, 175) < 32
        assert peak(outputs["master"], 350, 425) > 1000
        print("PASS two private CPU runners published a cut and honored per-run narration timing with identical video packets")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-media-finishing-") as directory:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True),
                 Path(directory), verification=verify_media)


if __name__ == "__main__":
    main()

# TokenCraft media tools

Run this optional worker to process media on infrastructure you choose. The runner uses Python's standard library, a private state directory, and the system tools required by the selected pack. The default TokenCraft gateway, native package, and demo do not launch it or install its dependencies.

## Run from an audited checkout

Check out the desired TokenCraft source revision. The server and the versioned manifest are in `plugins/tools/media`. Copy that directory to the worker host to run it separately from the gateway. Keep the source revision and the manifest together when distributing the plugin. Use Python 3.12 or newer and a matching `ffmpeg` and `ffprobe` build with the `libx264`, `libmp3lame`, and `libwebp` encoders. The runner checks the encoders on startup. Distribution builds differ, including the standard and full FFmpeg formulas on systems that package them separately.

```sh
export TOKENCRAFT_MEDIA_SERVICE_TOKEN="$(openssl rand -hex 32)"
python3 plugins/tools/media/server.py \
  --state "${XDG_STATE_HOME:-$HOME/.local/state}/tokencraft/media" \
  --host 127.0.0.1 --port 8094
```

Provide the service token to the authorized service caller through a secret store. Use one runner process per state directory and keep the directory on a local filesystem with SQLite locking and atomic rename support. For a separate host, run behind private HTTPS ingress with a verified hostname and a small connection pool. Run as an unprivileged account, mount only the state directory writable, and deny outbound network access from the media process. Keep the selected system tools current and set CPU, memory, and disk limits. The worker can process one job at a time, with a 90-second processing timeout, 64 MiB per file, and 32 retained or staged requests by default. Successful results, failed jobs, and unsubmitted inputs remain for 24 hours. Expiration retains request identity in SQLite and removes media bytes. When sizing disk capacity, allow for 128 MiB of staged inputs and one result per retained request, upload spools, one active transcode, and SQLite.

```sh
curl -fsS http://127.0.0.1:8094/ready
curl -fsS -H "Authorization: Bearer $TOKENCRAFT_MEDIA_SERVICE_TOKEN" \
  http://127.0.0.1:8094/v1/tools
```

The runner test suite is optional and uses generated media.

```sh
python3 -m unittest discover -s plugins/tools/media -v
```

## Select a pack

The `media` pack uses FFmpeg and is the default when you start this optional runner. The `studio` pack adds editing and analysis recipes and needs FFmpeg with `libx264`, `libvpx-vp9`, and `libopus`. The `documents` pack uses Poppler's `pdfinfo`, `pdftoppm`, and `pdftotext`. The `mesh` and `delivery` packs run with Python's standard library. Start each pack on its own port with a different private state directory and service token. A state directory stays with the pack that initialized it.

```sh
export TOKENCRAFT_MEDIA_SERVICE_TOKEN="$(openssl rand -hex 32)"
python3 plugins/tools/media/server.py \
  --pack documents --port 8095 \
  --state "$PWD/.tokencraft-documents"
```

Use `manifest.json` for the FFmpeg binding. Other packs use `studio.manifest.json`, `documents.manifest.json`, `mesh.manifest.json`, or `delivery.manifest.json`. Use the selected pack name in `--pack`. Each process serves only its own manifest at `/v1/tools`. [Register a pack in your gateway](../../../docs/media-tools.md#attach-a-tool-runner-to-a-self-hosted-gateway)

## Operations

| Family | Operations |
| --- | --- |
| Media | Inspect container, size, duration, and bounded video and audio stream metadata |
| Video | Trim, join with scale and audio normalization, replace soundtrack, extract audio, mute, resize, crop, transcode to H.264, change speed, extract a frame, and create a GIF |
| Audio | Trim, concatenate, mix, apply loudness normalization or fades, and convert to WAV, MP3, or FLAC |
| Images | Resize, crop, convert to PNG, JPEG, or WebP, overlay, and extend a still into an MP4 |
| Studio video | Letterbox, rotate, change frame rate, overlay an image, make a contact sheet, attach a selectable caption track, and export VP9 WebM |
| Studio audio | Change speed and gain, reduce steady noise, resample, downmix to mono, and measure peak and RMS waveform |
| Studio images | Letterbox, rotate, flip, convert to monochrome, apply alpha masks, and estimate an image palette |
| Documents | Inspect PDF page count and version, render a page as PNG, and extract text across a bounded page range |
| Captions | Normalize UTF-8 SRT or WebVTT, shift timing, and extract a plain-text transcript |
| Mesh | Inspect self-contained GLB files, scale scenes, strip application extras, and convert embedded glTF JSON into a GLB |
| Delivery | Compute a SHA-256 fingerprint and bundle two or four assets into a ZIP with a digest manifest |

The manifest is authoritative for names, revisions, input slots, parameter defaults and constraints, and possible response content types. For the FFmpeg pack, geometry uses encoded pixels. A time in milliseconds refers to the normalized source timeline. A video crop has even output dimensions for 4:2:0 encoding. The runner probes each source and verifies its actual kind. Supported source containers include MP4/MOV, Matroska/WebM, WAV, MP3, FLAC, Ogg, and supported PNG, JPEG, WebP, and GIF images.

The mesh pack accepts glTF 2.0 with one embedded buffer and images stored in buffer views. Scaling wraps each scene's roots in a scale transform and preserves the embedded geometry bytes. [glTF 2.0 specification](https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html#glb-file-format-specification)

## Tool protocol version 1

Every route except `/ready` requires the service bearer. Obtain the manifest from `GET /v1/tools`. A tool has a stable `id`, an integer `revision`, named typed input slots, a closed JSON parameter schema, and one typed result. Retain an operation revision and its behavior while clients have workflows pinned to it.

1. Pick a request ID containing 1–128 ASCII letters, digits, underscores, or hyphens. For each named slot, send the bytes to `PUT /v1/jobs/{request_id}/inputs/{slot}`. Provide the exact `Content-Length` and lowercase `X-TokenCraft-Content-Sha256`. An identical upload returns `204`. Replacing a staged source under the same request ID and slot returns `409`.
2. Submit `POST /v1/jobs` with `Content-Type: application/json` and `Idempotency-Key` equal to the request ID. Bind the staged bytes by size and digest. An identical submission returns the same handle. A different invocation under an accepted ID returns `409`. An incomplete set of inputs yields the version-1 `rejected` outcome before dispatch.
3. Recover a lost submission response with `GET /v1/jobs/by-request/{request_id}`. Poll `GET /v1/jobs/{handle}` until `succeeded` or `failed`. A restarted runner requeues any claimed operation without issuing a new handle.
4. Fetch the signed-in result bytes from `GET /v1/jobs/{handle}/artifacts/0`. Check the response type, byte length, and `X-TokenCraft-Content-Sha256`. The runner never returns a remote result URL.

This is the job submission shape for a trim operation. Supply the digest and length of the bytes actually uploaded.

```json
{
  "version": 1,
  "request_id": "req_sample_trim",
  "model": "video.trim",
  "input": {
    "kind": "tool",
    "revision": 1,
    "inputs": {
      "video": {
        "sha256": "7a59f4e3ba202f44dc68a5147533b673bfece7c82c2f8df9952d1569cb73a84e",
        "size_bytes": 1847736
      }
    },
    "parameters": {"start_ms": 500, "duration_ms": 2000}
  }
}
```

The runner uses the existing queue's accepted receipt, lookup, polling, and artifact response formats. To use it in TokenCraft workflows, follow the [gateway binding setup](../../../docs/media-tools.md#attach-a-tool-runner-to-a-self-hosted-gateway). The runner supports files up to 64 MiB per slot when called directly. TokenCraft workflows impose a 32 MiB limit per input and output for the gateway handoff.

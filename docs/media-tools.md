# Media tool plugins

## Runner

The independently started [media runner](../plugins/tools/media/README.md) uses reviewed, versioned recipes for video, audio, and image operations. Its manifest supplies typed inputs, constrained parameters, and result media types to an editor or an API client. It speaks the queued connector's receipt, result status, request lookup, and artifact transport. The operator provides the service origin, trust configuration, service token, and compute capacity.

CPU recipes can be replaced or extended by another tool service implementing the same contract. Generation models remain available through the existing inference bindings. The runner can process an existing source repeatedly with different parameters. Each operation gets its own request ID and keeps the input bytes immutable for that request.

## Workflow execution

On a self-hosted gateway with a binding registered for the current project, open **Workflows** and choose a recipe under **Media tools** in the component library. Connect the required named ports to typed uploads, generation steps, or earlier tool steps. Set any required parameters and expose the desired results in **Response outputs**. Save, test, then publish through the usual workflow controls. Use the binding revision and operation revision returned by discovery when authoring graphs directly.

- A published graph pins the binding identity and operation revision. Run admission checks project scope and matches the graph's input slots, output type, and parameters to the reviewed manifest. Workflow tool steps use operator-provided compute and carry a zero model spending cap.
- The workflow worker transfers private result bytes to the configured plugin over its authenticated service connection. A stable request ID covers the run and step. After a lost receipt, the worker asks the original binding for the handle before considering a new submission. Cancellation stops new admission once it takes effect. A job already admitted by the tool service retains its own lifecycle.
- Generation artifacts retain their existing size limits. A tool step can receive up to 32 MiB per input and 64 MiB across its inputs, and return up to 32 MiB. TokenCraft verifies the result's MIME type, signature, length, and digest. It encrypts result bytes and saves them in PostgreSQL in the same transaction as the successful step checkpoint. The existing workflow content cleanup removes these bytes with the run content after seven days.
- Run status shows each admitted tool's request ID, status, and timestamps. Result URLs use the existing project and app scope, and app keys require `outputs:read` to download. Tool result responses are attachments. The plugin has its own independent file-retention settings.
- Migrations 71 and 72 restore durable workflows and app keys. Migration 73 adds encrypted tool result storage. Migration 74 adds short-lived encrypted uploads. Earlier gateway migration checksums stay intact. Databases from older standalone workflow drafts need a separate upgrade review because those drafts used different migration numbers.

## Use an existing source file

In the step inspector, set the input source to **Uploaded file**, then select or create a typed request field. Save the draft and choose the file when you test it. For a published endpoint, upload the file from your application server with the same application credential you use to submit runs.

```sh
WORKFLOW_ENDPOINT="$TOKENCRAFT_URL/v1/projects/$PROJECT_ID/workflow-endpoints/$WORKFLOW_ID"
FILE="input.png"
SHA256="$(shasum -a 256 "$FILE" | cut -d' ' -f1)"
UPLOAD_ID="$(curl -fsS "$WORKFLOW_ENDPOINT/uploads" \
  --header "Authorization: Bearer $WORKFLOW_API_KEY" \
  --header "Content-Type: image/png" \
  --header "X-TokenCraft-Content-Sha256: $SHA256" \
  --data-binary "@$FILE" | jq -r .upload_id)"
curl -fsS "$WORKFLOW_ENDPOINT/runs" \
  --header "Authorization: Bearer $WORKFLOW_API_KEY" \
  --header "Content-Type: application/json" \
  --header "Idempotency-Key: media-job-001" \
  --data "$(jq -n --arg id "$UPLOAD_ID" '{inputs:{source:{upload_id:$id}}}')"
```

Replace `source` with the key from your published contract. The upload needs `runs:submit`, the actual supported media type, and the exact body length. The same app can repeat an identical upload and receive its existing eligible ID. The gateway verifies the media signature, length, and SHA-256 and encrypts the bytes. Uploads expire after one hour. Submit the run within 30 minutes so the file remains available through its run deadline. Each workflow can hold up to 32 active files or 128 MiB, with 32 MiB per file. Workflow drafts and published apps have their own upload ownership, and app key rotation preserves the application's ownership.

## Planned tool coverage

| Pack | Coverage to verify |
| --- | --- |
| Editing and delivery | Explicit frame sizes, multi-clip assembly, overlays, watermarks, sprite and contact sheets, subtitle and caption tracks, compatible web and editing formats |
| Audio | Extraction, loudness, transitions, channel routing, mix and concatenation, noise treatment, and stem workflows |
| Image | Geometry, format conversion, masks and alpha compositing, thumbnails, color transforms, and batch preparation |
| Analysis | Typed probe output, fingerprints, waveform and loudness data, timestamps, and quality inspection |
| Specialist assets | Documents and rasterized pages, vector assets, mesh inspection and conversion, transcripts and captions, and optional model-backed finishing |
| Publishing | Packaging several project outputs with verified metadata and deterministic names |

CPU and model-backed packs share the versioned descriptor, explicit project policy, scoped inputs, and durable job receipts. Each pack declares its own media envelope, prerequisites, and execution model. The public image, video, and audio tooling reference for the first runner is the [FFmpeg documentation](https://ffmpeg.org/documentation.html).

## Attach a tool runner to a self-hosted gateway

Start the [worker](../plugins/tools/media/README.md#run-from-an-audited-checkout) independently. Apply TokenCraft's current migrations and enable [Workflows](workflows.md) on the gateway. Put this file next to the reviewed manifest, replacing the account and project IDs with IDs from your gateway.

```json
{
  "version": 1,
  "bindings": [{
    "name": "studio-media",
    "revision": "v1",
    "origin": "http://127.0.0.1:8094",
    "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_STUDIO_MEDIA",
    "manifest": "manifest.json",
    "account": "acct_REPLACE",
    "projects": ["proj_REPLACE"]
  }]
}
```

Set `TOKENCRAFT_WORKFLOW_TOOL_BINDINGS` to the binding registry path on both the gateway and workflow workers. Set the named environment variable in their secret configuration to the worker's `TOKENCRAFT_MEDIA_SERVICE_TOKEN`. A loopback sidecar can use HTTP. A separate host requires HTTPS. Use the matching reviewed manifest on the worker and in the registry.

Project developers and admins can discover their approved bindings at `GET /v1/projects/{project}/workflow-tools?account_id={account}` with a session cookie. An admin can add `&verify=studio-media` to verify the selected binding against the live worker. A successful check returns `reachable: true` and the matching manifest. Ordinary discovery uses the reviewed local manifest and succeeds during a worker outage. The gateway keeps connector origins and credentials out of the discovery response.

A binding revision identifies the binding name, operator revision, origin, and reviewed manifest. Advertised tools include their own operation revisions. Give a replacement its own binding name and keep the older entry, origin, and manifest available with `drain_only: true` until admitted runs finish. New runs against a version pinned to the older binding require a revised workflow and publication.

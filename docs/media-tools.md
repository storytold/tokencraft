# Media tool plugins

## Runner

The independently started [media runner](../plugins/tools/media/README.md) uses reviewed, versioned recipes for video, audio, and image operations. Its manifest supplies typed inputs, constrained parameters, and result media types to an editor or an API client. It speaks the queued connector's receipt, result status, request lookup, and artifact transport. The operator provides the service origin, trust configuration, service token, and compute capacity.

CPU recipes can be replaced or extended by another tool service implementing the same contract. Generation models remain available through the existing inference bindings. The runner can process an existing source repeatedly with different parameters. Each operation gets its own request ID and keeps the input bytes immutable for that request.

## Workflow integration design

- **Configuration.** The gateway loads reviewed manifests from an operator-owned binding registry. Discovery applies the registered account and project scope. The workflow editor will build sockets and parameter controls from the reviewed manifest. A published tool step will pin the operation revision and binding identity.
- **Artifact handoff.** The workflow runtime resolves asset references within the run's project and application scope. It validates the source record, content type, length, digest, and retention state before transferring bytes to the configured runner. A workflow supplies named artifact references and typed parameters. Connector addresses, raw filter graphs, environment variables, and local paths come only from the operator or from the implementation.
- **Dispatch.** The scheduler checkpoints a stable step request ID. A worker stages each authorized source and submits its size and digest. It persists the accepted handle, and on timeout it asks the original binding for that handle. After success, it verifies the result and publishes it under the project before advancing dependents. A failed step does not publish a successful workflow response.
- **Migration.** The current source restores the durable graph runner at migrations 71 and 72. Earlier gateway migrations keep their original numbers and checksums. A fresh installation supports versioned workflow runs and app keys. Databases from older standalone workflow drafts need a separate upgrade review because those drafts used different migration numbers.
- **Artifacts.** Generation artifact limits are 2 MiB for images and 8 MiB for video, audio, and meshes. The tool protocol has its own bounded input and output envelope. For larger items, use a dedicated bounded authenticated transfer path that retains the gateway's integrity checks and storage encryption. Publication must remain recoverable from the original handle after storage failure.

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

A binding revision identifies the binding name, operator revision, origin, and reviewed manifest. Advertised tools include their own operation revisions. To remove an older binding from new discovery, keep its manifest and names available and set `drain_only: true`. This registry is currently available for discovery and connection verification. Workflow node execution and private artifact handoff are tracked above.

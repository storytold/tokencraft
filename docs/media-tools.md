# Media tool plugins

## Runner

The independently started [media runner](../plugins/tools/media/README.md) uses reviewed, versioned recipes for video, audio, and image operations. Its manifest supplies typed inputs, constrained parameters, and result media types to an editor or an API client. It speaks the queued connector's receipt, result status, request lookup, and artifact transport. The operator provides the service origin, trust configuration, service token, and compute capacity.

CPU recipes can be replaced or extended by another tool service implementing the same contract. Generation models remain available through the existing inference bindings. The runner can process an existing source repeatedly with different parameters. Each operation gets its own request ID and keeps the input bytes immutable for that request.

## Workflow integration design

- **Configuration.** The gateway will load a reviewed tool manifest and operator-owned binding registry. Project visibility and exclusions apply before discovery and dispatch. The workflow editor builds sockets and parameter controls from the reviewed manifest. A published graph pins the operation identifier and revision, and admitted work pins the binding identity.
- **Artifact handoff.** The workflow runtime resolves asset references within the run's project and application scope. It validates the source record, content type, length, digest, and retention state before transferring bytes to the configured runner. A workflow supplies named artifact references and typed parameters. Connector addresses, raw filter graphs, environment variables, and local paths come only from the operator or from the implementation.
- **Dispatch.** The scheduler checkpoints a stable step request ID. A worker stages each authorized source and submits its size and digest. It persists the accepted handle, and on timeout it asks the original binding for that handle. After success, it verifies the result and publishes it under the project before advancing dependents. A failed step does not publish a successful workflow response.
- **Migration.** The older workflow snapshot has a durable graph runner and private image handoff. Its workflow migrations use 52 and 53, which already have different meanings on the current release. Port the workflow tables and modules onto the current migration sequence. Keep checked-in historical migrations intact. Exercise both a fresh install and a populated upgrade before relying on the new workflow paths.
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

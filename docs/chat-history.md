# Retain request inputs and outputs

TokenCraft can keep encrypted chat and generated media available for 30 days and archived until day 395. The clock starts at the first recorded completion, cancellation, failure, or unconfirmed outcome. A replay of the same admitted request uses its original policy and deadlines.

Set `TOKENCRAFT_CONTENT_ARCHIVE_DIR` to a private operator-managed filesystem directory to opt in for newly admitted connected chat, images, video, audio, and 3D. Use a dedicated mounted volume at the same canonical path on API and worker replicas. The content and event archive directories have separate lifecycles. Keep PostgreSQL, the archive, and the keys that protect the retained data in the same recovery plan.

Deployments using `TOKENCRAFT_CHAT_ARCHIVE_DIR` can keep that setting for chat-only retention. To include media on disk, set `TOKENCRAFT_CONTENT_ARCHIVE_DIR` to the existing canonical path. TokenCraft keeps the existing chat manifests readable across the upgrade.

## Store content in an S3-compatible bucket

Set the following options on the API and workers. New requests use the bucket when configured. Keep the filesystem directory mounted at its original path for existing requests. Quoting, admission, and archive metadata lookups do not contact the bucket.

| Setting | Value |
| --- | --- |
| `TOKENCRAFT_CONTENT_ARCHIVE_S3_ENDPOINT` | Region-specific HTTPS origin. Local fixtures accept literal loopback HTTP. |
| `TOKENCRAFT_CONTENT_ARCHIVE_S3_BUCKET` | Private unversioned bucket with Object Lock disabled. |
| `TOKENCRAFT_CONTENT_ARCHIVE_S3_REGION` | Signing region. |
| `TOKENCRAFT_CONTENT_ARCHIVE_S3_ACCESS_KEY`, `TOKENCRAFT_CONTENT_ARCHIVE_S3_SECRET_KEY` | Operator credentials for the content prefix with GetObject, PutObject, and DeleteObject access. |
| `TOKENCRAFT_CONTENT_ARCHIVE_S3_SESSION_TOKEN` | Optional temporary credentials. Restart with fresh credentials on rotation. |
| `TOKENCRAFT_CONTENT_ARCHIVE_S3_AUTO_RESTORE` | Set to `1` to let project administrators request temporary cold copies. Grant the gateway `s3:RestoreObject` on the content prefix. |
| `TOKENCRAFT_CONTENT_ARCHIVE_S3_PREFIX` | Defaults to `tokencraft-content/v1`. Use a dedicated prefix for each environment. |

Keep the same endpoint, bucket, region, and prefix across replicas and restarts until all requests pinned to the target have expired and cleanup has finished. TokenCraft conditionally writes encrypted objects and reads them back before removing the warm copy. Deletion is retried until storage acknowledges every object deletion. A versioned delete marker does not count as deletion. [S3 object deletion](https://docs.aws.amazon.com/AmazonS3/latest/API/API_DeleteObject.html)

To use a cold storage class, configure a prefix lifecycle transition after a read window that allows sealing and retries to finish. Keep objects available through their saved TokenCraft deadline. When a storage class requires retrieval, the history API returns HTTP `409` with `archive_retrieval_required`. A storage administrator can restore the request's `<request-id>.tch` manifest and, for media, its `<request-id>-<slot>.tcm` objects under the configured prefix. Retry the history read or artifact download once the object is readable. The gateway uses its original deadlines when reading temporary copies. [S3 lifecycle transitions](https://docs.aws.amazon.com/AmazonS3/latest/userguide/lifecycle-transition-general-considerations.html) · [Restoring archived objects](https://docs.aws.amazon.com/AmazonS3/latest/userguide/restoring-objects.html)

## Rotate the request content encryption key

The gateway defaults to `TOKENCRAFT_DEMO_PAYLOAD_KEY` for request content encryption. Set `TOKENCRAFT_CONTENT_ENCRYPTION_KEY` to choose a separate key for new encryption of dispatch inputs, results, chat deltas, and media. The gateway can also read with up to eight distinct keys in `TOKENCRAFT_CONTENT_ENCRYPTION_READ_KEYS`, a comma-separated list of 32-byte hex values. Keep `TOKENCRAFT_DEMO_PAYLOAD_KEY` stable across the rollout because it signs request intent fingerprints and connector completion tokens.

1. Generate a separate key with `openssl rand -hex 32` and store it in your secret manager. Add it to `TOKENCRAFT_CONTENT_ENCRYPTION_READ_KEYS` on every API and worker. Deploy rotation-capable binaries on all replicas with their current write key.
2. Set `TOKENCRAFT_CONTENT_ENCRYPTION_KEY` to the new key. Move the old write key into the read list and remove the new key from that list. For the first rotation, the old write key is `TOKENCRAFT_DEMO_PAYLOAD_KEY`. Roll the API and workers with both generations readable.
3. Verify a saved receipt replay, an older archive download, and a new result. A rollback after new writes needs a rotation-capable build with the same read overlap. Keep each historical key available for retained data, queued work, and backups that need it.

Archived bundles can contain entries encrypted under different generations. The worker copies their verified ciphertext when sealing. TokenCraft rotates [event archive encryption](event-archives.md#rotate-the-event-archive-key) through its own settings.

## Restore from Requests

With gateway restores enabled, open archived content in Requests and select **Restore from cold storage**. Project administrators can also send an empty JSON object to `POST /v1/requests/{id}/content/restore`, using the same account and project query parameters as a history read. The gateway records an audit event and returns HTTP `202` with object counts and restore status. Metadata reads expose the status to authorized request readers. The console updates progress while the restore is pending.

The worker asks storage for a Standard retrieval with a one-day temporary copy. It checks restore metadata before retrying an unconfirmed submission and allows up to three submissions per object in a restore job. The worker checks every five minutes for up to 48 hours and keeps the original retention deadline. It verifies each object's stored checksum before marking it ready. An archive contains one manifest and up to eight media objects. Repeating the restore call returns the same job while pending or within 24 hours of its start. Retrieval can incur bucket charges. [S3 RestoreObject](https://docs.aws.amazon.com/AmazonS3/latest/API/API_RestoreObject.html) · [S3 HeadObject](https://docs.aws.amazon.com/AmazonS3/latest/API/API_HeadObject.html)

A replacement worker uses the recorded reservation and storage metadata to resume each pending object. A stalled storage read leaves the request available for removal. After removal or expiry, the worker discards a late verification result. When the status is ready, retrieve or download the archive from Requests. Expiry and administrator deletion remove queued restore work and still apply to any temporary copy.

Requests verifies the saved artifact size and SHA-256 before saving the file. When the gateway is busy and supplies a retry delay of up to five seconds, the console retries once in place. You can cancel while the transfer is in progress.

## Read or remove content

| Operation | Behavior |
| --- | --- |
| `GET /v1/requests/{id}/content` | Returns the saved phase and dates. During the first 30 days it also returns the input and available final result. Pass `metadata_only=true` to omit the content. The response includes `media: true` for retained media. |
| `POST /v1/requests/{id}/content/retrieve` | Reads and verifies the archive manifest for a scoped caller. Returns input and any final result. Media results include scoped archive download URLs. |
| `POST /v1/requests/{id}/content/restore` | Queues a temporary storage restore for an archived request. Requires a project administrator session, a JSON `{}` body, and operator opt-in. |
| `GET /v1/requests/{id}/content/artifacts/{slot}` | Downloads one verified archived media file, with its original content type and SHA-256. |
| `DELETE /v1/projects/{project}/requests/{id}/content` | Project administrator action. Revokes future reads and queues deletion of archived files. |

Use a project key with `requests:read` or an authorized session for reads and retrieval. Client-bound keys stay within their client. For session downloads, pass the same account and project query parameters used for the request. Responses use `Cache-Control: no-store`.

The phase starts at `processing` and moves to `available` at the recorded outcome. At day 30, warm reads stop and the state becomes `archiving`. The worker verifies the encrypted files before removing PostgreSQL payloads and hot artifact references. It retries unsuccessful seals. In the `archived` phase, explicit retrieval serves the saved content from cold storage. It uses the existing request scope and does not restart the provider job. Final chat output is retained as a single result. Stream replay, including its original chunks, is available during the warm window.

At day 395, the API reports `expired` and blocks reads before cleanup. An administrator can remove the content earlier, producing the `removed` phase. Cleanup deletes the archived objects and the gateway publishes a scoped removal signal for [customer data consumers](customer-data.md). Request status, usage, and trace metadata stay available.

Keep a filesystem archive directory mounted until its managed history has expired and cleanup has finished. Backups and external copies have their own retention and deletion schedules.

The standard successful-result deadlines apply to previous admissions and requests outside the configured profile. The history endpoint returns 404 for those requests.

## Follow a live chat stream

Chat Completions and Responses return a TokenCraft request ID and a `Location` for status and result reads. If a viewer disconnects or stops receiving updates, the provider job can still finish. Use the same scoped key and request ID to poll the location. While retained, the original stream can also be replayed with the same idempotency key and request. Replay starts at the beginning of the retained output. Reset the partial transcript before applying replayed deltas.

Each API process admits up to 32 active stream readers. A reader fetches up to 32 stored deltas at a time and uses an eight-event output buffer. It checks the caller's access on each read. The response pump has a 45-second deadline, after which the client can follow `Location`.

## Use an SDK

The Rust and TypeScript SDKs expose metadata-only reads, warm reads, archive retrieval, and verified media downloads. Use the saved artifact reference to download a slot from your configured gateway. [Rust example](../sdk/rust/README.md#read-retained-history) · [TypeScript example](../sdk/typescript/README.md#read-retained-history)

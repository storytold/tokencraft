# Retain and restore project event history

TokenCraft seals scoped lifecycle, usage, and settlement events as gzip-compressed NDJSON encrypted with AES-256-GCM. Each successful seal verifies the written object and records its schema version, event count, first and last event IDs, and exact NDJSON SHA-256.

## Try it locally

`tokencraft dev` enables filesystem archives with a private `archives/` directory and a persistent `archive-key` in the same state directory. Reopening the state reuses the key. Back up the archive objects, key, and PostgreSQL manifests together. Local archive seals are manual.

Run `bash examples/connector/smoke.sh` against a disposable dev state. It generates a sample, seals its scoped export, and compares the restored bytes with the original. Use these endpoints for your own exports.

| Endpoint | Operation |
| --- | --- |
| `POST /v1/archives` | Queue a seal from an export ID. A project admin session passes `account_id` and `project_id` in the body. |
| `GET /v1/archives` | List archives in scope. Page with `before` from the previous response's `next_before`. |
| `GET /v1/archives/{id}` | Read the state and sealed manifest. |
| `POST /v1/archives/{id}/restore` | Verify the object and open a one-hour download window. |
| `GET /v1/archives/{id}/data` | Read verified NDJSON during the window. Compare the bytes with `X-TokenCraft-Content-Sha256`. |

For scoped reads and restore, sessions supply `account_id` and `project_id` in the query. A project key needs `exports:read`. A client-bound key can access archives sealed for that client. Automatic project-wide partitions require project-wide access to the archive itself. Each seal holds up to 8 MiB of uncompressed NDJSON and ends at a complete event. If a manual seal receipt has `has_more: true`, queue the next seal from the same export with `after` set to that receipt’s `last_event_id`. Restoring a partition makes its events available for export without dispatching historical requests.

Each API replica reserves two slots for restored archive downloads and holds a slot while it is serving the response bytes. If both slots are busy, the download returns `503 export_capacity` with `Retry-After: 2`. Retry the same archive during its restore window. The live feed and snapshot pages use their own export slots.

## Configure a deployment

For a filesystem deployment, prepare an operator-owned private directory with mode `0700` and set `TOKENCRAFT_DEMO_ARCHIVE_DIR` to its path. For either backend, set `TOKENCRAFT_DEMO_ARCHIVE_KEY` to the same persistent 32-byte hex key across the processes that read and write archives. Generate it separately from the session and request payload keys.

To store objects in a private path-style S3-compatible bucket, configure these settings on the gateway. Keep the filesystem directory configured when you need to read older objects sealed there. Each manifest pins the chosen storage target. All API and worker replicas that serve the same archive history need access to its storage.

| Setting | Value |
| --- | --- |
| `TOKENCRAFT_DEMO_ARCHIVE_S3_ENDPOINT` | Region-specific HTTPS origin. Local fixtures can use literal loopback HTTP. |
| `TOKENCRAFT_DEMO_ARCHIVE_S3_BUCKET` | Private bucket name. |
| `TOKENCRAFT_DEMO_ARCHIVE_S3_REGION` | Signing region, such as `us-east-1`. |
| `TOKENCRAFT_DEMO_ARCHIVE_S3_ACCESS_KEY`, `TOKENCRAFT_DEMO_ARCHIVE_S3_SECRET_KEY` | Operator-managed credentials restricted to the archive prefix. |
| `TOKENCRAFT_DEMO_ARCHIVE_S3_SESSION_TOKEN` | Session token if using temporary credentials. Restart with fresh credentials on rotation. |
| `TOKENCRAFT_DEMO_ARCHIVE_S3_PREFIX` | Defaults to `tokencraft-archives/v1`. Assign a distinct prefix to each environment. |
| `TOKENCRAFT_DEMO_ARCHIVE_S3_AUTO_RESTORE` | Set to `1` to let project admins explicitly request cold retrieval. |

Grant bucket credentials `s3:GetObject` and `s3:PutObject` for the configured object prefix. Grant `s3:DeleteObject` for cleanup of failed staged writes and `s3:RestoreObject` when enabling retrieval requests. [S3 permissions by operation](https://docs.aws.amazon.com/AmazonS3/latest/userguide/using-with-s3-policy-actions.html)

## Rotate the event archive key

A gateway deployment uses `TOKENCRAFT_DEMO_ARCHIVE_KEY` for new seals. Set `TOKENCRAFT_DEMO_ARCHIVE_READ_KEYS` to a comma-separated list of up to eight distinct additional 32-byte hex keys. The API and workers can read, restore, and retire archives sealed with any configured key. Leave the read setting unset for a deployment with one key.

1. Back up the existing keys and introduce the new key in the read list across every API and worker. Deploy a build with read-key support to all replicas before promoting the new key.
2. Roll the API and workers again with the new key as `TOKENCRAFT_DEMO_ARCHIVE_KEY`. Replace its read-list entry with the former write key and keep any other historical keys. Both configurations can read objects created during the rollout.
3. Verify a download from before the rotation and a newly sealed archive. Retain the historical keys with the archive recovery material for as long as their objects are needed. Use a build with read-key support when rolling back after the first seal with the new key.

Rotation applies to new event archive objects. Existing objects keep their encryption, including cold objects. Store the keys in the deployment's secret store and include them in recovery exercises.

## Schedule sealing and move payloads to cold storage

1. Set `TOKENCRAFT_DEMO_ARCHIVE_AUTO_AFTER_DAYS` to an age from 1 to 3650 days. Each scheduled seal takes the next contiguous range of up to 1,000 published events older than midnight UTC at the selected age. When a range exceeds 8 MiB, the worker seals a complete prefix and starts the next range after its last event once the verified archive commits.
2. Set `TOKENCRAFT_DEMO_ARCHIVE_RETIRE_AFTER_DAYS` to an age from 1 to 3650 days after a scheduled seal. The worker rereads the encrypted object, verifies it against the source, and then removes payloads from PostgreSQL. Event IDs, scope, and cursor order stay available. Keep the object readable through verification and retry.
3. Apply a bucket lifecycle transition with a read window long enough for sealing and retirement. Keep archived objects and their keys for the required history. Amazon S3 defaults can skip objects smaller than 128 KB. Check the size filter, per-object charges, and retrieval delay for your archive sizes. [S3 lifecycle transitions](https://docs.aws.amazon.com/AmazonS3/latest/userguide/lifecycle-transition-general-considerations.html)

Unset the age settings to leave scheduling and retirement off. Workers retry unavailable or changed ranges before removing any hot data. Follow the response pagination headers when exporting archived events. A page can end early after four archived partitions.

## Recover an export or sync

When a page has verified records, the gateway spends up to one second on additional archive reads before returning the available records. A cold partition or unavailable archive storage also ends the page early. Follow the returned cursor. If the first event on the next page requires a cold object, the export returns HTTP `409` with `archive_retrieval_required`. If storage is unavailable, it returns HTTP `503`. Retry from the same cursor when storage recovers.

A project administrator can retrieve the object through the storage provider and call the restore endpoint once it is readable. With gateway retrieval enabled, the admin can send `POST /v1/archives/{id}/restore?account_id=…&project_id=…&retrieve=true` with a session and the exact `Content-Type: application/json` header. The gateway requests a Standard retrieval with a one-day temporary copy and checks for availability for up to 48 hours. After an unconfirmed send or worker restart, it checks the original object's restore metadata before trying again. A retrieval permits up to three submissions. The gateway verifies the downloaded archive before it opens a download window. [S3 RestoreObject](https://docs.aws.amazon.com/AmazonS3/latest/API/API_RestoreObject.html) · [S3 HeadObject](https://docs.aws.amazon.com/AmazonS3/latest/API/API_HeadObject.html)

An administrator's retrieval request takes precedence over a pending verification-only restore. Repeated requests reuse an active cold retrieval. Once the object is readable, continue from the last committed cursor. The [PostgreSQL importer](customer-data.md) keeps the checkpoint while waiting and resumes without duplicating events. If a fixed snapshot expired, start a new snapshot from the same cursor.

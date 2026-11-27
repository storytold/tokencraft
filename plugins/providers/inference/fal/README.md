# TokenCraft fal plugin

This service connects the TokenCraft queue protocol to fal for image, video, audio, and 3D generation. The example catalog contains these offers.

| TokenCraft model | fal model | Supported request | Result limit |
| --- | --- | --- | --- |
| `fal_sample` | FLUX.1 [dev] | 1–4 PNG images at the catalog's aspect ratios | 2 MiB per image |
| `pixverse_v6` | [PixVerse v6](https://fal.ai/models/fal-ai/pixverse/v6/text-to-video/api) | 5-second 720p MP4 at 16:9 | 8 MiB |
| `stable_audio_25` | [Stable Audio 2.5](https://fal.ai/models/fal-ai/stable-audio-25/text-to-audio/api) | 5- or 10-second WAV | 8 MiB |
| `meshy_v6_preview` | [Meshy v6](https://fal.ai/models/fal-ai/meshy/v6/text-to-3d/api) | Untextured GLB with a target of 10,000 polygons | 8 MiB |

## Run locally

Install Node.js 22. Set `FAL_KEY` to the fal API key, `TOKENCRAFT_CONNECTOR_TOKEN_FAL` to a generated gateway-to-plugin bearer token, and `FAL_ARTIFACT_ORIGINS` to a comma-separated list of reviewed HTTPS artifact origins. Use origin URLs such as `https://v3.fal.media` only when they match your deployment's actual responses.

```sh
npm test
npm start
```

The listener defaults to `127.0.0.1:4021`. Set `PORT` to override it. Keep `.state/` or set `TOKENCRAFT_PLUGIN_STATE` to a persistent directory with mode `0700`. Point to the directory itself. Run one process with its own state directory per binding.

Copy the [example binding](bindings.example.json) and [catalog offers](catalog.example.json) into your [TokenCraft profile](../README.md#start-a-plugin). Set the same gateway token in the gateway environment and fill in reviewed rates before serving live traffic. The sample catalog uses zero as its placeholder price. Keep `FAL_KEY` in the plugin environment.

The plugin saves the model with the intent before contacting fal and the vendor handle before acknowledging a job. The example binding enables [receipt lookup](../../../../docs/provider-connectors.md#recover-a-lost-submission-receipt) so polling deployments can recover a saved job if the gateway misses the acknowledgement. After a restart, the saved model determines how to poll, read, and validate the result. Existing state remains readable as FLUX.1 [dev]. The tests run against local fixtures. [fal queue documentation](https://fal.ai/docs/documentation/model-apis/inference/queue)

## Rotate the gateway service token

Set `TOKENCRAFT_CONNECTOR_TOKEN_FAL` on the plugin to the new token. Set `TOKENCRAFT_PREVIOUS_SERVICE_TOKEN` to the current token and `TOKENCRAFT_PREVIOUS_SERVICE_TOKEN_EXPIRES_AT` to the end of the overlap in UTC, using `YYYY-MM-DDTHH:MM:SSZ`. Start the plugin with these settings, then roll the gateway processes with the new token in the existing binding environment variable.

The plugin stops accepting the previous token at the deadline. Remove the two rotation settings once the gateway rollout is complete. Keep the binding name, origin, and revision stable so outstanding requests keep their connector identity.

## Enable completion webhooks

Polling works with the example binding. To add signed fal completion events, configure these three settings on the plugin.

| Setting | Value |
| --- | --- |
| `FAL_WEBHOOK_ORIGIN` | Public HTTPS origin for fal, such as `https://fal-hooks.example.com` |
| `FAL_WEBHOOK_USER_ID` | User ID in fal's signed headers. Required in operator mode and optional in tenant mode. |
| `TOKENCRAFT_GATEWAY_ORIGIN` | Operator-configured HTTPS gateway origin or local `http://127.0.0.1:<port>` |

Forward only `POST /v1/fal/webhooks/<request_id>/<proof>` from the public origin to the plugin's loopback listener. Preserve the path, fal signature headers, and raw request body. Exclude callback URLs from access logs because the path contains a per-request proof. Keep the state directory on a persistent, plugin-owned filesystem with mode `0700`. It holds the request-bound gateway grants and fal proofs. Give the plugin outbound access to fal's JWKS endpoint and the configured gateway.

In the gateway binding registry, add a named entry like this and point the catalog's `local_binding` at it.

```json
{
  "name": "fal-webhooks",
  "revision": "v1",
  "origin": "http://127.0.0.1:4121/",
  "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_FAL_WEBHOOKS",
  "completion_webhooks": true
}
```

Run this instance with `PORT=4121` and a separate state directory. Set its `TOKENCRAFT_CONNECTOR_TOKEN_FAL` to the same value as the gateway's `TOKENCRAFT_CONNECTOR_TOKEN_FAL_WEBHOOKS`. When moving from polling, retain the previous binding as `drain_only` until its outstanding work has finished.

The plugin verifies the signed request ID, user, timestamp, and raw payload before recovering a missing fal handle. It saves the mapping before sending the gateway a completion hint. A failed forward returns an error to fal so its delivery retry can use the saved mapping across a restart. Results come from the pinned fal queue and allowlisted artifact origins. Polling remains available during callback outages. [fal webhook verification and retry policy](https://fal.ai/docs/documentation/model-apis/inference/webhooks)

## Use organization and project keys

Set `FAL_TENANT_KEYS=true`, leave `FAL_KEY` unset, and use a separate state directory. Give the plugin and its matching `"tenant_keys": true` binding an independent service bearer token of at least 32 characters. The connector authenticates fal submission and polling with the saved request key supplied by TokenCraft. It persists only a per-job key tag. Downloads from allowlisted artifact origins use the artifact URL.

Tenant instances can also enable completion webhooks. Set the public webhook origin and gateway origin. Leave `FAL_WEBHOOK_USER_ID` unset for multiple fal accounts, or set it to limit the accepted signer account. A valid signed event can wake polling only when it matches a handle already acknowledged by fal. If the upstream acknowledgement is lost, the job stays in reconciliation until its ownership can be established. [fal Queue API](https://fal.ai/docs/documentation/model-apis/inference/queue)

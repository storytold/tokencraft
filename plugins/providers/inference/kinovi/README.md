# TokenCraft Kinovi plugin

This service connects TokenCraft queued video to Kinovi's Seedance 2.0 task API. The sample offer is five-second, 16:9, 720p MP4 generation.

## Run locally

Install Node.js 22. Set `KINOVI_KEY` to your API key, `TOKENCRAFT_CONNECTOR_TOKEN_KINOVI` to a generated gateway-to-plugin bearer token, and `KINOVI_ARTIFACT_ORIGINS` to a comma-separated list of reviewed HTTPS artifact origins.

```sh
npm test
npm start
```

The listener defaults to `127.0.0.1:4023`. Set `PORT` to override it. Keep `.state/` or set `TOKENCRAFT_PLUGIN_STATE` to a persistent directory with mode `0700`. Point to the directory itself. Run one process with its own state directory per binding.

Copy the [example binding](bindings.example.json) and [catalog offer](catalog.example.json) into your [TokenCraft profile](../README.md#start-a-plugin). Set the same gateway token in the gateway environment and fill in reviewed rates before serving live traffic. The sample catalog uses zero as its placeholder price. Keep `KINOVI_KEY` in the plugin environment.

The plugin saves the task handle before acknowledging the gateway. The example binding enables [receipt lookup](../../../../docs/provider-connectors.md#recover-a-lost-submission-receipt) for saved jobs whose gateway acknowledgement was lost. A task without a confirmed supplier handle remains in reconciliation. The loopback test covers task creation, result polling, restart and file retrieval. Verify the API contract and artifact origin with your Kinovi account before a live rollout.

On success, the plugin can report the exact credit quantity from Kinovi's `recordInfo` response. Gateways opt in with `X-TokenCraft-Provider-Charge: 1` on the status poll. Reserved or refunded credits are omitted. [Kinovi billing](https://kinovi.ai/docs/credits-billing)

## Rotate the gateway service token

Set `TOKENCRAFT_CONNECTOR_TOKEN_KINOVI` on the plugin to the new token. Set `TOKENCRAFT_PREVIOUS_SERVICE_TOKEN` to the current token and `TOKENCRAFT_PREVIOUS_SERVICE_TOKEN_EXPIRES_AT` to the end of the overlap in UTC, using `YYYY-MM-DDTHH:MM:SSZ`. Start the plugin with these settings, then roll the gateway processes with the new token in the existing binding environment variable.

The plugin stops accepting the previous token at the deadline. Remove the two rotation settings once the gateway rollout is complete. Keep the binding name, origin, and revision stable so outstanding requests keep their connector identity.

## Use organization and project keys

Set `KINOVI_TENANT_KEYS=true`, leave `KINOVI_KEY` unset, and use a separate state directory. Give the plugin and its matching `"tenant_keys": true` binding an independent service bearer token of at least 32 characters. TokenCraft supplies the pinned tenant key for creation and result polling. The connector stores a per-job key tag and uses the reviewed artifact URL for downloads. TokenCraft can send its standard customer webhook after polling observes completion. [Kinovi API reference](https://dev.kinovi.ai/docs/api)

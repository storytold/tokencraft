import { queueServer, required, validatedOrigin } from './queue.mjs';
import { configuredKeyMode } from './tenant-keys.mjs';
import { fal } from './provider.mjs';
import { configuredFalWebhooks } from './fal-webhooks.mjs';

const token = required('TOKENCRAFT_CONNECTOR_TOKEN_FAL');
const { key, tenantKeys } = configuredKeyMode('FAL_KEY', 'FAL_TENANT_KEYS', token);
const provider = fal({
  origin: 'https://queue.fal.run/',
  key,
  artifactOrigins: required('FAL_ARTIFACT_ORIGINS').split(',').map(validatedOrigin),
});
const server = await queueServer({
  token,
  tenantKeys,
  monitoringToken: process.env.TOKENCRAFT_PLUGIN_MONITOR_TOKEN,
  previousToken: process.env.TOKENCRAFT_PREVIOUS_SERVICE_TOKEN,
  previousTokenExpiresAt: process.env.TOKENCRAFT_PREVIOUS_SERVICE_TOKEN_EXPIRES_AT,
  state: process.env.TOKENCRAFT_PLUGIN_STATE ?? '.state',
  provider,
  webhooks: configuredFalWebhooks({ tenantKeys }),
});
server.listen(Number(process.env.PORT ?? '4021'), '127.0.0.1');

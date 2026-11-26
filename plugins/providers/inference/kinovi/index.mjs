import { queueServer, required, validatedOrigin } from './queue.mjs';
import { configuredKeyMode } from './tenant-keys.mjs';
import { kinovi } from './provider.mjs';

const token = required('TOKENCRAFT_CONNECTOR_TOKEN_KINOVI');
const { key, tenantKeys } = configuredKeyMode('KINOVI_KEY', 'KINOVI_TENANT_KEYS', token);
const provider = kinovi({
  origin: 'https://kinovi.ai/',
  key,
  artifactOrigins: required('KINOVI_ARTIFACT_ORIGINS').split(',').map(validatedOrigin),
});
const server = await queueServer({
  token,
  tenantKeys,
  traceLogs: process.env.TOKENCRAFT_PLUGIN_TRACE_LOGS === 'true',
  monitoringToken: process.env.TOKENCRAFT_PLUGIN_MONITOR_TOKEN,
  previousToken: process.env.TOKENCRAFT_PREVIOUS_SERVICE_TOKEN,
  previousTokenExpiresAt: process.env.TOKENCRAFT_PREVIOUS_SERVICE_TOKEN_EXPIRES_AT,
  state: process.env.TOKENCRAFT_PLUGIN_STATE ?? '.state',
  provider,
});
server.listen(Number(process.env.PORT ?? '4023'), '127.0.0.1');

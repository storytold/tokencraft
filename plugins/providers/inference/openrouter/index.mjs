import { openRouterProxy } from './proxy.mjs';

const {
  OPENROUTER_API_KEY: upstreamKey,
  TOKENCRAFT_LOCAL_CHAT_KEY_OPENROUTER: gatewayKey,
  OPENROUTER_MODELS,
  OPENROUTER_TENANT_KEYS = 'false',
  PORT = '4022',
} = process.env;
if (!['true', 'false'].includes(OPENROUTER_TENANT_KEYS)) {
  throw new Error('set OPENROUTER_TENANT_KEYS to true or false');
}
const tenantKeys = OPENROUTER_TENANT_KEYS === 'true';
if (!gatewayKey || !OPENROUTER_MODELS || (tenantKeys ? upstreamKey || gatewayKey.length < 32 : !upstreamKey)) {
  throw new Error('configure gateway auth, allowed models and exactly one OpenRouter credential mode');
}
const models = new Set(OPENROUTER_MODELS.split(',').map((model) => model.trim()).filter(Boolean));
if (!models.size) throw new Error('configure at least one OpenRouter model');
openRouterProxy({
  endpoint: 'https://openrouter.ai/api/v1/chat/completions',
  upstreamKey, tenantKeys, gatewayKey, models,
  monitoringToken: process.env.TOKENCRAFT_PLUGIN_MONITOR_TOKEN,
  previousToken: process.env.TOKENCRAFT_PREVIOUS_SERVICE_TOKEN,
  previousTokenExpiresAt: process.env.TOKENCRAFT_PREVIOUS_SERVICE_TOKEN_EXPIRES_AT,
}).listen(Number(PORT), '127.0.0.1');

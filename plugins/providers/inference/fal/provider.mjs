import { readJson, submitUpstream, upstream } from './queue.mjs';
import { models } from './models.mjs';

export function fal({ origin, key, artifactOrigins }) {
  const response = async (path, authorization) => {
    const reply = await upstream(new URL(path, origin), { headers: { authorization } });
    if (!reply.ok) throw new Error('fal response unavailable');
    return readJson(reply.body);
  };
  return {
    artifactOrigins,
    model: (name) => models.get(name),
    input({ model: name, input }) {
      const model = models.get(name);
      if (!model || input?.kind !== model.kind || typeof input.prompt !== 'string' || !input.prompt.trim()) return;
      return model.input(input);
    },
    submit(name, body, callbackUrl, delegated) {
      const endpoint = new URL(name, origin);
      if (callbackUrl) endpoint.searchParams.set('fal_webhook', callbackUrl);
      return submitUpstream(endpoint, `Key ${delegated ?? key}`, body, (result) => result.request_id);
    },
    async poll(model, id, delegated, { includeFiles = true } = {}) {
      const authorization = `Key ${delegated ?? key}`;
      const path = `${model.statusPath}/requests/${id}`;
      const status = await response(`${path}/status`, authorization);
      if (status.status === 'IN_QUEUE') return { status: 'queued' };
      if (status.status === 'IN_PROGRESS') return { status: 'running' };
      if (status.status !== 'COMPLETED') throw new Error('unexpected fal job status');
      if (status.error || status.error_type) return { status: 'failed' };
      if (!includeFiles) return { status: 'succeeded' };
      const files = model.files(await response(path, authorization));
      if (!files.length || files.some((url) => typeof url !== 'string' || !url)) {
        throw new Error('unexpected fal result');
      }
      return { status: 'succeeded', files };
    },
  };
}

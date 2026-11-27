import { cancelUnusedBody, readJson, submitUpstream, upstream } from './queue.mjs';

const model = 'seedance-20';

function creditQuantity(source) {
  if (!source || source.length > 48) return undefined;
  const decimal = /^(0|[1-9]\d*)(?:\.(\d+))?(?:[eE]([+-]?\d+))?$/.exec(source);
  if (!decimal) return undefined;
  const [, whole, fraction = '', exponent = '0'] = decimal;
  const digits = whole + fraction;
  const first = digits.search(/[1-9]/);
  if (first < 0) return '0';
  const significant = digits.slice(first).replace(/0+$/, '');
  const point = whole.length + Number(exponent) - first;
  if (point > 15 || significant.length - point > 9) return undefined;
  if (point <= 0) return `0.${'0'.repeat(-point)}${significant}`;
  if (point >= significant.length) return significant.padEnd(point, '0');
  return `${significant.slice(0, point)}.${significant.slice(point)}`;
}

export function kinovi({ origin, key, artifactOrigins }) {
  return {
    artifactOrigins,
    mediaType: 'video/mp4',
    maxBytes: 8 * 1024 * 1024,
    input({ model: name, input }) {
      if (name !== model || input?.kind !== 'video' || input.duration_seconds !== 5
          || input.aspect_ratio !== '16:9' || input.resolution !== '720p'
          || typeof input.prompt !== 'string' || !input.prompt.trim()
          || Buffer.byteLength(input.prompt) > 2000) return undefined;
      return { model, inputs: { prompt: input.prompt, duration: '5',
        outputResolution: '720p', aspectRatio: '16:9' }, autoFix: false };
    },
    submit(body, delegated) {
      return submitUpstream(new URL('/api/v1/jobs/createTask', origin), `Bearer ${delegated ?? key}`, body, (result) => result.taskId);
    },
    async poll(id, delegated) {
      const url = new URL('/api/v1/jobs/recordInfo', origin);
      url.searchParams.set('taskId', id);
      const response = await upstream(url, { headers: { authorization: `Bearer ${delegated ?? key}` } });
      if (!response.ok) {
        cancelUnusedBody(response);
        throw new Error('Kinovi response unavailable');
      }
      const result = await readJson(response.body, undefined, (name, value, context) => {
        if (name === 'creditsUsed') return typeof value === 'number' ? creditQuantity(context?.source) : undefined;
        return value;
      });
      if (result.taskId !== id || result.model !== model) throw new Error('unexpected Kinovi task');
      if (result.status === 'waiting') return { status: 'queued' };
      if (result.status === 'generating') return { status: 'running' };
      if (result.status === 'fail') return { status: 'failed' };
      if (result.status !== 'success' || result.output?.length !== 1
          || typeof result.output[0]?.url !== 'string') throw new Error('unexpected Kinovi video result');
      const providerCharge = result.creditsUsed === undefined ? undefined : { unit: 'credit', quantity: result.creditsUsed };
      return { status: 'succeeded', files: [result.output[0].url], providerCharge };
    },
  };
}

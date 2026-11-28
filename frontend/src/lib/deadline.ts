const maxTimerDelay = 2_147_483_647;

export function scheduleDeadline(timestamp: number, onDeadline: () => void, now = Date.now): () => void {
  let timer: number | undefined;
  function check() {
    const remaining = timestamp - now();
    if (remaining <= 0) onDeadline();
    else timer = window.setTimeout(check, Math.min(Math.ceil(remaining), maxTimerDelay));
  }
  check();
  return () => window.clearTimeout(timer);
}

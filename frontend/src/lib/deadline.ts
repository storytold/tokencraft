const maxTimerDelay = 2_147_483_647;

export function scheduleDeadline(timestamp: number, onDeadline: () => void): () => void {
  let timer: number | undefined;
  function check() {
    const remaining = timestamp - Date.now();
    if (remaining <= 0) onDeadline();
    else timer = window.setTimeout(check, Math.min(remaining, maxTimerDelay));
  }
  check();
  return () => window.clearTimeout(timer);
}

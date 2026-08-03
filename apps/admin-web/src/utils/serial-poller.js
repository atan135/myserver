const DEFAULT_INTERVAL_MS = 15_000;
const DEFAULT_MAX_INTERVAL_MS = 120_000;

export function createSerialPoller({
  task,
  intervalMs = DEFAULT_INTERVAL_MS,
  maxIntervalMs = DEFAULT_MAX_INTERVAL_MS,
  backoffFactor = 2,
  setTimer = setTimeout,
  clearTimer = clearTimeout,
  createAbortController = () => new AbortController()
}) {
  if (typeof task !== "function") {
    throw new TypeError("task must be a function");
  }
  if (!Number.isFinite(intervalMs) || intervalMs < 1) {
    throw new RangeError("intervalMs must be positive");
  }
  if (!Number.isFinite(maxIntervalMs) || maxIntervalMs < intervalMs) {
    throw new RangeError("maxIntervalMs must be at least intervalMs");
  }
  if (!Number.isFinite(backoffFactor) || backoffFactor < 1) {
    throw new RangeError("backoffFactor must be at least 1");
  }

  let running = false;
  let inFlight = false;
  let rerunRequested = false;
  let timer = null;
  let controller = null;
  let consecutiveFailures = 0;

  function nextDelay() {
    return Math.min(intervalMs * (backoffFactor ** consecutiveFailures), maxIntervalMs);
  }

  function clearScheduledTimer() {
    if (timer !== null) {
      clearTimer(timer);
      timer = null;
    }
  }

  function schedule(delayMs) {
    if (!running) return;
    clearScheduledTimer();
    timer = setTimer(run, delayMs);
  }

  async function run() {
    timer = null;
    if (!running) return;
    if (inFlight) {
      rerunRequested = true;
      return;
    }

    inFlight = true;
    controller = createAbortController();
    let succeeded = false;
    try {
      succeeded = (await task({ signal: controller.signal })) !== false;
    } catch (error) {
      if (!controller.signal.aborted) {
        succeeded = false;
      }
    } finally {
      inFlight = false;
      controller = null;
    }

    if (!running) return;
    consecutiveFailures = succeeded ? 0 : consecutiveFailures + 1;
    const delayMs = rerunRequested ? 0 : nextDelay();
    rerunRequested = false;
    schedule(delayMs);
  }

  return {
    start() {
      if (running) return;
      running = true;
      consecutiveFailures = 0;
      schedule(0);
    },
    trigger() {
      if (!running) return;
      if (inFlight) {
        rerunRequested = true;
        return;
      }
      schedule(0);
    },
    stop() {
      running = false;
      rerunRequested = false;
      clearScheduledTimer();
      controller?.abort();
    },
    getState() {
      return { running, inFlight, consecutiveFailures };
    }
  };
}

export function closeHttpServer(httpServer) {
  if (typeof httpServer?.close !== "function") return Promise.resolve();
  return new Promise((resolve, reject) => {
    let settled = false;
    const complete = (error) => {
      if (settled) return;
      settled = true;
      if (error && error.code !== "ERR_SERVER_NOT_RUNNING") reject(error);
      else resolve();
    };
    try {
      const result = httpServer.close(complete);
      if (result && typeof result.then === "function") result.then(() => complete(), complete);
    } catch (error) {
      complete(error);
    }
  });
}

export function createShutdownHandler({
  shutdownGateway,
  closeHttp,
  closeApplication,
  exit,
  info = (..._args) => {},
  error = (..._args) => {}
}) {
  let shutdownPromise = null;
  return (signal) => {
    if (!shutdownPromise) {
      shutdownPromise = (async () => {
        info(`Shutdown signal: ${signal}`);
        try {
          await shutdownGateway();
          await closeHttp();
          await closeApplication();
          info("Shutdown complete");
          exit(0);
        } catch (cause) {
          error("Shutdown failed", cause);
          exit(1);
        }
      })();
    }
    return shutdownPromise;
  };
}

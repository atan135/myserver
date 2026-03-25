export function log(level, message, extra = {}) {
  const entry = {
    level,
    message,
    service: "auth-http",
    time: new Date().toISOString(),
    ...extra
  };

  console.log(JSON.stringify(entry));
}


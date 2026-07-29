/**
 * Build NATS connection options from the common MyServer NATS_URL setting.
 *
 * Production secret initialization stores a Core NATS token as
 * `nats://<token>@host:port`. The JavaScript NATS client expects token
 * authentication in a separate option, so remove userinfo from the server
 * address and provide the decoded token explicitly.
 */
export function natsConnectOptions(natsUrl, name) {
  let parsed;
  try {
    parsed = new URL(natsUrl);
  } catch {
    return { servers: natsUrl, name };
  }

  if (!parsed.username || parsed.password) {
    return { servers: natsUrl, name };
  }

  const token = decodeURIComponent(parsed.username);
  parsed.username = "";
  return {
    servers: parsed.toString(),
    name,
    token
  };
}

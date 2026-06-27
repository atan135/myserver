/**
 * Authentication utilities for mock-client
 */

const DEFAULT_CHARACTER_APPEARANCE = {};

function getCharacterId(character) {
  return character?.character_id || character?.characterId || "";
}

function getCharacterWorldId(character) {
  if (!character) {
    return null;
  }

  return character.world_id ?? character.worldId ?? null;
}

function getCharacterName(character) {
  return character?.name || "";
}

function summarizeCharacter(character) {
  if (!character) {
    return null;
  }

  return {
    characterId: getCharacterId(character) || null,
    name: getCharacterName(character) || null,
    worldId: getCharacterWorldId(character),
    status: character.status || null,
    displayDiscriminator: character.display_discriminator || character.displayDiscriminator || null
  };
}

export function decodeTicketPayload(ticket) {
  if (!ticket || typeof ticket !== "string") {
    return null;
  }

  const [payloadB64] = ticket.split(".");
  if (!payloadB64) {
    return null;
  }

  try {
    return JSON.parse(Buffer.from(payloadB64, "base64url").toString("utf8"));
  } catch {
    return null;
  }
}

export function summarizeTicketPayload(ticket) {
  const payload = decodeTicketPayload(ticket);
  if (!payload) {
    return null;
  }

  return {
    accountPlayerId: payload.playerId || null,
    characterId: payload.characterId || null,
    worldId: payload.worldId ?? null,
    exp: payload.exp || null
  };
}

function attachTicketSummary(login) {
  const ticketPayload = summarizeTicketPayload(login.ticket);
  if (!ticketPayload) {
    return login;
  }

  return {
    ...login,
    playerId: login.playerId || ticketPayload.accountPlayerId,
    accountPlayerId: login.accountPlayerId || login.playerId || ticketPayload.accountPlayerId,
    characterId: login.characterId || ticketPayload.characterId,
    worldId: login.worldId ?? ticketPayload.worldId,
    ticketPayload
  };
}

/**
 * Resolve account credentials from options with optional overrides
 * @param {Object} options
 * @param {Object} overrides
 * @returns {{loginName: string, password: string}|null}
 */
export function resolveAccountCredentials(options, overrides = {}) {
  const loginName = overrides.loginName ?? options.loginName;
  const password = overrides.password ?? options.password;

  if (!loginName && !password) {
    return null;
  }

  if (!loginName || !password) {
    throw new Error("account login requires both loginName and password");
  }

  return {
    loginName,
    password
  };
}

/**
 * Resolve credentials for multi-client login (clientA/clientB)
 * @param {Object} options
 * @param {string} clientSuffix - "A" or "B"
 * @param {string} guestId
 * @returns {Object}
 */
export function resolveMultiClientLoginOverrides(options, clientSuffix, guestId) {
  const loginNameKey = `loginName${clientSuffix}`;
  const passwordKey = `password${clientSuffix}`;
  const loginName = options[loginNameKey];
  const password = options[passwordKey];

  if (loginName || password) {
    if (!loginName || !password) {
      throw new Error(
        `client${clientSuffix} account login requires both --login-name-${clientSuffix.toLowerCase()} and --password-${clientSuffix.toLowerCase()}`
      );
    }

    return {
      loginName,
      password
    };
  }

  if (options.loginName || options.password) {
    throw new Error(
      "multi-client account login requires --login-name-a/--password-a and --login-name-b/--password-b"
    );
  }

  return { guestId };
}

/**
 * Format login summary for display
 * @param {Object} login
 * @returns {Object}
 */
export function formatLoginSummary(login) {
  const ticketPayload = login.ticketPayload || summarizeTicketPayload(login.ticket);

  return {
    accountPlayerId: login.accountPlayerId || login.playerId || ticketPayload?.accountPlayerId || null,
    characterId: login.characterId || ticketPayload?.characterId || null,
    worldId: login.worldId ?? ticketPayload?.worldId ?? null,
    loginName: login.loginName || null,
    guestId: login.guestId || null,
    hasAccessToken: Boolean(login.accessToken),
    ticketPreview: login.ticket ? `${login.ticket.slice(0, 16)}...` : null,
    ticketPayload: ticketPayload
      ? {
          accountPlayerId: ticketPayload.accountPlayerId || null,
          characterId: ticketPayload.characterId || null,
          worldId: ticketPayload.worldId ?? null,
          exp: ticketPayload.exp || null
        }
      : null,
    ticketExpiresAt: login.ticketExpiresAt || null,
    character: summarizeCharacter(login.character),
    services: summarizeServices(login.services)
  };
}

function summarizeServices(services) {
  if (!services) {
    return null;
  }

  return {
    game: services.game ? `${services.game.host}:${services.game.port}` : null,
    chat: services.chat ? `${services.chat.host}:${services.chat.port}` : null,
    mail: services.mail ? `${services.mail.host}:${services.mail.port}` : null,
    announce: services.announce ? `${services.announce.host}:${services.announce.port}` : null
  };
}

function applyTcpService(options, service, hostKey, portKey) {
  if (!service?.host || !service?.port) {
    return;
  }

  if (service.protocol && service.protocol !== "tcp") {
    return;
  }

  options[hostKey] = service.host;
  options[portKey] = Number(service.port);
}

function applyHttpService(options, service, baseUrlKey) {
  if (!service?.host || !service?.port) {
    return;
  }

  const protocol = service.protocol === "https" ? "https" : "http";
  options[baseUrlKey] = `${protocol}://${service.host}:${service.port}`;
}

export function applyDiscoveredServices(options, login) {
  if (!options.useServiceDiscovery || !login?.services) {
    return;
  }

  applyTcpService(options, login.services.game, "gameHost", "port");
  applyTcpService(options, login.services.chat, "chatHost", "chatPort");
  applyHttpService(options, login.services.mail, "mailBaseUrl");
  applyHttpService(options, login.services.announce, "announceBaseUrl");
}

function buildAuthHeaders(accessToken, hasBody = false) {
  const headers = hasBody ? { "content-type": "application/json" } : {};
  if (accessToken) {
    headers.authorization = `Bearer ${accessToken}`;
  }
  return headers;
}

async function readJsonPayload(response) {
  if (typeof response.text === "function") {
    const text = await response.text();
    if (!text) {
      return null;
    }

    try {
      return JSON.parse(text);
    } catch {
      return { rawText: text };
    }
  }

  try {
    return await response.json();
  } catch {
    return null;
  }
}

async function requestAuthJson(options, pathname, init = {}) {
  const response = await fetch(`${options.httpBaseUrl}${pathname}`, {
    ...init,
    headers: {
      ...(init.headers || {})
    },
    signal: AbortSignal.timeout(options.timeoutMs)
  });
  const payload = await readJsonPayload(response);

  return {
    ok: response.ok && payload?.ok !== false,
    status: response.status,
    payload
  };
}

function assertAuthJsonOk(label, result) {
  if (result.ok) {
    return result.payload;
  }

  const error = result.payload?.error || result.payload?.errorCode || "REQUEST_FAILED";
  const message = result.payload?.message || JSON.stringify(result.payload);
  throw new Error(`${label} failed with status ${result.status}: ${error}${message ? `: ${message}` : ""}`);
}

export function parseCharacterAppearance(options) {
  const raw = options.characterAppearanceJson || "";
  if (!raw.trim()) {
    return { ...DEFAULT_CHARACTER_APPEARANCE };
  }

  let appearance;
  try {
    appearance = JSON.parse(raw);
  } catch (error) {
    throw new Error(`invalid --character-appearance-json: ${error.message}`);
  }

  if (!appearance || typeof appearance !== "object" || Array.isArray(appearance)) {
    throw new Error("--character-appearance-json must be a JSON object");
  }

  return appearance;
}

function normalizeCharacterNameToken(value) {
  return String(value || "")
    .replace(/[^A-Za-z0-9_-]/g, "")
    .slice(0, 16);
}

export function buildGeneratedCharacterName(options, suffix = Date.now().toString(36)) {
  const token = normalizeCharacterNameToken(suffix).slice(-6) || "000001";
  const fallbackPrefix = "MockRole";
  const rawPrefix = normalizeCharacterNameToken(options.characterNamePrefix || fallbackPrefix) || fallbackPrefix;
  const maxPrefixLength = Math.max(2, 16 - token.length);
  const prefix = rawPrefix.slice(0, maxPrefixLength);
  return `${prefix}${token}`.slice(0, 16);
}

export function buildCharacterCreateInput(options, overrides = {}) {
  return {
    name: overrides.name || options.characterName || buildGeneratedCharacterName(options, overrides.suffix),
    appearance: overrides.appearance || parseCharacterAppearance(options)
  };
}

export async function requestCharacterList(options, accessToken) {
  return requestAuthJson(options, "/api/v1/characters", {
    method: "GET",
    headers: buildAuthHeaders(accessToken)
  });
}

export async function listCharacters(options, accessToken) {
  const payload = assertAuthJsonOk(
    "character list",
    await requestCharacterList(options, accessToken)
  );

  return {
    ...payload,
    characters: Array.isArray(payload.characters) ? payload.characters : []
  };
}

export async function requestCreateCharacter(options, accessToken, input = null) {
  const characterInput = input || buildCharacterCreateInput(options);
  return requestAuthJson(options, "/api/v1/characters", {
    method: "POST",
    headers: buildAuthHeaders(accessToken, true),
    body: JSON.stringify({
      name: characterInput.name,
      appearance: characterInput.appearance
    })
  });
}

export async function createCharacter(options, accessToken, input = null) {
  const payload = assertAuthJsonOk(
    "character create",
    await requestCreateCharacter(options, accessToken, input)
  );

  return payload.character;
}

export async function requestSelectCharacter(options, accessToken, characterId) {
  return requestAuthJson(options, "/api/v1/characters/select", {
    method: "POST",
    headers: buildAuthHeaders(accessToken, true),
    body: JSON.stringify({ character_id: characterId })
  });
}

export async function requestDeleteCharacter(options, accessToken, characterId) {
  return requestAuthJson(options, "/api/v1/characters/delete", {
    method: "POST",
    headers: buildAuthHeaders(accessToken, true),
    body: JSON.stringify({ character_id: characterId })
  });
}

export async function deleteCharacter(options, accessToken, characterId) {
  return assertAuthJsonOk(
    "character delete",
    await requestDeleteCharacter(options, accessToken, characterId)
  );
}

export async function requestRestoreCharacter(options, accessToken, characterId) {
  return requestAuthJson(options, "/api/v1/characters/restore", {
    method: "POST",
    headers: buildAuthHeaders(accessToken, true),
    body: JSON.stringify({ character_id: characterId })
  });
}

export async function restoreCharacter(options, accessToken, characterId) {
  return assertAuthJsonOk(
    "character restore",
    await requestRestoreCharacter(options, accessToken, characterId)
  );
}

export async function requestCharacterProfile(options, accessToken, characterId) {
  const encodedCharacterId = encodeURIComponent(characterId);
  return requestAuthJson(options, `/api/v1/characters/${encodedCharacterId}/profile`, {
    method: "GET",
    headers: buildAuthHeaders(accessToken)
  });
}

export async function getCharacterProfile(options, accessToken, characterId) {
  return assertAuthJsonOk(
    "character profile",
    await requestCharacterProfile(options, accessToken, characterId)
  );
}

export async function selectCharacter(options, session, characterId) {
  const payload = assertAuthJsonOk(
    "character select",
    await requestSelectCharacter(options, session.accessToken, characterId)
  );
  const character = payload.character || { character_id: payload.characterId || characterId };
  const login = attachTicketSummary({
    ...session,
    ...payload,
    character,
    characterId: payload.characterId || getCharacterId(character),
    worldId: payload.worldId ?? getCharacterWorldId(character)
  });

  applyDiscoveredServices(options, login);
  return login;
}

function findCharacterForOptions(characters, options) {
  if (options.characterName) {
    return characters.find((character) => getCharacterName(character) === options.characterName) || null;
  }

  return characters[0] || null;
}

function buildNoCharacterError(session) {
  return new Error(
    `account ${session.playerId} has no characters; create one first with ` +
    `--scenario character-create or rerun with --auto-create-character / --create-character-if-missing`
  );
}

async function resolveCharacterLogin(options, session) {
  if (session.ticket) {
    return attachTicketSummary(session);
  }

  if (!session.accessToken) {
    throw new Error("character selection requires accessToken; omit --ticket or login with account/guest credentials");
  }

  if (options.characterId) {
    return selectCharacter(options, session, options.characterId);
  }

  let characters = [];
  if (!options.autoCreateCharacter) {
    const listed = await listCharacters(options, session.accessToken);
    characters = listed.characters;
    const selected = findCharacterForOptions(characters, options);
    if (selected) {
      return selectCharacter(options, session, getCharacterId(selected));
    }
  }

  if (options.autoCreateCharacter || options.createCharacterIfMissing) {
    const character = await createCharacter(options, session.accessToken);
    return selectCharacter(options, session, getCharacterId(character));
  }

  if (characters.length === 0) {
    throw buildNoCharacterError(session);
  }

  throw new Error(
    `character "${options.characterName}" was not found; pass --create-character-if-missing or choose an existing --character-id`
  );
}

function ticketExpiresSoon(login, skewMs = 30000) {
  if (!login?.ticketExpiresAt) {
    return false;
  }

  const expiresAt = new Date(login.ticketExpiresAt).getTime();
  return Number.isFinite(expiresAt) && expiresAt <= Date.now() + skewMs;
}

export async function refreshTicketIfNeeded(options, login, skewMs = 30000) {
  if (!ticketExpiresSoon(login, skewMs)) {
    return login;
  }

  if (!login.accessToken) {
    throw new Error("ticket is near expiry but accessToken is unavailable; fetch a new login or omit --ticket");
  }

  const characterId = login.characterId || getCharacterId(login.character);
  if (!characterId) {
    throw new Error("ticket refresh requires a selected characterId; login again with --character-id or select a character");
  }

  const response = await fetch(`${options.httpBaseUrl}/api/v1/game-ticket/issue`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${login.accessToken}`,
      "content-type": "application/json"
    },
    body: JSON.stringify({ character_id: characterId })
  });

  const payload = await response.json();
  if (!response.ok || !payload.ok) {
    throw new Error(`ticket refresh failed with status ${response.status}: ${JSON.stringify(payload)}`);
  }

  login.ticket = payload.ticket;
  login.ticketExpiresAt = payload.ticketExpiresAt;
  login.services = payload.services || login.services;
  login.characterId = payload.characterId || characterId;
  login.worldId = payload.worldId ?? login.worldId;
  login.ticketPayload = summarizeTicketPayload(payload.ticket);
  applyDiscoveredServices(options, login);
  return login;
}

/**
 * Logout: destroy session and optionally revoke ticket
 * @param {string} baseUrl
 * @param {string} accessToken
 * @param {string} [ticket]
 * @returns {Promise<Object>}
 */
export async function logout(baseUrl, accessToken, ticket = null) {
  const headers = {
    authorization: `Bearer ${accessToken}`,
    "content-type": "application/json"
  };

  const body = ticket ? JSON.stringify({ ticket }) : "{}";

  const response = await fetch(`${baseUrl}/api/v1/auth/logout`, {
    method: "POST",
    headers,
    body
  });

  return {
    status: response.status,
    payload: await response.json()
  };
}

/**
 * Run logout scenario: login -> verify session -> logout -> verify session destroyed
 * @param {Object} options
 */
export async function runLogout(options) {
  // Step 1: Login
  console.log("[logout] step 1: logging in...");
  const login = await fetchTicket(options);
  console.log("[logout] login:", JSON.stringify(formatLoginSummary(login), null, 2));

  // Step 2: Verify session is active
  console.log("[logout] step 2: verifying session with /me...");
  const meResponse = await fetch(`${options.httpBaseUrl}/api/v1/auth/me`, {
    headers: { authorization: `Bearer ${login.accessToken}` }
  });
  const mePayload = await meResponse.json();
  if (meResponse.status !== 200 || !mePayload.ok) {
    throw new Error(`expected /me to succeed, got status=${meResponse.status}: ${JSON.stringify(mePayload)}`);
  }
  console.log("[logout] /me OK:", JSON.stringify({ playerId: mePayload.playerId, guestId: mePayload.guestId }));

  // Step 3: Logout (with ticket)
  console.log("[logout] step 3: calling logout...");
  const logoutResult = await logout(options.httpBaseUrl, login.accessToken, login.ticket);
  if (logoutResult.status !== 200 || !logoutResult.payload.ok) {
    throw new Error(`logout failed: status=${logoutResult.status}, ${JSON.stringify(logoutResult.payload)}`);
  }
  console.log("[logout] logout OK:", JSON.stringify(logoutResult.payload));

  // Step 4: Verify session is destroyed
  console.log("[logout] step 4: verifying session is destroyed...");
  const meAfterResponse = await fetch(`${options.httpBaseUrl}/api/v1/auth/me`, {
    headers: { authorization: `Bearer ${login.accessToken}` }
  });
  const meAfterPayload = await meAfterResponse.json();
  if (meAfterResponse.status !== 401) {
    throw new Error(`expected /me to return 401 after logout, got status=${meAfterResponse.status}: ${JSON.stringify(meAfterPayload)}`);
  }
  console.log("[logout] /me after logout correctly returned 401:", JSON.stringify(meAfterPayload));

  // Step 5: Verify ticket is also revoked
  console.log("[logout] step 5: verifying ticket is revoked...");
  const ticketIssueResponse = await fetch(`${options.httpBaseUrl}/api/v1/game-ticket/issue`, {
    method: "POST",
    headers: { authorization: `Bearer ${login.accessToken}` }
  });
  if (ticketIssueResponse.status !== 401) {
    throw new Error(`expected ticket issue to return 401 after logout, got status=${ticketIssueResponse.status}`);
  }
  console.log("[logout] ticket issue after logout correctly returned 401");

  console.log("[logout] all checks passed");
}

/**
 * Run kick-session scenario: login twice with same account, verify old session is kicked
 * Phase 1: HTTP session invalidation
 * Phase 2: TCP active kick push via game-server
 * @param {Object} options
 */
export async function runKickSession(options) {
  // ===== Phase 1: HTTP session kick =====
  console.log("[kick-session] ===== Phase 1: HTTP session kick =====");

  // Step 1: Login first time
  console.log("[kick-session] step 1: first login...");
  const loginA = await fetchTicket(options);
  console.log("[kick-session] session A:", JSON.stringify(formatLoginSummary(loginA), null, 2));

  // Step 2: Verify session A is active
  console.log("[kick-session] step 2: verifying session A with /me...");
  const meA = await fetch(`${options.httpBaseUrl}/api/v1/auth/me`, {
    headers: { authorization: `Bearer ${loginA.accessToken}` }
  });
  const meAPayload = await meA.json();
  if (meA.status !== 200 || !meAPayload.ok) {
    throw new Error(`expected session A /me to succeed, got status=${meA.status}: ${JSON.stringify(meAPayload)}`);
  }
  console.log("[kick-session] session A /me OK:", JSON.stringify({ playerId: meAPayload.playerId }));

  // Step 3: Login again with same account (should kick session A)
  console.log("[kick-session] step 3: second login (same account)...");
  const loginB = await fetchTicket(options);
  console.log("[kick-session] session B:", JSON.stringify(formatLoginSummary(loginB), null, 2));

  // Step 4: Verify session A is now invalid (kicked)
  console.log("[kick-session] step 4: verifying session A is kicked...");
  const meAAfter = await fetch(`${options.httpBaseUrl}/api/v1/auth/me`, {
    headers: { authorization: `Bearer ${loginA.accessToken}` }
  });
  const meAAfterPayload = await meAAfter.json();
  if (meAAfter.status !== 401) {
    throw new Error(`expected session A /me to return 401 after kick, got status=${meAAfter.status}: ${JSON.stringify(meAAfterPayload)}`);
  }
  console.log("[kick-session] session A correctly returned 401 (kicked)");

  // Step 5: Verify session B is still valid
  console.log("[kick-session] step 5: verifying session B is still valid...");
  const meB = await fetch(`${options.httpBaseUrl}/api/v1/auth/me`, {
    headers: { authorization: `Bearer ${loginB.accessToken}` }
  });
  const meBPayload = await meB.json();
  if (meB.status !== 200 || !meBPayload.ok) {
    throw new Error(`expected session B /me to succeed, got status=${meB.status}: ${JSON.stringify(meBPayload)}`);
  }
  console.log("[kick-session] session B /me OK:", JSON.stringify({ playerId: meBPayload.playerId }));

  console.log("[kick-session] Phase 1 all checks passed");

  // ===== Phase 2: TCP active kick push =====
  console.log("\n[kick-session] ===== Phase 2: TCP active kick push =====");

  const { TcpProtocolClient } = await import("./client.js");
  const { MESSAGE_TYPE } = await import("./constants.js");
  const { encodeAuthReq, decodeByMessageType } = await import("./messages.js");

  // Step 6: Login to get a fresh ticket for TCP auth
  console.log("[kick-session] step 6: login for TCP auth...");
  const loginC = await fetchTicket(options);
  console.log("[kick-session] session C:", JSON.stringify(formatLoginSummary(loginC), null, 2));

  // Step 7: Connect to game-server TCP and authenticate
  console.log("[kick-session] step 7: connecting to game-server and authenticating...");
  const client = new TcpProtocolClient(options, "kick-test");
  await client.connect();

  try {
    await client.send(MESSAGE_TYPE.AUTH_REQ, 1, encodeAuthReq(loginC.ticket));
    const authPacket = await client.readNextPacket(options.timeoutMs);
    const authRes = decodeByMessageType(authPacket.messageType, authPacket.body);
    if (authPacket.messageType !== MESSAGE_TYPE.AUTH_RES) {
      throw new Error(`expected AUTH_RES (${MESSAGE_TYPE.AUTH_RES}), got messageType=${authPacket.messageType}`);
    }
    if (!authRes.ok) {
      throw new Error(`TCP auth failed: ${authRes.errorCode}`);
    }
    console.log("[kick-session] TCP auth OK, accountPlayerId:", authRes.accountPlayerId);

    // Step 8: Second login via HTTP (triggers kick on the TCP connection)
    console.log("[kick-session] step 8: second login to trigger TCP kick...");
    const loginD = await fetchTicket(options);
    console.log("[kick-session] session D created:", JSON.stringify(formatLoginSummary(loginD), null, 2));

    // Step 9: Wait for SESSION_KICK_PUSH on TCP
    console.log("[kick-session] step 9: waiting for SESSION_KICK_PUSH on TCP...");
    const kickPacket = await client.readNextPacket(options.timeoutMs);
    const kickData = decodeByMessageType(kickPacket.messageType, kickPacket.body);

    if (kickPacket.messageType !== MESSAGE_TYPE.SESSION_KICK_PUSH) {
      throw new Error(`expected SESSION_KICK_PUSH (${MESSAGE_TYPE.SESSION_KICK_PUSH}), got messageType=${kickPacket.messageType}: ${JSON.stringify(kickData)}`);
    }
    console.log("[kick-session] received SESSION_KICK_PUSH:", JSON.stringify(kickData));
  } finally {
    client.close();
  }

  console.log("[kick-session] all Phase 1 + Phase 2 checks passed");
}

async function changePassword(options, accessToken, oldPassword, newPassword) {
  const response = await fetch(`${options.httpBaseUrl}/api/v1/auth/change-password`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${accessToken}`,
      "content-type": "application/json"
    },
    body: JSON.stringify({ oldPassword, newPassword })
  });

  const payload = await response.json();
  if (!response.ok || !payload.ok) {
    throw new Error(`change-password failed with status ${response.status}: ${JSON.stringify(payload)}`);
  }

  return payload;
}

async function assertGameAuth(options, login, expectedOk, expectedErrorCode = "") {
  const { TcpProtocolClient } = await import("./client.js");
  const { MESSAGE_TYPE } = await import("./constants.js");
  const { encodeAuthReq, decodeByMessageType } = await import("./messages.js");

  const client = new TcpProtocolClient(options, "auth-check");
  await client.connect();
  try {
    await client.send(MESSAGE_TYPE.AUTH_REQ, 1, encodeAuthReq(login.ticket));
    const packet = await client.readNextPacket(options.timeoutMs);
    const authRes = decodeByMessageType(packet.messageType, packet.body);
    if (packet.messageType !== MESSAGE_TYPE.AUTH_RES) {
      throw new Error(`expected AUTH_RES (${MESSAGE_TYPE.AUTH_RES}), got messageType=${packet.messageType}`);
    }
    if (authRes.ok !== expectedOk) {
      throw new Error(`expected auth ok=${expectedOk}, got ${JSON.stringify(authRes)}`);
    }
    if (expectedErrorCode && authRes.errorCode !== expectedErrorCode) {
      throw new Error(`expected auth errorCode=${expectedErrorCode}, got ${authRes.errorCode}`);
    }
    console.log("[password-ticket-revoke] auth check:", JSON.stringify(authRes));
  } finally {
    client.close();
  }
}

export async function runPasswordTicketRevoke(options) {
  if (!options.loginName || !options.password || !options.newPassword) {
    throw new Error("password-ticket-revoke requires --login-name, --password and --new-password");
  }

  if (options.password === options.newPassword) {
    throw new Error("--new-password must differ from --password");
  }

  console.log("[password-ticket-revoke] step 1: login with old password...");
  const oldLogin = await fetchTicket(options);
  console.log("[password-ticket-revoke] old login:", JSON.stringify(formatLoginSummary(oldLogin), null, 2));

  console.log("[password-ticket-revoke] step 2: old ticket should authenticate before password change...");
  await assertGameAuth(options, oldLogin, true);

  console.log("[password-ticket-revoke] step 3: changing password...");
  await changePassword(options, oldLogin.accessToken, options.password, options.newPassword);

  console.log("[password-ticket-revoke] step 4: old ticket should be revoked after password change...");
  await assertGameAuth(options, oldLogin, false, "TICKET_REVOKED");

  console.log("[password-ticket-revoke] step 5: login with new password and verify fresh ticket...");
  const originalPassword = options.password;
  let newLogin = null;
  options.password = options.newPassword;
  try {
    newLogin = await fetchTicket(options);
    console.log("[password-ticket-revoke] new login:", JSON.stringify(formatLoginSummary(newLogin), null, 2));
    await assertGameAuth(options, newLogin, true);
  } finally {
    options.password = originalPassword;
  }

  if (options.restorePasswordAfterTest && newLogin?.accessToken) {
    console.log("[password-ticket-revoke] step 6: restoring original password...");
    await changePassword(options, newLogin.accessToken, options.newPassword, originalPassword);
  }

  console.log("[password-ticket-revoke] all checks passed");
}

/**
 * Fetch login session from HTTP auth service without selecting a character.
 * @param {Object} options
 * @param {Object} overrides - Override loginName/password/guestId
 * @returns {Promise<Object>} Login response with playerId and accessToken
 */
export async function fetchLoginSession(options, overrides = {}) {
  // If ticket is provided and no overrides, use it directly
  if (options.ticket && Object.keys(overrides).length === 0) {
    return attachTicketSummary({ playerId: "manual-ticket", accessToken: "", ticket: options.ticket, manualTicket: true });
  }

  // If guestId is explicitly provided in overrides, use guest login directly
  // This ensures that guest login works even when options has loginName/password
  if (overrides.guestId) {
    const response = await fetch(`${options.httpBaseUrl}/api/v1/auth/guest-login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ guestId: overrides.guestId })
    });

    if (!response.ok) {
      throw new Error(`guest-login failed with status ${response.status}`);
    }

    const payload = await response.json();
    if (!payload.ok) {
      throw new Error(`guest-login failed: ${JSON.stringify(payload)}`);
    }

    applyDiscoveredServices(options, payload);
    return payload;
  }

  // Try account login first
  const accountCredentials = resolveAccountCredentials(options, overrides);
  if (accountCredentials) {
    const response = await fetch(`${options.httpBaseUrl}/api/v1/auth/login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(accountCredentials)
    });

    if (!response.ok) {
      throw new Error(`account login failed with status ${response.status}`);
    }

    const payload = await response.json();
    if (!payload.ok) {
      throw new Error(`account login failed: ${JSON.stringify(payload)}`);
    }

    applyDiscoveredServices(options, payload);
    return payload;
  }

  // Fall back to guest login
  const guestId = overrides.guestId || options.guestId;
  const response = await fetch(`${options.httpBaseUrl}/api/v1/auth/guest-login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(guestId ? { guestId } : {})
  });

  if (!response.ok) {
    throw new Error(`guest-login failed with status ${response.status}`);
  }

  const payload = await response.json();
  if (!payload.ok) {
    throw new Error(`guest-login failed: ${JSON.stringify(payload)}`);
  }

  applyDiscoveredServices(options, payload);
  return payload;
}

/**
 * Fetch a character-bound game ticket from HTTP auth service.
 * @param {Object} options
 * @param {Object} overrides - Override loginName/password/guestId
 * @returns {Promise<Object>} Login response with playerId, characterId, ticket, accessToken
 */
export async function fetchTicket(options, overrides = {}) {
  const session = await fetchLoginSession(options, overrides);
  if (session.manualTicket) {
    return attachTicketSummary(session);
  }
  if (session.ticket) {
    const ticketPayload = summarizeTicketPayload(session.ticket);
    if (ticketPayload?.characterId) {
      return attachTicketSummary(session);
    }
    session.ticket = null;
    session.ticketExpiresAt = null;
  }

  return resolveCharacterLogin(options, session);
}

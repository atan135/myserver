/**
 * Authentication utilities for mock-client
 */

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
  return {
    playerId: login.playerId,
    loginName: login.loginName || null,
    guestId: login.guestId || null,
    hasAccessToken: Boolean(login.accessToken),
    ticketPreview: login.ticket ? `${login.ticket.slice(0, 16)}...` : null
  };
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
    console.log("[kick-session] TCP auth OK, playerId:", authRes.playerId);

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

/**
 * Fetch authentication ticket from HTTP auth service
 * @param {Object} options
 * @param {Object} overrides - Override loginName/password/guestId
 * @returns {Promise<Object>} Login response with playerId, ticket, accessToken
 */
export async function fetchTicket(options, overrides = {}) {
  // If ticket is provided and no overrides, use it directly
  if (options.ticket && Object.keys(overrides).length === 0) {
    return { playerId: "manual-ticket", accessToken: "", ticket: options.ticket };
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

  return payload;
}

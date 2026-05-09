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

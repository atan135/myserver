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

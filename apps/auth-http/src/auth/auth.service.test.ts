import assert from "node:assert/strict";
import test from "node:test";

import { AuthService } from "./auth.service.js";

function createRequest() {
  return {
    headers: {},
    socket: { remoteAddress: "127.0.0.1" }
  };
}

function createService({ lockStatus, loginError, recordResult }: any) {
  const audits: any[] = [];
  const lockout = {
    async getLockStatus() {
      return lockStatus;
    },
    async recordFailedAttempt() {
      return recordResult;
    },
    async clearFailedAttempts() {
      throw new Error("successful login must not clear attempts in this test");
    }
  };
  const authStore = {
    async createPasswordSession() {
      throw loginError;
    }
  };
  const dbStore = {
    appendSecurityAudit(entry: any) {
      audits.push(entry);
    }
  };
  const service = new AuthService(
    {
      dbEnabled: true,
      accountLockEnabled: true,
      gameProxyHost: "127.0.0.1",
      gameProxyPort: 4000,
      trustProxy: false,
      trustedProxies: []
    },
    authStore,
    lockout,
    dbStore,
    null,
    { async getStatus() { return { enabled: false }; } }
  );

  return { service, audits };
}

test("AuthService records an invalid password and preserves the stable credential error", async () => {
  const invalidCredentials = Object.assign(new Error("invalid credentials"), {
    code: "INVALID_LOGIN_CREDENTIALS"
  });
  const { service, audits } = createService({
    lockStatus: { locked: false, remainingSeconds: 0 },
    loginError: invalidCredentials,
    recordResult: { locked: true, attempts: 5 }
  });

  await assert.rejects(
    () => service.login({ loginName: "locked_user", password: "wrong-password" }, createRequest(), {}),
    (error: any) => error.getStatus() === 401 && error.getResponse().error === "INVALID_LOGIN_CREDENTIALS"
  );
  assert.deepEqual(
    audits.map((entry) => [entry.eventType, entry.details]),
    [
      ["account_locked", { attempts: 5 }],
      ["login_failed", { reason: "INVALID_LOGIN_CREDENTIALS" }]
    ]
  );
});

test("AuthService rejects a locked account before credential verification and returns Retry-After", async () => {
  const { service, audits } = createService({
    lockStatus: { locked: true, remainingSeconds: 42 },
    loginError: new Error("credential verification must not run"),
    recordResult: { locked: false, attempts: 0 }
  });
  const headers = new Map<string, string>();
  const response = {
    setHeader(name: string, value: string) {
      headers.set(name, value);
    }
  };

  await assert.rejects(
    () => service.login({ loginName: "locked_user", password: "Password123!" }, createRequest(), response),
    (error: any) => error.getStatus() === 403 && error.getResponse().error === "ACCOUNT_LOCKED"
  );
  assert.equal(headers.get("Retry-After"), "42");
  assert.deepEqual(audits.map((entry) => entry.eventType), ["account_locked_login_attempt"]);
});

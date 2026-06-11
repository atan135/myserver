import assert from "node:assert/strict";
import { test } from "node:test";

import { getConfig } from "../apps/auth-http/src/config.js";

function withEnv(overrides, fn) {
  const previousEnv = new Map(Object.entries(process.env));
  try {
    for (const [key, value] of Object.entries(overrides)) {
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
    return fn();
  } finally {
    for (const key of Object.keys(process.env)) {
      if (!previousEnv.has(key)) {
        delete process.env[key];
      }
    }
    for (const [key, value] of previousEnv.entries()) {
      process.env[key] = value;
    }
  }
}

test("auth-http production config rejects default and placeholder ticket secrets", () => {
  for (const ticketSecret of [
    undefined,
    "dev-only-change-this-ticket-secret",
    "replace-with-a-long-random-string"
  ]) {
    withEnv(
      {
        NODE_ENV: "production",
        APP_ENV: undefined,
        TICKET_SECRET: ticketSecret,
        GAME_ADMIN_TOKEN: "prod-game-admin-token",
        INTERNAL_API_TOKEN: "prod-internal-api-token"
      },
      () => {
        assert.throws(
          () => getConfig(),
          /TICKET_SECRET must be set to a non-default value in production/
        );
      }
    );
  }
});

test("auth-http production config rejects default, empty, and missing game admin token", () => {
  for (const gameAdminToken of [
    undefined,
    "dev-only-change-this-game-admin-token",
    ""
  ]) {
    withEnv(
      {
        NODE_ENV: "production",
        APP_ENV: undefined,
        TICKET_SECRET: "prod-ticket-secret",
        GAME_ADMIN_TOKEN: gameAdminToken,
        INTERNAL_API_TOKEN: "prod-internal-api-token"
      },
      () => {
        assert.throws(
          () => getConfig(),
          /GAME_ADMIN_TOKEN must be set to a non-default value in production/
        );
      }
    );
  }
});

test("auth-http production config rejects empty and missing internal api token", () => {
  for (const internalApiToken of [undefined, ""]) {
    withEnv(
      {
        NODE_ENV: "production",
        APP_ENV: undefined,
        TICKET_SECRET: "prod-ticket-secret",
        GAME_ADMIN_TOKEN: "prod-game-admin-token",
        INTERNAL_API_TOKEN: internalApiToken
      },
      () => {
        assert.throws(
          () => getConfig(),
          /INTERNAL_API_TOKEN must be set in production/
        );
      }
    );
  }
});

test("auth-http production config accepts non-default required secrets", () => {
  withEnv(
    {
      NODE_ENV: "production",
      APP_ENV: undefined,
      TICKET_SECRET: "prod-ticket-secret",
      GAME_ADMIN_TOKEN: "prod-game-admin-token",
      INTERNAL_API_TOKEN: "prod-internal-api-token"
    },
    () => {
      const config = getConfig();
      assert.equal(config.env, "production");
      assert.equal(config.ticketSecret, "prod-ticket-secret");
      assert.equal(config.gameAdminToken, "prod-game-admin-token");
      assert.equal(config.internalApiToken, "prod-internal-api-token");
    }
  );
});

test("auth-http production config also honors APP_ENV", () => {
  withEnv(
    {
      NODE_ENV: "development",
      APP_ENV: "production",
      TICKET_SECRET: "dev-only-change-this-ticket-secret",
      GAME_ADMIN_TOKEN: "prod-game-admin-token",
      INTERNAL_API_TOKEN: "prod-internal-api-token"
    },
    () => {
      assert.throws(
        () => getConfig(),
        /TICKET_SECRET must be set to a non-default value in production/
      );
    }
  );
});

test("auth-http development config allows defaults and empty internal token", () => {
  withEnv(
    {
      NODE_ENV: "development",
      APP_ENV: undefined,
      TICKET_SECRET: undefined,
      GAME_ADMIN_TOKEN: undefined,
      INTERNAL_API_TOKEN: undefined
    },
    () => {
      const config = getConfig();
      assert.equal(config.env, "development");
      assert.equal(config.ticketSecret, "dev-only-change-this-ticket-secret");
      assert.equal(config.gameAdminToken, "dev-only-change-this-game-admin-token");
      assert.equal(config.internalApiToken, "");
    }
  );
});

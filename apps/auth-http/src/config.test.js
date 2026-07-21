import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const CONFIG_ENV_KEYS = [
  "NODE_ENV",
  "APP_ENV",
  "AUTH_REQUIRE_TLS",
  "AUTH_EXPOSE_INTERNAL_SERVICE_ENDPOINTS",
  "AUTH_REGISTER_REQUIRE_REVIEW",
  "DISCOVERY_REQUIRED",
  "DISALLOW_LEGACY_DIRECT_CONFIG",
  "REGISTRY_ENABLED",
  "REGISTRY_KEY_PREFIX",
  "REDIS_KEY_PREFIX",
  "TRUST_PROXY",
  "TRUSTED_PROXIES",
  "SERVICE_BUILD_VERSION",
  "SERVICE_NAME",
  "SERVICE_INSTANCE_ID",
  "SERVICE_ZONE",
  "SERVICE_BIND_HOST",
  "SERVICE_PUBLIC_HOST",
  "SERVICE_ADVERTISED_HOST",
  "HOST",
  "GAME_PROXY_HOST",
  "GAME_PROXY_PORT",
  "GAME_SERVER_ADMIN_HOST",
  "GAME_SERVER_ADMIN_PORT",
  "GAME_DATABASE_URL",
  "AUTH_GAME_DATABASE_URL",
  "GAME_DB_POOL_SIZE",
  "AUTH_GAME_DB_POOL_SIZE",
  "CHARACTER_MAX_EFFECTIVE_PER_ACCOUNT",
  "CHARACTER_ALLOW_DUPLICATE_NAMES",
  "CHARACTER_NAME_MIN_LENGTH",
  "CHARACTER_NAME_MAX_LENGTH",
  "CHARACTER_NAME_FORBIDDEN_WORDS",
  "CHARACTER_DEFAULT_WORLD_ID",
  "CHARACTER_DEFAULT_SCENE_ID",
  "CHARACTER_DEFAULT_X",
  "CHARACTER_DEFAULT_Y",
  "CHARACTER_DEFAULT_DIR_X",
  "CHARACTER_DEFAULT_DIR_Y",
  "CHARACTER_APPEARANCE_MAX_JSON_BYTES",
  "ALLOW_MISSING_CHARACTER_ID_TICKET",
  "TICKET_ALLOW_MISSING_CHARACTER_ID",
  "AUTH_ALLOW_MISSING_CHARACTER_ID_TICKET",
  "ALLOW_LEGACY_TICKET_WITHOUT_CHARACTER_ID",
  "TICKET_ALLOW_LEGACY_MISSING_CHARACTER_ID",
  "TICKET_SECRET",
  "GAME_ADMIN_TOKEN",
  "GAME_ADMIN_ACTOR",
  "GAME_ADMIN_WRITE_TIMEOUT_MS",
  "INTERNAL_API_TOKEN"
];

async function withEnv(env, fn) {
  const previous = new Map(CONFIG_ENV_KEYS.map((key) => [key, process.env[key]]));
  const previousCwd = process.cwd();
  const tempCwd = fs.mkdtempSync(path.join(os.tmpdir(), "auth-http-config-test-"));
  for (const key of CONFIG_ENV_KEYS) {
    delete process.env[key];
  }
  Object.assign(process.env, env);
  process.chdir(tempCwd);

  try {
    const mod = await import(`./config.js?test=${Date.now()}-${Math.random()}`);
    await fn(mod.getConfig());
  } finally {
    process.chdir(previousCwd);
    fs.rmSync(tempCwd, { recursive: true, force: true });
    for (const key of CONFIG_ENV_KEYS) {
      const value = previous.get(key);
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
  }
}

async function withCapturedWarnings(env, fn) {
  const warnings = [];
  const originalWarn = console.warn;
  console.warn = (...args) => {
    warnings.push(args.join(" "));
  };

  try {
    return await withEnv(env, (config) => fn(config, warnings));
  } finally {
    console.warn = originalWarn;
  }
}

test("auth-http keeps TLS enforcement disabled by default in development", async () => {
  await withEnv({ NODE_ENV: "development" }, (config) => {
    assert.equal(config.authRequireTls, false);
  });
});

test("auth-http ignores retired game-server write credential settings", async () => {
  await withEnv({
    GAME_ADMIN_ACTOR: "auth-http",
    GAME_ADMIN_WRITE_TIMEOUT_MS: "10"
  }, (config) => {
    assert.equal(Object.hasOwn(config, "gameAdminActor"), false);
    assert.equal(Object.hasOwn(config, "gameAdminWriteTimeoutMs"), false);
  });
});

test("auth-http requires TLS by default in production", async () => {
  await withEnv({
    NODE_ENV: "production",
    REGISTRY_ENABLED: "true",
    TICKET_SECRET: "prod-ticket-secret-with-enough-entropy",
    GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
    INTERNAL_API_TOKEN: "prod-internal-api-token-with-enough-entropy"
  }, (config) => {
    assert.equal(config.authRequireTls, true);
  });
});

test("auth-http requires registry discovery by default in production", async () => {
  await withEnv({
    NODE_ENV: "production",
    REGISTRY_ENABLED: "true",
    TICKET_SECRET: "prod-ticket-secret-with-enough-entropy",
    GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
    INTERNAL_API_TOKEN: "prod-internal-api-token-with-enough-entropy"
  }, (config) => {
    assert.equal(config.registryDiscoveryRequired, true);
  });
});

test("auth-http hides internal service endpoints by default in production", async () => {
  await withEnv({
    NODE_ENV: "production",
    REGISTRY_ENABLED: "true",
    TICKET_SECRET: "prod-ticket-secret-with-enough-entropy",
    GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
    INTERNAL_API_TOKEN: "prod-internal-api-token-with-enough-entropy"
  }, (config) => {
    assert.equal(config.authExposeInternalServiceEndpoints, false);
  });
});

test("auth-http requires registry discovery by default in test", async () => {
  await withEnv({ NODE_ENV: "test", REGISTRY_ENABLED: "true" }, (config) => {
    assert.equal(config.registryDiscoveryRequired, true);
  });
});

test("auth-http hides internal service endpoints outside production by default", async () => {
  await withEnv({ NODE_ENV: "development" }, (config) => {
    assert.equal(config.authExposeInternalServiceEndpoints, false);
  });
});

test("auth-http internal service endpoint exposure can be explicitly enabled outside production", async () => {
  await withEnv({
    NODE_ENV: "development",
    AUTH_EXPOSE_INTERNAL_SERVICE_ENDPOINTS: "true"
  }, (config) => {
    assert.equal(config.authExposeInternalServiceEndpoints, true);
  });
});

test("auth-http rejects internal service endpoint exposure in production", async () => {
  await withEnv({
    NODE_ENV: "production",
    REGISTRY_ENABLED: "true",
    AUTH_EXPOSE_INTERNAL_SERVICE_ENDPOINTS: "true",
    TICKET_SECRET: "prod-ticket-secret-with-enough-entropy",
    GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
    INTERNAL_API_TOKEN: "prod-internal-api-token-with-enough-entropy"
  }, (config) => {
    assert.equal(config.authExposeInternalServiceEndpoints, false);
  });
});

test("auth-http production discovery requirement cannot be disabled", async () => {
  await assert.rejects(
    () => withEnv({
      NODE_ENV: "production",
      DISCOVERY_REQUIRED: "false",
      REGISTRY_ENABLED: "false",
      TICKET_SECRET: "prod-ticket-secret-with-enough-entropy",
      GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
      INTERNAL_API_TOKEN: "prod-internal-api-token-with-enough-entropy"
    }, () => {}),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});

test("auth-http rejects required discovery when registry is disabled", async () => {
  await assert.rejects(
    () => withEnv({
      NODE_ENV: "development",
      REGISTRY_ENABLED: "false",
      DISCOVERY_REQUIRED: "true"
    }, () => {}),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});

test("auth-http test environment rejects registry disabled", async () => {
  await assert.rejects(
    () => withEnv({
      NODE_ENV: "test",
      REGISTRY_ENABLED: "false"
    }, () => {}),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});

test("auth-http test environment ignores DISCOVERY_REQUIRED=false override", async () => {
  await assert.rejects(
    () => withEnv({
      NODE_ENV: "test",
      DISCOVERY_REQUIRED: "false",
      REGISTRY_ENABLED: "false"
    }, () => {}),
    /DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true/
  );
});

test("auth-http accepts required discovery when registry is enabled", async () => {
  await withEnv({
    NODE_ENV: "development",
    REGISTRY_ENABLED: "true",
    DISCOVERY_REQUIRED: "true"
  }, (config) => {
    assert.equal(config.registryDiscoveryEnabled, true);
    assert.equal(config.registryDiscoveryRequired, true);
  });
});

test("auth-http reads game-proxy host and port as local fallback config", async () => {
  await withEnv({
    NODE_ENV: "development",
    REGISTRY_ENABLED: "false",
    DISCOVERY_REQUIRED: "false",
    GAME_PROXY_HOST: "127.0.0.2",
    GAME_PROXY_PORT: "4100"
  }, (config) => {
    assert.equal(config.registryDiscoveryEnabled, false);
    assert.equal(config.registryDiscoveryRequired, false);
    assert.equal(config.gameProxyHost, "127.0.0.2");
    assert.equal(config.gameProxyPort, 4100);
  });
});

test("auth-http rejects legacy direct endpoints in strict discovery environments", async () => {
  const strictCases = [
    { NODE_ENV: "test" },
    { NODE_ENV: "testing" },
    { NODE_ENV: "staging" },
    { NODE_ENV: "prod" },
    { NODE_ENV: "production" },
    { APP_ENV: "test" },
    { APP_ENV: "staging" },
    { APP_ENV: "prod" },
    { APP_ENV: "production" },
    { APP_ENV: "testing" },
    { NODE_ENV: "development", DISCOVERY_REQUIRED: "true" }
  ];

  for (const strictEnv of strictCases) {
    await assert.rejects(
      () => withEnv({
        ...strictEnv,
        REGISTRY_ENABLED: "true",
        GAME_PROXY_HOST: "203.0.113.10",
        GAME_PROXY_PORT: "4100",
        GAME_SERVER_ADMIN_HOST: "203.0.113.20",
        GAME_SERVER_ADMIN_PORT: "17500"
      }, () => {}),
      /strict service discovery forbids legacy direct config: GAME_PROXY_HOST, GAME_PROXY_PORT, GAME_SERVER_ADMIN_HOST, GAME_SERVER_ADMIN_PORT/
    );
  }
});

test("auth-http does not warn for local fallback direct endpoint env", async () => {
  await withCapturedWarnings({
    NODE_ENV: "development",
    GAME_PROXY_HOST: "127.0.0.2",
    GAME_SERVER_ADMIN_HOST: "127.0.0.3"
  }, (config, warnings) => {
    assert.equal(config.localDiscoveryFallbackEnabled, true);
    assert.deepEqual(config.legacyDirectConfigWarnings, []);
    assert.deepEqual(warnings, []);
  });
});

test("auth-http allows legacy direct endpoints with APP_ENV local", async () => {
  await withCapturedWarnings({
    APP_ENV: "local",
    GAME_PROXY_HOST: "127.0.0.2",
    GAME_PROXY_PORT: "4100",
    GAME_SERVER_ADMIN_HOST: "127.0.0.3"
  }, (config, warnings) => {
    assert.equal(config.localDiscoveryFallbackEnabled, true);
    assert.equal(config.gameProxyHost, "127.0.0.2");
    assert.equal(config.gameProxyPort, 4100);
    assert.equal(config.gameServerAdminHost, "127.0.0.3");
    assert.deepEqual(config.legacyDirectConfigWarnings, []);
    assert.deepEqual(warnings, []);
  });
});

test("auth-http ignores legacy direct endpoints when APP_ENV is development without NODE_ENV development", async () => {
  await withCapturedWarnings({
    APP_ENV: "development",
    GAME_PROXY_HOST: "203.0.113.10",
    GAME_PROXY_PORT: "4100"
  }, (config, warnings) => {
    assert.equal(config.localDiscoveryFallbackEnabled, false);
    assert.equal(config.gameProxyHost, "127.0.0.1");
    assert.equal(config.gameProxyPort, 4000);
    assert.deepEqual(
      config.legacyDirectConfigWarnings.map((warning) => warning.name),
      ["GAME_PROXY_HOST", "GAME_PROXY_PORT"]
    );
    assert.equal(warnings.length, 2);
  });
});

test("auth-http rejects legacy direct config when migration complete switch is enabled", async () => {
  await assert.rejects(
    () => withEnv({
      NODE_ENV: "development",
      DISALLOW_LEGACY_DIRECT_CONFIG: "true",
      GAME_PROXY_HOST: "127.0.0.2",
      GAME_SERVER_ADMIN_PORT: "17500"
    }, () => {}),
    /DISALLOW_LEGACY_DIRECT_CONFIG=true forbids legacy direct config: GAME_PROXY_HOST, GAME_SERVER_ADMIN_PORT/
  );
});

test("auth-http test environment rejects legacy direct config when migration complete switch is enabled", async () => {
  await assert.rejects(
    () => withEnv({
      NODE_ENV: "test",
      REGISTRY_ENABLED: "true",
      DISCOVERY_REQUIRED: "true",
      DISALLOW_LEGACY_DIRECT_CONFIG: "true",
      GAME_PROXY_HOST: "127.0.0.2",
      GAME_SERVER_ADMIN_HOST: "127.0.0.3"
    }, () => {}),
    /DISALLOW_LEGACY_DIRECT_CONFIG=true forbids legacy direct config: GAME_PROXY_HOST, GAME_SERVER_ADMIN_HOST/
  );
});

test("auth-http accepts migration complete switch when legacy direct config is absent", async () => {
  await withEnv({
    NODE_ENV: "development",
    DISALLOW_LEGACY_DIRECT_CONFIG: "true"
  }, (config) => {
    assert.equal(config.disallowLegacyDirectConfig, true);
    assert.deepEqual(config.legacyDirectConfigWarnings, []);
  });
});

test("auth-http requires TLS by default when APP_ENV is production", async () => {
  await withEnv({
    APP_ENV: "production",
    REGISTRY_ENABLED: "true",
    TICKET_SECRET: "prod-ticket-secret-with-enough-entropy",
    GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
    INTERNAL_API_TOKEN: "prod-internal-api-token-with-enough-entropy"
  }, (config) => {
    assert.equal(config.authRequireTls, true);
  });
});

test("auth-http TLS enforcement can be explicitly disabled for test deployments", async () => {
  await withEnv({
    NODE_ENV: "production",
    AUTH_REQUIRE_TLS: "false",
    REGISTRY_ENABLED: "true",
    TICKET_SECRET: "prod-ticket-secret-with-enough-entropy",
    GAME_ADMIN_TOKEN: "prod-game-admin-token-with-enough-entropy",
    INTERNAL_API_TOKEN: "prod-internal-api-token-with-enough-entropy"
  }, (config) => {
    assert.equal(config.authRequireTls, false);
  });
});

test("auth-http reads trusted proxy list for forwarded proto checks", async () => {
  await withEnv({
    NODE_ENV: "development",
    TRUST_PROXY: "true",
    TRUSTED_PROXIES: "127.0.0.1,10.0.0.0/24"
  }, (config) => {
    assert.equal(config.trustProxy, true);
    assert.deepEqual(config.trustedProxies, ["127.0.0.1", "10.0.0.0/24"]);
  });
});

test("auth-http reads password registration review switch", async () => {
  await withEnv({
    NODE_ENV: "development",
    AUTH_REGISTER_REQUIRE_REVIEW: "true"
  }, (config) => {
    assert.equal(config.registerRequireReview, true);
  });
});

test("auth-http character config defaults match P0 identity split design", async () => {
  await withEnv({ NODE_ENV: "development" }, (config) => {
    assert.equal(config.characterMaxEffectivePerAccount, 6);
    assert.equal(config.characterAllowDuplicateNames, true);
    assert.equal(config.characterNameMinLength, 2);
    assert.equal(config.characterNameMaxLength, 16);
    assert.deepEqual(config.characterNameForbiddenWords, []);
    assert.equal(config.characterDefaultWorldId, 0);
    assert.equal(config.characterDefaultSceneId, 100);
    assert.equal(config.characterDefaultX, 0);
    assert.equal(config.characterDefaultY, 0);
    assert.equal(config.characterDefaultDirX, 0);
    assert.equal(config.characterDefaultDirY, 1);
    assert.equal(config.characterAppearanceMaxJsonBytes, 4096);
  });
});

test("auth-http reads explicit character config overrides", async () => {
  await withEnv({
    NODE_ENV: "development",
    CHARACTER_MAX_EFFECTIVE_PER_ACCOUNT: "9",
    CHARACTER_ALLOW_DUPLICATE_NAMES: "false",
    CHARACTER_NAME_MIN_LENGTH: "3",
    CHARACTER_NAME_MAX_LENGTH: "20",
    CHARACTER_NAME_FORBIDDEN_WORDS: "gm, admin ",
    CHARACTER_DEFAULT_WORLD_ID: "2",
    CHARACTER_DEFAULT_SCENE_ID: "300",
    CHARACTER_DEFAULT_X: "10.5",
    CHARACTER_DEFAULT_Y: "-20.25",
    CHARACTER_DEFAULT_DIR_X: "1",
    CHARACTER_DEFAULT_DIR_Y: "0",
    CHARACTER_APPEARANCE_MAX_JSON_BYTES: "8192"
  }, (config) => {
    assert.equal(config.characterMaxEffectivePerAccount, 9);
    assert.equal(config.characterAllowDuplicateNames, false);
    assert.equal(config.characterNameMinLength, 3);
    assert.equal(config.characterNameMaxLength, 20);
    assert.deepEqual(config.characterNameForbiddenWords, ["gm", "admin"]);
    assert.equal(config.characterDefaultWorldId, 2);
    assert.equal(config.characterDefaultSceneId, 300);
    assert.equal(config.characterDefaultX, 10.5);
    assert.equal(config.characterDefaultY, -20.25);
    assert.equal(config.characterDefaultDirX, 1);
    assert.equal(config.characterDefaultDirY, 0);
    assert.equal(config.characterAppearanceMaxJsonBytes, 8192);
  });
});

test("auth-http rejects invalid character config with clear startup errors", async () => {
  const invalidCases = [
    [
      { CHARACTER_MAX_EFFECTIVE_PER_ACCOUNT: "0" },
      /CHARACTER_MAX_EFFECTIVE_PER_ACCOUNT must be a positive integer/
    ],
    [
      { CHARACTER_ALLOW_DUPLICATE_NAMES: "maybe" },
      /CHARACTER_ALLOW_DUPLICATE_NAMES must be true, false, 1, or 0/
    ],
    [
      { CHARACTER_NAME_MIN_LENGTH: "8", CHARACTER_NAME_MAX_LENGTH: "4" },
      /CHARACTER_NAME_MIN_LENGTH must be less than or equal to CHARACTER_NAME_MAX_LENGTH/
    ],
    [
      { CHARACTER_NAME_MAX_LENGTH: "65" },
      /CHARACTER_NAME_MAX_LENGTH must be at most 64 characters/
    ],
    [
      { CHARACTER_DEFAULT_WORLD_ID: "-1" },
      /CHARACTER_DEFAULT_WORLD_ID must be a non-negative integer/
    ],
    [
      { CHARACTER_DEFAULT_SCENE_ID: "NaN" },
      /CHARACTER_DEFAULT_SCENE_ID must be a non-negative integer/
    ],
    [
      { CHARACTER_DEFAULT_X: "Infinity" },
      /CHARACTER_DEFAULT_X must be a finite number/
    ],
    [
      { CHARACTER_APPEARANCE_MAX_JSON_BYTES: "0" },
      /CHARACTER_APPEARANCE_MAX_JSON_BYTES must be a positive integer/
    ]
  ];

  for (const [env, expectedError] of invalidCases) {
    await assert.rejects(
      () => withEnv({ NODE_ENV: "development", ...env }, () => {}),
      expectedError
    );
  }
});

test("auth-http rejects legacy ticket configs that allow missing characterId", async () => {
  for (const switchName of [
    "ALLOW_MISSING_CHARACTER_ID_TICKET",
    "TICKET_ALLOW_MISSING_CHARACTER_ID",
    "AUTH_ALLOW_MISSING_CHARACTER_ID_TICKET",
    "ALLOW_LEGACY_TICKET_WITHOUT_CHARACTER_ID",
    "TICKET_ALLOW_LEGACY_MISSING_CHARACTER_ID"
  ]) {
    await assert.rejects(
      () => withEnv({ NODE_ENV: "development", [switchName]: "true" }, () => {}),
      new RegExp(`missing-characterId game tickets are not supported.*${switchName}`)
    );
  }
});

test("auth-http reads service identity and build version", async () => {
  await withEnv({
    NODE_ENV: "development",
    SERVICE_NAME: "auth-http-blue",
    SERVICE_INSTANCE_ID: "auth-http-blue-001",
    SERVICE_ZONE: "zone-a",
    SERVICE_BUILD_VERSION: "2026.06.18+auth"
  }, (config) => {
    assert.equal(config.serviceName, "auth-http-blue");
    assert.equal(config.serviceInstanceId, "auth-http-blue-001");
    assert.equal(config.serviceZone, "zone-a");
    assert.equal(config.serviceBuildVersion, "2026.06.18+auth");
  });
});

test("auth-http separates bind host from advertised registry host", async () => {
  await withEnv({
    NODE_ENV: "development",
    SERVICE_BIND_HOST: "0.0.0.0",
    SERVICE_PUBLIC_HOST: "10.0.0.10",
    HOST: "127.0.0.9"
  }, (config) => {
    assert.equal(config.host, "0.0.0.0");
    assert.equal(config.bindHost, "0.0.0.0");
    assert.equal(config.advertisedHost, "10.0.0.10");
  });

  await withEnv({
    NODE_ENV: "development",
    SERVICE_BIND_HOST: "0.0.0.0"
  }, (config) => {
    assert.equal(config.advertisedHost, "127.0.0.1");
  });

  await withEnv({
    NODE_ENV: "development",
    HOST: "0.0.0.0"
  }, (config) => {
    assert.equal(config.host, "0.0.0.0");
    assert.equal(config.advertisedHost, "127.0.0.1");
  });
});

test("auth-http reads registry key prefix with Redis prefix fallback", async () => {
  await withEnv({
    NODE_ENV: "development",
    REDIS_KEY_PREFIX: "redis:",
    REGISTRY_KEY_PREFIX: "registry:"
  }, (config) => {
    assert.equal(config.registryKeyPrefix, "registry:");
  });

  await withEnv({
    NODE_ENV: "development",
    REDIS_KEY_PREFIX: "redis:"
  }, (config) => {
    assert.equal(config.registryKeyPrefix, "redis:");
  });
});

test("auth-http service identity defaults to auth-http dev build", async () => {
  await withEnv({ NODE_ENV: "development" }, (config) => {
    assert.equal(config.serviceName, "auth-http");
    assert.equal(config.serviceInstanceId, "auth-http-001");
    assert.equal(config.serviceZone, "local");
    assert.equal(config.serviceBuildVersion, "dev");
  });
});

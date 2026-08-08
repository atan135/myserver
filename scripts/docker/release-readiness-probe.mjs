if (process.argv[2] === "--extract-game-server-instance-id") {
  try {
    const chunks = [];
    for await (const chunk of process.stdin) chunks.push(chunk);
    const config = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    const instanceId = config?.services?.["game-server"]?.environment?.SERVICE_INSTANCE_ID;
    if (typeof instanceId !== "string" || !/^[A-Za-z0-9_.:-]{1,128}$/.test(instanceId)) {
      throw new Error("invalid resolved instance ID");
    }
    process.stdout.write(`${instanceId}\n`);
    process.exit(0);
  } catch {
    process.stderr.write("unable to extract resolved game-server instance ID\n");
    process.exit(65);
  }
}

const targets = [
  ["auth-http", "auth-http-1", "MYSERVER_DB_DEPLOY_AUTH_HTTP_READINESS_URL", "health"],
  ["admin-api", "admin-api-1", "MYSERVER_DB_DEPLOY_ADMIN_API_READINESS_URL", "health"],
  ["game-server", process.env.MYSERVER_RELEASE_GAME_SERVER_INSTANCE_ID || "game-server-1", "MYSERVER_DB_DEPLOY_GAME_SERVER_READINESS_URL", "ready"],
  ["game-proxy", "game-proxy-1", "MYSERVER_DB_DEPLOY_GAME_PROXY_READINESS_URL", "ready"],
  ["match-service", "match-service-1", "MYSERVER_RELEASE_MATCH_SERVICE_READINESS_URL", "ready"],
  ["chat-server", "chat-server-1", "MYSERVER_DB_DEPLOY_CHAT_SERVER_READINESS_URL", "ready"],
  ["announce-service", "announce-service-1", "MYSERVER_DB_DEPLOY_ANNOUNCE_SERVICE_READINESS_URL", "health"],
  ["mail-service", "mail-service-1", "MYSERVER_DB_DEPLOY_MAIL_SERVICE_READINESS_URL", "health"]
];

const safeToken = (value, fallback) =>
  typeof value === "string" && /^[A-Za-z0-9_.:-]{1,128}$/.test(value) ? value : fallback;

function safeDependencies(value) {
  if (!Array.isArray(value)) return [];
  return value.slice(0, 32).map((dependency) => ({
    dependency: safeToken(dependency?.dependency, "unknown"),
    requirement: safeToken(dependency?.requirement, "unknown"),
    status: safeToken(dependency?.status, "unknown"),
    errorCode: safeToken(dependency?.error_code, "")
  }));
}

async function probe([service, expectedInstanceId, environmentName, kind]) {
  const endpoint = process.env[environmentName];
  const base = {
    service,
    instanceId: expectedInstanceId,
    dependencyState: "unreachable",
    errorCode: "READINESS_UNREACHABLE",
    dependencies: []
  };
  if (!endpoint) {
    return { ...base, dependencyState: "invalid_config", errorCode: "READINESS_NOT_CONFIGURED" };
  }

  try {
    const response = await fetch(endpoint, { signal: AbortSignal.timeout(4_000) });
    let body;
    try {
      body = await response.json();
    } catch {
      return { ...base, dependencyState: "invalid_response", errorCode: "READINESS_INVALID_JSON" };
    }
    const reportedService = safeToken(body?.service, "");
    const reportedInstanceId = safeToken(body?.instance_id, expectedInstanceId);
    const serviceMatches = reportedService === service;
    const instanceMatches = kind !== "ready" || reportedInstanceId === expectedInstanceId;
    const payloadReady = kind === "ready" ? body?.ready === true : body?.ok === true;
    const ready = response.ok && payloadReady && serviceMatches && instanceMatches;
    const dependencies = safeDependencies(body?.dependencies);
    const firstDependencyError = dependencies.find((dependency) => dependency.errorCode)?.errorCode;
    return {
      service,
      instanceId: reportedInstanceId,
      dependencyState: ready ? "ready" : safeToken(body?.state, "not_ready"),
      errorCode: ready
        ? ""
        : firstDependencyError || (!serviceMatches || !instanceMatches
          ? "READINESS_IDENTITY_MISMATCH"
          : response.ok
            ? "READINESS_PAYLOAD_NOT_READY"
            : `READINESS_HTTP_${response.status}`),
      dependencies,
      ready
    };
  } catch {
    return base;
  }
}

const services = await Promise.all(targets.map(probe));
const ready = services.every((service) => service.ready === true);
process.stdout.write(`${JSON.stringify({ ready, services })}\n`);
process.exitCode = ready ? 0 : 1;

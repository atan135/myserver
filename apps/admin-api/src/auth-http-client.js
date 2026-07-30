import { discoverAuthHttpInternalEndpoints } from "./registry-client.js";

const AUTH_HTTP_INTERNAL_PATH = "/api/v1/internal/characters";

function createAuthHttpError(code, message = code, statusCode = 502, details = {}) {
  const error = new Error(message);
  error.code = code;
  error.statusCode = statusCode;
  Object.assign(error, details);
  return error;
}

function endpointUrl(endpoint) {
  const protocol = endpoint.protocol === "https" ? "https" : "http";
  const host = String(endpoint.host || "").includes(":")
    ? `[${String(endpoint.host).replace(/^\[|\]$/g, "")}]`
    : endpoint.host;
  return new URL(AUTH_HTTP_INTERNAL_PATH, `${protocol}://${host}:${endpoint.port}`).toString();
}

async function readJsonResponse(response) {
  const text = await response.text();
  if (!text) {
    return {};
  }
  try {
    return JSON.parse(text);
  } catch {
    throw createAuthHttpError(
      "AUTH_HTTP_INVALID_RESPONSE",
      "auth-http returned an invalid JSON response",
      502,
      { downstreamStatus: response.status }
    );
  }
}

export class AuthHttpClient {
  constructor(config, redis = null, fetchImpl = globalThis.fetch) {
    this.config = config;
    this.redis = redis;
    this.fetchImpl = fetchImpl;
  }

  async listInternalEndpoints() {
    if (!this.config.registryDiscoveryEnabled) {
      if (this.config.registryDiscoveryRequired || !this.config.localDiscoveryFallbackEnabled) {
        throw createAuthHttpError(
          "SERVICE_DISCOVERY_REQUIRED",
          "Required registry discovery failed: REGISTRY_ENABLED=false",
          503
        );
      }

      return [{
        service: "auth-http",
        instanceId: "local-fallback",
        instance_id: "local-fallback",
        endpointName: "internal",
        endpoint_name: "internal",
        protocol: "http",
        host: "127.0.0.1",
        port: 3000,
        healthy: true,
        fallback: true,
        source: "fallback",
        reason: "fallback_used"
      }];
    }

    if (!this.redis) {
      throw createAuthHttpError(
        "SERVICE_DISCOVERY_UNAVAILABLE",
        "Redis client is required for auth-http discovery",
        503
      );
    }

    return discoverAuthHttpInternalEndpoints(this.redis, this.config);
  }

  async resolveInternalEndpoint(options = {}) {
    if (options.endpoint) {
      return options.endpoint;
    }

    const endpoints = await this.listInternalEndpoints();
    const endpoint = endpoints.find((candidate) => candidate.healthy !== false);
    if (!endpoint) {
      throw createAuthHttpError(
        "AUTH_HTTP_INTERNAL_ENDPOINT_NOT_FOUND",
        "auth-http internal endpoint not found in service registry",
        503
      );
    }
    return endpoint;
  }

  async createCharacterForAdmin(payload, options = {}) {
    const token = String(this.config.internalApiToken || "").trim();
    if (!token) {
      throw createAuthHttpError(
        "INTERNAL_API_TOKEN_REQUIRED",
        "INTERNAL_API_TOKEN is required for auth-http internal requests",
        503
      );
    }

    const endpoint = await this.resolveInternalEndpoint(options);
    const controller = new AbortController();
    const timeout = setTimeout(
      () => controller.abort(),
      this.config.authHttpRequestTimeoutMs || 3000
    );

    let response;
    let body;
    try {
      const headers = {
        "content-type": "application/json",
        "x-service-token": token
      };
      if (options.requestId) {
        headers["x-request-id"] = options.requestId;
      }
      response = await this.fetchImpl(endpointUrl(endpoint), {
        method: "POST",
        headers,
        body: JSON.stringify(payload),
        signal: controller.signal
      });
      body = await readJsonResponse(response);
    } catch (error) {
      if (controller.signal.aborted || error?.name === "AbortError") {
        throw createAuthHttpError("AUTH_HTTP_TIMEOUT", "auth-http request timed out", 504);
      }
      if (error?.code === "AUTH_HTTP_INVALID_RESPONSE") {
        throw error;
      }
      throw createAuthHttpError(
        "AUTH_HTTP_UNAVAILABLE",
        error?.message || "auth-http request failed",
        502,
        { cause: error }
      );
    } finally {
      clearTimeout(timeout);
    }

    if (!response.ok) {
      throw createAuthHttpError(
        body?.error || "AUTH_HTTP_REQUEST_FAILED",
        body?.message || `auth-http request failed with status ${response.status}`,
        response.status,
        { downstreamStatus: response.status, downstreamBody: body }
      );
    }
    if (body?.ok !== true || !body?.character) {
      throw createAuthHttpError(
        "AUTH_HTTP_INVALID_RESPONSE",
        "auth-http character creation response is incomplete",
        502
      );
    }

    return body.character;
  }
}

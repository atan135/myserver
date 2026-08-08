#!/usr/bin/env bash
# Sourced by release and operations runners. Callers provide release_compose_command().

readonly RELEASE_READINESS_TIMEOUT_SECONDS_DEFAULT=180
readonly RELEASE_READY_STABILITY_SECONDS_DEFAULT=10
readonly RELEASE_REGISTRY_HEARTBEAT_TTL_SECONDS=30
readonly RELEASE_READINESS_POLL_SECONDS=2

validate_release_readiness_window() {
  local timeout_seconds="${1:-$RELEASE_READINESS_TIMEOUT_SECONDS_DEFAULT}"
  local stability_seconds="${2:-$RELEASE_READY_STABILITY_SECONDS_DEFAULT}"
  local registry_ttl_seconds="${3:-$RELEASE_REGISTRY_HEARTBEAT_TTL_SECONDS}"
  [[ "$timeout_seconds" =~ ^[1-9][0-9]*$ ]] || return 64
  [[ "$stability_seconds" =~ ^[1-9][0-9]*$ ]] || return 64
  [[ "$registry_ttl_seconds" =~ ^[1-9][0-9]*$ ]] || return 64

  local required_stable_seconds=$((registry_ttl_seconds + stability_seconds))
  if (( timeout_seconds <= required_stable_seconds )); then
    printf 'readiness_config_error error_code=READINESS_WINDOW_INVALID\n' >&2
    return 64
  fi
}

release_readiness_probe() {
  local -a probe_options=(
    --profile ops run --rm --no-deps --entrypoint node
    -e MYSERVER_DB_DEPLOY_AUTH_HTTP_READINESS_URL=http://auth-http:3000/healthz
    -e MYSERVER_DB_DEPLOY_ADMIN_API_READINESS_URL=http://admin-api:3001/healthz
    -e MYSERVER_DB_DEPLOY_GAME_SERVER_READINESS_URL=http://game-server:7600/readyz
    -e "MYSERVER_RELEASE_GAME_SERVER_INSTANCE_ID=${MYSERVER_RELEASE_GAME_SERVER_INSTANCE_ID:-game-server-1}"
    -e MYSERVER_DB_DEPLOY_GAME_PROXY_READINESS_URL=http://game-proxy:7601/readyz
    -e MYSERVER_RELEASE_MATCH_SERVICE_READINESS_URL=http://match-service:7603/readyz
    -e MYSERVER_DB_DEPLOY_CHAT_SERVER_READINESS_URL=http://chat-server:7602/readyz
    -e MYSERVER_DB_DEPLOY_ANNOUNCE_SERVICE_READINESS_URL=http://announce-service:9004/healthz
    -e MYSERVER_DB_DEPLOY_MAIL_SERVICE_READINESS_URL=http://mail-service:9003/healthz
  )
  if [[ -n "${RELEASE_READINESS_PROBE_FILE:-}" ]]; then
    probe_options+=(--volume "${RELEASE_READINESS_PROBE_FILE}:/app/tools/release-readiness-probe.mjs:ro")
  fi
  release_compose_command "${probe_options[@]}" \
    migration-runner /app/tools/release-readiness-probe.mjs
}

release_readiness_diagnostics() {
  local diagnostics
  diagnostics="$(release_readiness_probe 2>/dev/null || true)"
  diagnostics="${diagnostics##*$'\n'}"
  if [[ "$diagnostics" == \{*\} ]]; then
    printf '%s\n' "$diagnostics" >&2
  else
    printf '%s\n' \
      '{"ready":false,"services":[{"service":"release-readiness","instanceId":"unknown","dependencyState":"probe_unavailable","errorCode":"READINESS_PROBE_UNAVAILABLE","dependencies":[]}]}' >&2
  fi
}

wait_for_release_readiness() {
  local timeout_seconds="${1:-$RELEASE_READINESS_TIMEOUT_SECONDS_DEFAULT}"
  local stability_seconds="${2:-$RELEASE_READY_STABILITY_SECONDS_DEFAULT}"
  local registry_ttl_seconds="${3:-$RELEASE_REGISTRY_HEARTBEAT_TTL_SECONDS}"
  validate_release_readiness_window \
    "$timeout_seconds" "$stability_seconds" "$registry_ttl_seconds" || return $?

  local started_at now stable_since=0
  local required_stable_seconds=$((registry_ttl_seconds + stability_seconds))
  started_at="$(date +%s)"
  printf 'readiness_convergence timeout_seconds=%s registry_ttl_seconds=%s stability_seconds=%s\n' \
    "$timeout_seconds" "$registry_ttl_seconds" "$stability_seconds"

  while true; do
    if release_readiness_probe >/dev/null 2>&1; then
      now="$(date +%s)"
      if (( stable_since == 0 )); then
        stable_since="$now"
      fi
      if (( now - stable_since >= required_stable_seconds )); then
        printf 'readiness_converged stable_seconds=%s\n' "$required_stable_seconds"
        return 0
      fi
    else
      stable_since=0
      now="$(date +%s)"
    fi
    if (( now - started_at >= timeout_seconds )); then
      printf 'readiness_timeout elapsed_seconds=%s\n' "$((now - started_at))" >&2
      release_readiness_diagnostics
      return 70
    fi
    sleep "$RELEASE_READINESS_POLL_SECONDS"
  done
}

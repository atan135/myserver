#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/initialize-production-secrets.sh --release-dir /data/myserver/release/<release-id> \
    --origin-id <1-1023> --admin-ip-allowlist <ip-or-cidr[,ip-or-cidr...]>

Creates the first-release env files in /data/myserver/secrets. It refuses to
overwrite an existing managed file and never prints generated secret values.
EOF
}

release_dir=""
origin_id=""
admin_ip_allowlist=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --release-dir)
      release_dir="${2:?--release-dir requires a value}"
      shift 2
      ;;
    --origin-id)
      origin_id="${2:?--origin-id requires a value}"
      shift 2
      ;;
    --admin-ip-allowlist)
      admin_ip_allowlist="${2:?--admin-ip-allowlist requires a value}"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 64
      ;;
  esac
done

if [ -z "$release_dir" ] || [ -z "$origin_id" ] || [ -z "$admin_ip_allowlist" ]; then
  usage >&2
  exit 64
fi
if [[ "$release_dir" != /data/myserver/release/* ]] || [ ! -f "$release_dir/compose.production.env" ]; then
  echo "--release-dir must be an extracted release under /data/myserver/release containing compose.production.env." >&2
  exit 64
fi
if ! [[ "$origin_id" =~ ^[0-9]+$ ]] || [ "$origin_id" -lt 1 ] || [ "$origin_id" -gt 1023 ]; then
  echo "--origin-id must be an integer from 1 to 1023." >&2
  exit 64
fi
if ! [[ "$admin_ip_allowlist" =~ ^[0-9A-Fa-f:.,/]+$ ]]; then
  echo "--admin-ip-allowlist must be a comma-separated list of IP addresses or CIDRs." >&2
  exit 64
fi

for command in openssl base64 tail tr install mktemp rm; do
  command -v "$command" >/dev/null || {
    echo "Required command is unavailable: $command" >&2
    exit 69
  }
done

secrets_dir="/data/myserver/secrets"
files=(
  infrastructure.env migration.env auth-http.env admin-api.env announce-service.env
  mail-service.env metrics-collector.env game-server.env game-proxy.env
  chat-server.env match-service.env
)
for file in "${files[@]}"; do
  if [ -e "$secrets_dir/$file" ]; then
    echo "Refusing to overwrite existing secret file: $secrets_dir/$file" >&2
    exit 65
  fi
done

install -d -m 0700 "$secrets_dir"
temporary_dir="$(mktemp -d)"
trap 'rm -rf "$temporary_dir"' EXIT

random_secret() {
  openssl rand -hex 32
}

create_ed25519_material() {
  local name="$1"
  local pem="$temporary_dir/$name.pem"
  openssl genpkey -algorithm ED25519 -out "$pem" >/dev/null 2>&1
  local private_key public_key
  private_key="$(base64 < "$pem" | tr -d '\n')"
  public_key="$(openssl pkey -in "$pem" -pubout -outform DER 2>/dev/null | tail -c 32 | base64 | tr -d '\n' | tr '+/' '-_' | tr -d '=')"
  if ! [[ "$public_key" =~ ^[A-Za-z0-9_-]{43}$ ]]; then
    echo "Unable to derive a raw Ed25519 public key." >&2
    exit 70
  fi
  printf '%s:%s\n' "$private_key" "$public_key"
}

IFS=: read -r admin_assertion_private_key admin_assertion_public_key <<EOF
$(create_ed25519_material admin-assertion)
EOF
IFS=: read -r mail_assertion_private_key mail_assertion_public_key <<EOF
$(create_ed25519_material mail-assertion)
EOF

postgres_password="$(random_secret)"
redis_password="$(random_secret)"
nats_token="$(random_secret)"
ticket_secret="$(random_secret)"
game_admin_token="$(random_secret)"
game_internal_token="$(random_secret)"
proxy_admin_token="$(random_secret)"
proxy_admin_read_token="$(random_secret)"
internal_api_token="$(random_secret)"
jwt_secret="$(random_secret)"
admin_password="$(random_secret)"
mail_service_token="$(random_secret)"
mail_operations_token="$(random_secret)"
mail_high_risk_token="$(random_secret)"
announce_admin_token="$(random_secret)"
announce_read_token="$(random_secret)"

postgres_url() {
  local database="$1"
  printf 'postgresql://myserver:%s@postgres:5432/%s' "$postgres_password" "$database"
}
redis_url="redis://:${redis_password}@redis:6379"
nats_url="nats://${nats_token}@nats:4222"

write_secret_file() {
  local name="$1"
  shift
  umask 077
  printf '%s\n' "$@" > "$secrets_dir/$name"
  chmod 0600 "$secrets_dir/$name"
}

write_secret_file infrastructure.env \
  'POSTGRES_USER=myserver' \
  "POSTGRES_PASSWORD=$postgres_password" \
  'POSTGRES_DB=myserver_control' \
  "REDIS_PASSWORD=$redis_password" \
  "NATS_TOKEN=$nats_token"

write_secret_file migration.env \
  "MYSERVER_DB_MIGRATION_AUTH_URL=$(postgres_url myserver_auth)" \
  "MYSERVER_DB_MIGRATION_GAME_URL=$(postgres_url myserver_game)" \
  "MYSERVER_DB_MIGRATION_CHAT_URL=$(postgres_url myserver_chat)" \
  "MYSERVER_DB_MIGRATION_ANNOUNCE_URL=$(postgres_url myserver_announce)" \
  "MYSERVER_DB_MIGRATION_MAIL_URL=$(postgres_url myserver_mail)" \
  "MYSERVER_DB_MIGRATION_METRICS_NATS_URL=$nats_url"

write_secret_file auth-http.env \
  "REDIS_URL=$redis_url" \
  "REGISTRY_URL=$redis_url" \
  "NATS_URL=$nats_url" \
  "DATABASE_URL=$(postgres_url myserver_auth)" \
  "GAME_DATABASE_URL=$(postgres_url myserver_game)" \
  "TICKET_SECRET=$ticket_secret" \
  "GAME_ADMIN_TOKEN=$game_admin_token" \
  "INTERNAL_API_TOKEN=$internal_api_token" \
  "GLOBAL_ID_ORIGIN_ID=$origin_id" \
  'GLOBAL_ID_WORKER_ID=1'

write_secret_file admin-api.env \
  "REDIS_URL=$redis_url" \
  "REGISTRY_URL=$redis_url" \
  "NATS_URL=$nats_url" \
  "DATABASE_URL=$(postgres_url myserver_auth)" \
  "GAME_DATABASE_URL=$(postgres_url myserver_game)" \
  "JWT_SECRET=$jwt_secret" \
  "GAME_ADMIN_TOKEN=$game_admin_token" \
  "GAME_PROXY_ADMIN_READ_TOKEN=$proxy_admin_read_token" \
  "ADMIN_ASSERTION_PRIVATE_KEY_BASE64=$admin_assertion_private_key" \
  "ADMIN_API_IP_ALLOWLIST=$admin_ip_allowlist" \
  'ADMIN_USERNAME=admin' \
  "ADMIN_PASSWORD=$admin_password"

write_secret_file announce-service.env \
  "REDIS_URL=$redis_url" \
  "REGISTRY_URL=$redis_url" \
  "NATS_URL=$nats_url" \
  "DATABASE_URL=$(postgres_url myserver_announce)" \
  "TICKET_SECRET=$ticket_secret" \
  "ANNOUNCE_ADMIN_TOKEN=$announce_admin_token" \
  "ANNOUNCE_READ_TOKEN=$announce_read_token" \
  "GLOBAL_ID_ORIGIN_ID=$origin_id" \
  'GLOBAL_ID_WORKER_ID=3'

write_secret_file mail-service.env \
  "REDIS_URL=$redis_url" \
  "REGISTRY_URL=$redis_url" \
  "NATS_URL=$nats_url" \
  "DATABASE_URL=$(postgres_url myserver_mail)" \
  "TICKET_SECRET=$ticket_secret" \
  "MAIL_SERVICE_TOKEN=$mail_service_token" \
  "MAIL_OPERATIONS_TOKEN=$mail_operations_token" \
  "MAIL_HIGH_RISK_TOKEN=$mail_high_risk_token" \
  "MAIL_GRANT_ASSERTION_PRIVATE_KEY_BASE64=$mail_assertion_private_key" \
  "GLOBAL_ID_ORIGIN_ID=$origin_id" \
  'GLOBAL_ID_WORKER_ID=2'

write_secret_file metrics-collector.env \
  "REDIS_URL=$redis_url" \
  "REGISTRY_URL=$redis_url" \
  "NATS_URL=$nats_url"

write_secret_file game-server.env \
  "REDIS_URL=$redis_url" \
  "REGISTRY_URL=$redis_url" \
  "NATS_URL=$nats_url" \
  "DATABASE_URL=$(postgres_url myserver_game)" \
  "TICKET_SECRET=$ticket_secret" \
  "GAME_ADMIN_TOKEN=$game_admin_token" \
  "GAME_INTERNAL_TOKEN=$game_internal_token" \
  "ADMIN_ASSERTION_PUBLIC_KEYS_JSON={\"admin-api-v1\":\"$admin_assertion_public_key\"}" \
  "MAIL_GRANT_ASSERTION_PUBLIC_KEYS_JSON={\"mail-service-v1\":\"$mail_assertion_public_key\"}" \
  "GLOBAL_ID_ORIGIN_ID=$origin_id" \
  'GLOBAL_ID_WORKER_ID=5'

write_secret_file game-proxy.env \
  "REDIS_URL=$redis_url" \
  "REGISTRY_URL=$redis_url" \
  "NATS_URL=$nats_url" \
  "PROXY_ROUTE_STORE_REDIS_URL=$redis_url" \
  "TICKET_SECRET=$ticket_secret" \
  "PROXY_ADMIN_TOKEN=$proxy_admin_token" \
  "PROXY_ADMIN_READ_TOKEN=$proxy_admin_read_token" \
  "ADMIN_ASSERTION_PUBLIC_KEYS_JSON={\"admin-api-v1\":\"$admin_assertion_public_key\"}"

write_secret_file chat-server.env \
  "REDIS_URL=$redis_url" \
  "REGISTRY_URL=$redis_url" \
  "NATS_URL=$nats_url" \
  "DATABASE_URL=$(postgres_url myserver_chat)" \
  "TICKET_SECRET=$ticket_secret" \
  "GLOBAL_ID_ORIGIN_ID=$origin_id" \
  'GLOBAL_ID_WORKER_ID=4'

write_secret_file match-service.env \
  "REDIS_URL=$redis_url" \
  "REGISTRY_URL=$redis_url" \
  "NATS_URL=$nats_url" \
  "GAME_INTERNAL_TOKEN=$game_internal_token" \
  "GLOBAL_ID_ORIGIN_ID=$origin_id" \
  'GLOBAL_ID_WORKER_ID=6'

printf 'Created %s protected env files in %s for release %s.\n' \
  "${#files[@]}" "$secrets_dir" "$(sed -n 's/^RELEASE_ID=//p' "$release_dir/compose.production.env")"
printf 'The initial admin username is admin; retrieve its generated password only from %s/admin-api.env through the approved secret-management path.\n' "$secrets_dir"

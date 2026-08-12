# MyServer Load Test

`tools/load-test` is an independent Rust package for the player core path.
The first two stages implement an offline-safe controller/worker contract,
validation, load scheduling, account manifests, `auth-http` request contracts,
guardrails, metrics, reports, and deterministic test doubles. The default
commands deliberately do not connect to an application service.

## Protocol Sources

The load tool must not keep a hand-written player protocol copy.

| Concern | Source | Consumer boundary |
| --- | --- | --- |
| Protobuf message schemas | `packages/proto/game.proto` | `tools/load-test/build.rs` uses the repository's locked `protoc-bin-vendored 3.2.0` and `prost-build 0.13.5` at build time. |
| 14-byte player packet header, message IDs and packet size checks | `packages/game-protocol` | `game-proxy`, `game-server`, and load-test test doubles consume the same crate. |
| KCP stream/no-delay profile | `packages/game-protocol::player_kcp_config` | `game-proxy` frontend is the production source; a later KCP load client must call this helper. |
| Ticket ownership and protocol-version validation | `apps/game-proxy/src/auth.rs`, `apps/game-proxy/src/protocol_version_policy.rs` | The generator treats tickets as opaque secret-provider results. Stage three will verify compatible `AuthReq` construction without reimplementing ticket verification. |
| Frame-sync scenario semantics | `tools/lockstep-client` and `packages/sim-core` | Stage three may reuse their scenario/input boundary rather than copying gameplay inputs. |

Run `npm run check:proto` after protobuf changes. The repository's existing
generator still checks application-generated Rust files; the load tool builds
its protobuf output into Cargo `OUT_DIR`, so it does not add a generated source
copy to the repository.

## Safe Local Commands

All examples use loopback endpoints and do not contact them:

```powershell
cargo run --manifest-path tools/load-test/Cargo.toml --bin loadtest -- validate --config tools/load-test/examples/local-dry-run.json
cargo run --manifest-path tools/load-test/Cargo.toml --bin loadtest -- run --config tools/load-test/examples/local-dry-run.json --dry-run
cargo run --manifest-path tools/load-test/Cargo.toml --bin account-prepare -- plan --config tools/load-test/examples/local-dry-run.json
```

`validate` performs schema, target and safety checks only. `calibrate` remains
offline-only and requires `--dry-run`. A `run --dry-run` with `scenario.auth`
uses the deterministic auth fake; it does not create a HTTP client or contact
the loopback endpoint.

`account-prepare plan` writes a manifest and a write estimate under
`prepare_reports_root/account-manifests/<environment>/<batch>/`. It does not
read a secret or make a network request. `plan` estimates registration and
character creation separately, so preparation traffic is never added to run
metrics.

The plan reports conservative HTTP-operation and potential-write estimates for
both `apply` and `verify`. Potential writes include database, session, ticket,
Redis, and audit effects, rather than only SQL rows. A live prepare command is
rejected before creating its HTTP client when the selected command's estimate
exceeds either `max_total_operations` or `max_data_writes`.

`apply` and `verify` are live operations and require all of the following:

- `--execute`
- `--confirm-write <environment>` with the exact configured environment name
- `--private-config <file>`
- the normal remote protection flags when the profile is non-local:
  `--allow-remote --confirm <environment>`, allowlists, and approval reference

`apply` registers through the supported `POST /api/v1/auth/register` endpoint,
treats `LOGIN_NAME_EXISTS` as the idempotent resume path, then logs in, checks
or creates a character, selects it, and issues a ticket. `verify` performs the
login/character/select/ticket checks without registering or creating data.
Both persist only readiness and verification state after each logical account,
so a partial failure can be resumed. Neither command performs cleanup. Cleanup
is intentionally not a prepare, verify, or run side effect.

`export` copies a validated credential-free manifest and does not contact a
service:

```powershell
cargo run --manifest-path tools/load-test/Cargo.toml --bin account-prepare -- export --config tools/load-test/examples/local-dry-run.json
```

Manifests contain only the logical `loadtest_<environment>_<batch>_<index>`
identifier, source, environment, batch, readiness, and verification timestamp.
The supported `auth-http` password account syntax uses underscores, so a batch
hyphen is converted only for the in-memory login-name projection. Credentials
are never stored in a manifest, scenario, report, or error sample. The private
configuration maps each logical ID to a declared environment-variable reference:

```json
{
  "secret_references": ["LOADTEST_ACCOUNT_000001_PASSWORD"],
  "account_credentials": {
    "loadtest_local_default_000001": "LOADTEST_ACCOUNT_000001_PASSWORD"
  }
}
```

The environment variable's value is resolved only while a guarded request is
being sent. Do not put its value in JSON or pass it on the command line.

Preparation derives a stable character name from the configured prefix, batch,
and account index. When that readable form would exceed the current default
`auth-http` 16-character limit, it uses a compact deterministic ASCII name so
apply/resume does not fail solely because of a long batch name.

An actual auth run is separately gated and is never implied by `run`:

```powershell
cargo run --manifest-path tools/load-test/Cargo.toml --bin loadtest -- run --config <config> --execute-auth --confirm-auth <environment> --account-manifest <manifest> --private-config <private-config>
```

This reaches the mature `reqwest` client only after the explicit execution and
exact environment confirmation,
manifest, private-reference, access/profile, preflight, account-pool and
same-account-session-effect checks pass. It uses the documented player routes:
login, `me`, character list/create/select, ticket issue, and logout. Retrying
is bounded and limited to explicit read-only `me` and character-list requests;
registration, creation, ticket issue, and logout are not retried automatically.
Before a live auth transport is created, its budget estimate must fit the
virtual-player, total-operation, potential-write, scenario-duration, and login
QPS limits. The operation estimate reserves all three possible attempts for
each retryable read. Runtime admission consumes the same quota before every
outbound attempt; a later quota breach aborts with `BudgetExceeded` before the
request is dispatched. Dry-runs retain their offline fake behavior and do not
consume a live write allowance.

Live auth accepts `arrival_rate`, `staged`, and `burst` load models. A staged
auth scenario treats each stage's `virtual_players` as a bounded auth-flow wave
launched at the stage boundary, not as long-lived concurrency. Every flow in
the wave must be admitted and complete before that stage's duration window
ends; a rate, budget, or deadline shortfall fails closed rather than spilling
into the next stage. `fixed_concurrency` remains rejected before manifest,
private-config, or transport setup because the synchronous executor cannot
maintain the declared concurrent flows. Dry-runs continue to support every
configured load model.

For a conservative enforcement boundary, the live `reqwest` client is fixed to
HTTP/1.1 with `Connection: close` and no idle connection pool. Every outbound
attempt is admitted as one new connection, one business message, and one
message on a single worst-case connection. The admission controller applies
`max_new_connections_per_second`, `max_business_messages_per_second`, and
`max_messages_per_connection_per_second` to that mapping, as well as login QPS
and operation/write quotas. It rechecks the stop/protection state while
waiting, and passes the remaining scenario deadline as the individual request
timeout. This deliberately does not claim to observe internal socket events.

Before `run` or `calibrate` creates a plan, schedules any player, opens a
transport, or writes a report, it prints a single `preflight=<JSON>` line. The
summary includes the environment, hashed target summaries, credential-free account
batch, planned count and manifest-presence status, selected and supported load models, effective budget/duration and
deadline, write budget, dry-run state, remote-gate state and the fail-closed
protection contract. It contains no raw endpoint, account, credential, token or
ticket value.

## Safety Contract

- Local profiles accept loopback `auth_http` and `game_proxy` targets only.
- `game_proxy` must use `kcp://`; direct `game-server`, port `7000`, and TCP
  fallback targets are rejected during static configuration validation.
- Remote profiles require `--allow-remote`, an exact `--confirm <environment>`
  value, non-empty approval reference, host allowlist, IP allowlist and DNS
  revalidation. CLI budget flags only reduce the profile's hard budgets.
- Configs cannot contain unknown fields. Private configuration contains secret
  references only, never credential values. Passwords, tokens, tickets and
  identity fields are redacted from errors and reports.
- Ctrl+C, a configured stop file, deadline and threshold signals share one
  abort controller. The controller state machine stops admission, drains for a
  bounded graceful window, forces any remaining release, then flushes metrics.
- Target protection is checked before ramping and on every controller tick. DNS,
  certificate, descriptor and environment identity must all be confirmed; a
  failed or unavailable check aborts before further admission. Stage-one remote
  dry-runs fail closed because they cannot inspect a live certificate or
  descriptor without connecting.

Each report has `run.json`, `metrics.json`, `timeseries.csv`, `errors.jsonl`
and `summary.md`. Auth runs and fake auth dry-runs also have
`auth-metrics.json`, which records login attempt QPS, login success rate,
P50/P95/P99, bounded HTTP-status and business-code categories, ticket success
rate, rate-limit and connection-failure rates, virtual player states, and the existing
generator-resource report. `metrics.json` stores compact mergeable HDR V2+DEFLATE
histogram payloads, so controller/worker merge operates on distributions,
never locally calculated percentiles. Target values are hashed, error samples
are bounded, and unavailable Windows measurements are reported as
`unavailable`, never zero.

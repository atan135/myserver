# MyServer Load Test

`tools/load-test` is an independent Rust package for the player core path.
The current implementation provides an offline-safe controller/worker contract,
validation, load scheduling, account manifests, `auth-http` request contracts,
guardrails, metrics, reports, and deterministic test doubles. The default
commands deliberately do not connect to an application service.

## Current implementation status

The package is currently an offline-safe framework with explicitly gated local
diagnostics. The following boundaries are intentional and should be kept in
mind when interpreting a report:

| Area | Available now | Boundary / not a capacity claim |
| --- | --- | --- |
| Player core path | Guarded `auth-http -> game-proxy` KCP flow, optional approved-room gameplay, and the two-account `default_match` ready/start smoke | Requires explicit execution flags, a credential-free manifest plus private secret references, and a pre-provisioned room. It is not a general multiplayer load model. |
| Chat | Deterministic WSS fake and a bounded live WebSocket runner | Live WebSocket is restricted to an explicit local/test diagnostic gate. Server persistence and push latency require a separately confirmed service smoke. |
| Match | Deterministic gRPC fake, bounded `MatchInternal` diagnostic, and match event/queue metric projection | Direct gRPC and `MatchInternal` are development/isolated diagnostics only. They do not represent the formal player `game-proxy -> game-server -> match-service -> room` capacity path. |
| Mail and announcements | Deterministic HTTP fake and bounded live list/detail/read/claim or announcement-read requests | Live HTTP is restricted to local/test. Mail notification delivery, outbox/NATS timing, and announcement writes require a separately approved service test; production writes are rejected. |
| Calibration, soak and reconnect | Offline generator calibration, bounded soak-window assessment/rolling persistence, and a guarded live reconnect-burst executor | The executor requires local/test plus explicit game gates. No real reconnect storm, live soak, resource-leak observation, or service recovery/capacity conclusion has been accepted. |
| Baseline comparison | Offline report identity/threshold comparison for throughput, P95/P99, errors and generator working set | Reports without aligned service-version observations are non-comparable; no repeated stable-environment acceptance run has been completed. |
| Distributed execution | Versioned plan, assignment, batch, heartbeat, abort and summary contracts; deterministic slicing, ledger, pending and fail-closed state machines; private mTLS gRPC controller/worker control plane | The control plane is loopback-tested only. It is not wired into a production CLI/orchestrator, and no cross-machine or real multi-worker acceptance run has been completed. |

`side_services` supports per-step `weight` and `think_time_ms`, a composite
service weight map, and global/per-player operation caps. These controls shape
deterministic plans and local diagnostics; they do not yet constitute a
production-validated user-journey model. Service registry discovery, server
metrics snapshots, PostgreSQL/Redis/NATS telemetry, and host-level observations
must be supplied by a separately confirmed environment run. Missing or stale
observations are reported as incomplete and cannot be used to infer capacity.

The default `run` and `calibrate` commands remain transport-free unless their
explicit execution gates are provided. `report` only reads an existing report
directory. A successful dry-run or deterministic fake run proves planner,
budget, protocol-correlation, classification, and redaction behavior; it does
not prove service availability or end-to-end capacity.

### Distributed metric aggregation

`MetricBatch` carries counters, mergeable HDR V2 histogram payloads, and raw
time-series samples stamped with the worker monotonic boundary. The controller
aggregator aligns batches by sequence and rejects boundary drift or samples
outside their declared window. Repeated batches are ignored by sequence ledger;
late, out-of-order batches are merged once. Counters and samples are summed at
the same boundary, while HDR payloads are merged before any P50/P95/P99 is
calculated. Percentile values are not accepted on the wire and are never
averaged across workers. A missing sequence or worker leaves the final report
incomplete, so remaining observations must not be extrapolated.

### Private controller/worker control plane

`tools/load-test/proto/loadtest_control.proto` is a private protocol rather
than a player or target-service protocol. It carries only control identity,
strict message sequencing, and JSON envelopes of the existing `RunPlan`,
`WorkerAssignment`, `MetricBatch`, `WorkerHeartbeat`, and `AbortSignal`
contracts. Player HTTP/KCP/WSS/gRPC endpoints, target descriptors, tickets,
account pools, target credentials, and business metrics transports have no
representation in this interface.

The controller listener requires a worker client certificate signed by its
configured CA. A worker credential is bound to exactly one `run_id`,
`worker_id`, and client-certificate SHA-256 fingerprint, has a maximum
15-minute lifetime, and contains a one-time registration nonce. Rotating a
credential revokes that worker's old control sessions. Once registered, a
worker can only read its own assignment, submit its own metric batches and
heartbeats, and read the abort signal for its own run. Unsupported protocol
versions, expired or rotated credentials, cross-worker/run requests, replayed
or out-of-order control messages, and duplicate metric batches are rejected.
Any RPC or malformed control response makes the worker client fail closed.

Controller and worker TLS material use separate types and redact PEM/key bytes
from debug output. The implemented tests generate certificates in memory and
exercise mTLS on a local loopback listener. This is not a CLI startup path or
evidence of a real cross-machine, multi-worker, remote, or production run.

This exact private proto is registered in
`packages/proto/compatibility/inventory.json` under `localProtocols`. Its
semantic snapshot is maintained separately at
`tools/load-test/proto/compatibility/baseline.json`, and is deliberately not
included in the shared/player `packages/proto` release reference or breaking
comparison. The v1 boundary is the `protocol_version` field together with
`CONTROL_PROTOCOL_VERSION`. The inventory records this boundary as
`currentVersion`, validates the `uint32` field declaration and requires the
Rust constant to have the same value. A wire-incompatible change must increase
both versions before `--write` can create a reviewed local baseline; either a
missing upgrade or an upgraded-but-unwritten boundary fails `--check`.

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

`calibrate --dry-run` performs a local, transport-free 1/2/4/... progressive
generator probe up to the profile virtual-player limit. Each level drives a
fixed-cadence synthetic scheduler through a bounded monotonic time window and
records its planned, scheduled, and dropped actions, maximum queue depth, and
a deterministic work checksum alongside Windows process CPU, working set,
scheduler lag, and metric-channel drops. The sum of all levels is rejected
before execution if it exceeds either `max_total_operations` or
`max_duration_secs`. It stops before the next level when CPU, memory,
scheduler lag, or metric-drop thresholds are met; unavailable CPU, memory,
lag, or drop measurements are fail-closed and are never interpreted as zero.
The default thresholds reserve 20% of the highest stable generator level for
normal test runs. Profiles may tighten or otherwise set these explicit
thresholds with a `calibration` object:

```json
{
  "calibration": {
    "max_cpu_utilization_basis_points": 8000,
    "max_working_set_bytes": 2147483648,
    "max_scheduler_lag_ms": 50,
    "max_metrics_dropped": 0,
    "reserve_percent": 20,
    "level_window_ms": 100,
    "tick_interval_ms": 25
  }
}
```

Calibration reports always distinguish three conclusions: generator capacity,
service stable capacity, and system burst capacity. Offline calibration has no
server observation, so both service conclusions are explicitly `unavailable`.
Even with live observations added later, a service capacity conclusion remains
unavailable if service telemetry is incomplete or the generator saturated
first. This prevents a generator bottleneck from being reported as server
capacity.

The offline reconnect-burst planner produces a bounded trace of
`login -> issue-ticket -> KCP proxy connect -> proxy auth -> room reconnect`
steps. It applies the configured login-QPS and new-connection-per-second hard
limits before creating the trace, enforces `max_virtual_players`,
`max_total_operations`, `max_data_writes`, global business-message and
per-connection message rates, and `max_duration_secs`, and uses the shared KCP
reconnect policy's exponential backoff. Login and ticket issuance reserve the
same conservative potential-write upper bounds as the auth admission layer. It intentionally carries player slots
only, never account IDs, character IDs, or ticket values. The deterministic
fake exercises lifecycle transitions and backoff; it does not validate a real
session, Redis ticket owner, proxy routing, or game-server room recovery.

### Opt-in live room gameplay

The normal scenario leaves `scenario.live_gameplay` absent, so a guarded live
run remains `AuthReq -> AuthRes -> PingReq -> PingRes -> close`. Room, frame,
and reconnect traffic is deliberately opt-in and requires all of the following:

- an explicitly approved, pre-provisioned `room_id` and `policy_id`;
- `writes_data: true` and a non-idle `profile`;
- a bounded lockstep JSON payload (64 KiB maximum) and `max_frame_inputs` in
  `1..=8`;
- optional reconnect with exactly one reconnect attempt and an explicitly
  supplied push cursor.

The runner sends only the planned join, bounded frame inputs, optional
ticket-bound room reconnect, and leave messages. Every mutable gameplay message
reserves `4` potential data writes before dispatch. A no-reconnect flow with one
frame input therefore reserves `12` gameplay writes; enabling the one reconnect
reserves `20`, in addition to the auth/login/select/ticket/logout estimate.
Responses are correlated by the shared `(message type, sequence)` contract and
successful room responses must echo the approved room ID. Step reads enforce the
declared 2-second deadline independently of the overall run deadline.

Credential-free template (disabled by omission):

```json
{
  "scenario": {
    "name": "approved-room-smoke",
    "load": { "type": "staged", "stages": [{ "name": "one-vp", "virtual_players": 1, "duration_secs": 31 }] },
    "writes_data": true,
    "live_gameplay": {
      "room_id": "<approved-room-id>",
      "policy_id": "<approved-policy-id>",
      "profile": "normal",
      "lockstep_scenario_json": "<bounded scenario JSON>",
      "max_frame_inputs": 1
    }
  }
}
```

This template contains no credentials and is not executable until the normal
`--execute-game`, `--confirm-game`, manifest, private-config, target, and hard
budget gates all pass. A real smoke requires a separately confirmed approved
room/policy, one virtual player, a staged 31-second window, `max_frame_inputs: 1`,
and the smallest possible auth/game rate and write budgets. Offline tests and
dry-runs never create a KCP transport and never send these requests.
Because auth dispatch uses `Connection: close`, the planner also includes its
login and ticket HTTP attempts in the new-connection budget, in addition to
KCP proxy connects.

### Two-account `default_match` smoke

The legacy live gameplay path remains single-player unless the gameplay block
explicitly sets `"coordination": "two_player_default_match"`. This mode is a
controlled smoke for the current `default_match` policy, not a general
multiplayer load generator. It requires all of the following before a live
transport is created:

- `policy_id: "default_match"`, an explicitly pre-provisioned unique room ID,
  `writes_data: true`, and a non-idle generated-input profile;
- `max_frame_inputs: 1` (one bounded input from each player), no gameplay
  reconnect, and an auth scenario with same-account concurrency disabled;
- exactly one staged wave with two virtual players, plus
  `account_prepare.account_count >= 2` and two verified manifest accounts.

The coordinator holds both distinct leases, authenticates and heartbeats both
KCP sessions, then sends this deterministic room sequence:

`RoomJoin(player 1) -> RoomJoin(player 2) -> RoomReady(true, player 1) ->
RoomReady(true, player 2) -> RoomStart(player 1) -> PlayerInput(player 1) ->
PlayerInput(player 2) -> FrameBundle(each player) -> RoomLeave(player 1) ->
RoomLeave(player 2)`.

`RoomReadyRes` and `RoomStartRes` use the same exact message-type/sequence
correlation as the existing gameplay responses and must echo the approved room
ID. Any rejection, mismatch, timeout, abort, or KCP error closes both sessions
and releases both leases before deferred logout is considered for either auth
session. A successful run dispatches each eligible final logout only after the
two KCP sessions have closed.

With the normal five auth operations per account
(`login/list/select/ticket/logout`), this one-pair smoke emits 10 HTTP
operations on a no-retry happy path, 2 KCP connections, 4 auth/heartbeat player
messages, and 9 mutable gameplay messages: 25 operations and 52 potential
writes (16 auth plus 36 room messages). The existing hard estimator also
reserves the two permitted `list_characters` retries for each account, so an
executable profile must set `max_total_operations >= 29`; `max_data_writes: 56`
provides deliberate write headroom. At a staged 31-second window,
`Connection: close` means the conservative plan can admit 16 possible HTTP/KCP
connections, including those retry reservations. Use new-connection QPS above
`0.5` (for example `0.6`), login QPS `0.2`, and game
business/per-connection message rates of at least `2.0`.

Credential-free shape (the room is pre-provisioned separately; this tool never
creates or discovers it):

```json
{
  "account_prepare": { "account_count": 2 },
  "scenario": {
    "load": { "type": "staged", "stages": [{ "name": "pair", "virtual_players": 2, "duration_secs": 31 }] },
    "writes_data": true,
    "auth": {
      "allow_same_account_concurrency": false,
      "operations": ["login", "list_characters", "select_character", "issue_ticket", "logout"]
    },
    "live_gameplay": {
      "room_id": "<unique-approved-default-match-room>",
      "policy_id": "default_match",
      "coordination": "two_player_default_match",
      "profile": "normal",
      "lockstep_scenario_json": "<bounded scenario JSON>",
      "max_frame_inputs": 1
    }
  }
}
```

This remains subject to the normal `--execute-auth`, `--execute-game`, exact
environment confirmations, manifest, private-config, target-protection, and
hard-budget gates. Offline tests do not provision a room or send HTTP/KCP.

Compatibility tests consume `packages/game-protocol`, generated
`packages/proto/game.proto` messages, and the shared protocol-version policy.
They assert the 14-byte header, message numbers, stream/no-delay KCP profile,
exact response sequence matching, independent push handling, and that
`RoomReconnectReq` carries only the current ticket-bound character's push
cursor. Current production code must additionally verify signed ticket owner
against Redis and resolve the reconnect subject server-side; an explicitly
approved end-to-end smoke remains required to verify those service behaviors.

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
  revalidation. They also require a bounded test window, named observer, stop
  owner, manual-confirmation reference, STOP-file path, explicit account count
  and a non-default dedicated account batch. CLI budget flags only reduce the
  profile's hard budgets.
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

### Remote Test/Preprod Profile

Set `environment.kind` to `test` only for an isolated remote test/preprod
environment whose public auth health response identifies `env: "test"`. A
different profile name does not relax this identity check. The profile keeps
the same remote gate as every non-local run: exact CLI confirmation, approved
time window, observers and stop owner, STOP file, host/IP allowlists, private
credential references, verified manifest accounts, and hard operation/write
budgets. Before every admitted auth request, and again at controller and
side-service admission points, the runner rechecks the approved window and
the DNS/TLS/auth-descriptor baseline. Auth-supplied side-service descriptors
are pinned for the run and must remain within their configured allowlists.

An approved remote `test` profile may use the existing local/test diagnostics:
bounded Chat WSS, mail/announcement HTTP operations, direct match diagnostics,
guarded reconnect execution, and the read-only registry observer. Mail writes
still require `writes_data: true`, a positive data-write budget, and a
`write_batch` exactly matching the dedicated account batch. The registry
adapter exposes read commands only and its runtime connection details are never
serialized into reports. These diagnostics remain bounded service smoke tests,
not a capacity conclusion or a distributed-worker acceptance run.

`production` remains stricter: it rejects Chat, mail/announcement live
diagnostics, direct match diagnostics, reconnect bursts, auth-only live runs,
registry observation, and all control-plane writes. Its only supported remote
player execution is the existing guarded two-account `default_match` chain.
This tool has no real GM or operations-write transport; `gm_or_ops_write` and
other prohibited operations remain fail-closed in every profile.

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

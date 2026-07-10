# lockstep-client

`lockstep-client` is a verification tool for shared lockstep simulation
scenarios. The CLI supports offline replay and an online MyServer mode. Offline
mode loads a scenario, steps both server-side and client-side simulation with
the same inputs, and checks the final frame/hash assertions. Online mode connects
to a local game endpoint, joins a `lockstep_sim_demo` room, sends `sim_input`,
and replays server frames locally through the same `sim-core`.

## Offline replay

Offline mode does not start MyServer services. It loads a scenario JSON, builds
two local `SimWorld` instances, applies the same frame inputs to both, then
checks the final frame, final hash, event assertions, and tracked positions.

Run a scenario by name from `tools/lockstep-client/scenarios`:

```powershell
cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario move_straight
```

Run a scenario by explicit JSON path:

```powershell
cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario tools/lockstep-client/scenarios/move_straight.json
```

Successful output includes the resolved scenario path, final frame, and final
hash:

```text
scenario: tools/lockstep-client/scenarios/move_straight.json
final frame: 5
final hash: f70bc6733be8be87
```

Useful movement and combat scenarios:

```powershell
cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario move_straight
cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario move_stop
cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario move_diagonal
cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario melee_hit
cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario lockstep_demo_melee
```

Scenario files use raw milli-units. For example, `x: 1500` means 1.5 simulation
units, and `speedPerSecondMilli: 6000` means 6 simulation units per second.

## Run all passing scenarios

`move_invalid_input` is a negative fixture and is expected to fail. Exclude it
from normal batch verification:

```powershell
$scenarios = Get-ChildItem -LiteralPath tools/lockstep-client/scenarios -Filter *.json |
  Where-Object { $_.BaseName -ne "move_invalid_input" } |
  Sort-Object Name

foreach ($scenario in $scenarios) {
  cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode offline --scenario $scenario.BaseName
  if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
  }
}
```

## Online replay

Dry-run online mode parses a scenario and builds `PlayerInputReq(action =
"sim_input", payload_json = ...)` packets without opening a socket. Run both
movement and `lockstep_sim_demo` melee dry-runs before a real service
integration run:

```powershell
cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online --scenario move_straight --dry-run

cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online --scenario lockstep_demo_melee --dry-run
```

`lockstep_demo_melee` is aligned with the server demo defaults: player entity
`1000`, skill id `1`, and training target entity `9000`.

The generated `sim_input` payload shape is:

```json
{
  "version": 1,
  "seq": 1,
  "commands": [
    { "type": "move", "dirX": 1000, "dirY": 0, "speed": 6000 },
    { "type": "castSkill", "skillId": 1, "targetEntityId": 9000 }
  ]
}
```

Supported online commands are `move`, `stop`, `face`, and `castSkill`.
`castSkill` online payloads currently support `targetEntityId` or no target;
position and direction skill targets are supported by `sim-core` / offline
scenarios but are rejected by the current online wire adapter.
The online JSON is an intent payload only. Do not add authoritative result
fields such as `entityId`, `hit`, `damage`, `buffs`, `finalState`, or
`stateHash`; the server adapter uses a strict schema and rejects unknown fields.

`lockstep_sim_demo` is an independent MyServer room policy for shared
`sim-core` verification. It does not replace `robot_sync_room`,
`movement_demo`, or `combat_demo`: the robot room remains a lightweight input
forwarding sample, movement remains the older server-authoritative correction
sample, and combat remains the older ECS/snapshot comparison sample.

Real online mode requires the MyServer dependencies and game endpoint to be
started by the operator first. It does not start Redis, PostgreSQL, NATS,
`auth-http`, `game-server`, or `game-proxy` itself.

Keep bearer tickets out of shell arguments by using an environment variable:

Real online movement replay:

```powershell
$env:MYSERVER_LOCKSTEP_TICKET = "<character-bound-ticket>"
cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online `
  --scenario move_straight `
  --server 127.0.0.1:7000 `
  --ticket-env MYSERVER_LOCKSTEP_TICKET `
  --room lockstep-online-demo `
  --policy lockstep_sim_demo
```

Real online melee replay:

```powershell
cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online `
  --scenario lockstep_demo_melee `
  --server 127.0.0.1:7000 `
  --ticket-env MYSERVER_LOCKSTEP_TICKET `
  --room lockstep-online-demo `
  --policy lockstep_sim_demo
```

## Automated local online reconciliation

`scripts/online-lockstep-reconcile.ps1` is the reusable local orchestration
entry point. A call without `-Execute` or `-DryRun` only prints a JSON plan. It
does not create an artifact directory, read ticket values, start a process, or
open a network connection:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/online-lockstep-reconcile.ps1 `
  -StartDevStack -ProvisionDevTickets
```

Run all three client packet plans without services or tickets:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/online-lockstep-reconcile.ps1 -DryRun
```

`-DryRun` does not open network connections, but it does invoke Cargo and write
the run report/stdout/stderr artifacts. Its report therefore marks local
`sideEffects=true`, `externalSideEffects=false`, and
`networkConnectionsAllowed=false`.

Use `-Check move`, `-Check melee`, or `-Check observer` to run one check. The
default `-Check all` runs movement, melee, then observer recovery with a unique
run id and a separate room id for each check.

Real execution is intentionally gated by `-Execute`. Review the plan and obtain
operator confirmation before using either example below. These commands are
documented for a future confirmed run; this README does not assert that a real
online run has passed.

External `auth-http` ticket path:

```powershell
$env:MYSERVER_LOCKSTEP_TICKET = "<primary-character-ticket>"
$env:MYSERVER_LOCKSTEP_OBSERVER_TICKET = "<different-observer-character-ticket>"

powershell -NoProfile -ExecutionPolicy Bypass -File scripts/online-lockstep-reconcile.ps1 `
  -Execute -StartDevStack -TicketSource auth-http-external

Remove-Item Env:MYSERVER_LOCKSTEP_TICKET, Env:MYSERVER_LOCKSTEP_OBSERVER_TICKET
```

The external tickets must already have valid owner and version bindings in the
same Redis used by `game-server`. Before opening the game socket, the script
decodes only non-secret ticket metadata and checks these exact keys:

- `<REDIS_KEY_PREFIX>ticket:<sha256(ticket)>` must equal the account player id.
- `<REDIS_KEY_PREFIX>player-ticket-version:<account-player-id>` must equal the
  ticket `ver` field.

Pass `-RedisKeyPrefix` when `game-server` uses a non-empty
`REDIS_KEY_PREFIX`; the wrapper and server values must match exactly.

Use `-SkipTicketRedisPreflight` only when the operator has separately verified
those bindings and the script cannot access Redis. The game server still
performs the authoritative signature, owner, expiry, and version checks.

Ephemeral local dev ticket path:

```powershell
$env:MYSERVER_LOCKSTEP_TICKET_SECRET = "<same-local-secret-used-by-game-server>"

powershell -NoProfile -ExecutionPolicy Bypass -File scripts/online-lockstep-reconcile.ps1 `
  -Execute -StartDevStack -ProvisionDevTickets

Remove-Item Env:MYSERVER_LOCKSTEP_TICKET_SECRET
```

Provisioning is restricted to loopback Redis. It creates two character-bound
HMAC tickets and four unique keys with `SET NX` and a maximum one-hour TTL. It
does not create accounts or PostgreSQL rows. Cleanup uses compare-and-delete on
those four exact keys and refuses wildcard keys, unrelated key types, or values
that no longer match this run. Neither ticket nor the signing secret is written
to the command line, logs, or JSON report. The client reads them through
`--ticket-env` and `--observer-ticket-env`.

### Local dependency matrix

| Component | Local default | Required by the wrapper |
|-----------|---------------|-------------------------|
| Redis | `redis://127.0.0.1:6379` | Yes: ticket bindings and registry |
| Core NATS | `nats://127.0.0.1:4222` | Yes: local game runtime channels |
| `game-server` player TCP | `127.0.0.1:7000` | Yes: direct local debug endpoint |
| `game-server` admin | `127.0.0.1:7500` | Yes when the wrapper starts the stack, for readiness |
| `auth-http` | `127.0.0.1:3000` | Only to issue external tickets before the run |
| PostgreSQL | `127.0.0.1:5432` | Never touched by the wrapper-owned stack (`DB_ENABLED=false`); an external operator-owned endpoint may use it |
| `game-proxy` TCP fallback | usually `127.0.0.1:14000` | Not started by this direct local reconciliation |

The wrapper also requires PowerShell, Cargo, and Node.js. Redis ticket
provisioning or binding validation loads the repository's existing `ioredis`
workspace dependency, so run the normal root `npm install` before a real
execution.

With `-StartDevStack`, the wrapper selects only Redis, Core NATS, and
`game-server` through `scripts/dev-stack.ps1`; it explicitly disables auth,
proxy, admin UI/API, and metrics collector. Redis and Core NATS may already be
listening; they are then reused and never stopped. The game player/admin ports
must both be free: the wrapper refuses to reuse a game-server whose runtime
configuration it does not own. Before startup it explicitly sets `REDIS_URL`,
`REDIS_KEY_PREFIX`, `REGISTRY_URL`, `REGISTRY_KEY_PREFIX`, `NATS_URL`,
`SERVICE_NAME=game-server`, and `DB_ENABLED=false` in the child environment.
This direct reconciliation path therefore does not connect to or write
PostgreSQL, even if `apps/game-server/.env` enables the database. Without
`-StartDevStack`, the endpoint and its configuration remain entirely
operator-owned and the wrapper does not override them.

The wrapper records the PID, name, launcher timestamp, and process start time of
each process actually started by this run. Registry ownership begins as
`planned` and changes to `owned` only after the new PID file proves that this
invocation started exactly one `game-server`; successful guarded deletion changes
it to `cleaned`. The wrapper stops only those exact process trees, then checks
only their owned ports. It never uses `-Restart`,
`-StopExistingProjectProcesses`, or a broad project-process stop. An existing
`logs/dev-stack/dev-stack.pids.json` blocks startup instead of being replaced.
Returning success without a new PID ownership file is also a hard failure.
Failures before ownership confirmation report
`not-attempted-no-owned-game-server` and issue no registry delete command.

The local fixed ports above are bind/probe defaults, not a discovery strategy.
Test, staging, and production consumers must continue to use Redis service
registry endpoints. This wrapper deliberately accepts only a loopback TCP
endpoint and is not a production deployment tool.

### Reports and cleanup

Every `-DryRun` or `-Execute` invocation writes
`logs/lockstep-online/<run-id>/report.json`, plus separate stdout/stderr files
for movement, melee, observer recovery, and dev-stack startup when selected.
The report schema is `myserver.lockstep-online-reconcile.report.v1`. Redis URLs
in plans and reports omit userinfo, query, and fragment fields. The report records
room ids, masked ticket fingerprints, ticket source, endpoint, stage, frame,
server/client hashes, entity/event/input differences, owned Redis keys, owned
PIDs, cleanup results, port checks, and all artifact paths. Ticket cleanup
ownership includes each exact key and its non-secret expected player-id/version
value. Registry cleanup ownership includes its `planned`/`owned`/`cleaned`
status, the confirmed game-server PID identity, exact instance/heartbeat keys,
expected `data.id`, expected `data.name`, and heartbeat value. Ticket values,
signing secrets, and Redis credentials are never included.

`-Execute` always attempts cleanup in `finally`, including failed runs. It first
stops the run-owned game-server, then compare-deletes ticket keys and atomically
validates and deletes the exact registry instance/heartbeat keys, and only then
stops run-owned NATS/Redis. A mismatching registry hash, heartbeat value, or
ticket value is not deleted and makes cleanup fail. Reused services and
external ticket keys are never deleted or stopped. The wrapper never runs
`FLUSH*`, wildcard deletion, database reset, or account/character deletion.
The normal stop command is therefore the same one-shot `-Execute` command; no
separate broad stop command is needed. After startup has returned and the report
shows registry status `owned`, `report.json` is the normal recovery record. It
is not sufficient by itself during the narrow startup window between
`dev-stack.ps1` writing its PID file and the wrapper persisting confirmed
ownership.

For a hard interruption in that window, also inspect
`logs/dev-stack/dev-stack.pids.json`, the run's `dev-stack.stdout.log` and
`dev-stack.stderr.log`, and the game-server log. Correlate PID-file `startedAt`
and the live process start time with `ownership.registry.startInvocationAt`, and
verify the configured instance is exactly
`ownership.registry.gameInstanceIdArgument` (`lockstep-<runId>`). Treat
`planned` as unconfirmed: do not delete registry keys from it. Do not use broad
`dev-stack -Stop`, a
port-wide kill, or an unrelated PID solely because the run id matches.

Read-only stack status is available with:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev-stack.ps1 -Status
```

The automatic stop action is process-specific `Stop-Process -Id <owned-pid>`
after matching the report's name, PID, and process start time, followed by owned-port
checks. Do not substitute `dev-stack -Restart`,
`-StopExistingProjectProcesses`, or a port-wide kill. A manual interrupted-run
stop must repeat the same PID/start-time check from `report.json` before using
`Stop-Process`.

After stopping the report-owned game-server, an interrupted run can replay the
same guarded Redis cleanup without ticket plaintext. Set the original Redis URL
in the runtime environment, then feed the ownership fields from the report to
the helper:

```powershell
$report = Get-Content logs/lockstep-online/<run-id>/report.json -Raw | ConvertFrom-Json
$env:MYSERVER_LOCKSTEP_REDIS_URL_RUNTIME = "<original-redis-url>"

@{
  action = "cleanup"
  keyPrefix = $report.runtimeConfig.redisKeyPrefix
  entries = @($report.ticket.ownedRedisKeys)
} | ConvertTo-Json -Depth 10 -Compress |
  node tools/lockstep-client/online-ticket-store.mjs

if ($report.ownership.registry.status -eq "owned") {
  @{
    action = "cleanup-registry"
    runId = $report.runId
    keyPrefix = $report.runtimeConfig.registryKeyPrefix
    serviceName = $report.ownership.registry.serviceName
    instanceId = $report.ownership.registry.instanceId
  } | ConvertTo-Json -Depth 10 -Compress |
    node tools/lockstep-client/online-ticket-store.mjs
}
```

Skip the ticket helper call when `ownedRedisKeys` is empty. Manual registry
cleanup is allowed only for status `owned`; skip `cleaned`, and investigate
`planned` using the startup-window evidence above instead of issuing a Redis
delete. Stop remaining report-owned infrastructure only after both guarded
cleanups finish, again requiring the recorded PID/start-time identity to match.
Remove the runtime Redis environment variable when recovery is complete.

After each successful scenario, the client strictly validates `RoomEndReq` and
`RoomLeaveReq`. If replay or observer validation fails, it still attempts both
primary cleanup calls while preserving the original error; cleanup failures are
appended to that error. Once an observer has joined, its connection always
attempts `RoomLeaveReq`, including snapshot validation failures. This releases
character room bindings and leaves temporary rooms eligible for normal empty-room
cleanup.

Online mode consumes `RoomSnapshot.game_state` JSON. It restores
`initialSnapshot.snapshot` through `sim-core`, consumes each
`SimFrameEnvelope` from `observerFrame.lastFrame` or `lastFrame`, reconstructs
frame inputs from `FrameBundlePush.inputs`, and compares server `stateHash` and
`events` against local replay.

Downlink semantics:

- `initialSnapshot` schema is `myserver.lockstep-sim.initial-snapshot.v1` with
  `schemaVersion = 1`; it carries `snapshot`, `stateHash`, `configHash`,
  `rngSeed`, `entities`, and `controlBindings`.
- `lastFrame` / `observerFrame.lastFrame` schema is
  `myserver.lockstep-sim.frame-envelope.v1` with `schemaVersion = 1`; it
  carries `frame`, `stateHash`, `eventCount`, `events`, `eventSummaries`,
  `inputSources`, `debugSummary`, and `debugState`.
- `stateHash.hex` is the 16-character server hash for the world after that
  frame. Events are emitted for that frame only and are compared separately
  from the world hash.
- `eventSummaries` is a stable summary stream with fields such as `kind`,
  `sourceEntityId`, `targetEntityId`, `skillId`, `buffId`, `amount`, and
  `sequence`.
- `debugSummary` and `debugState` are diagnostic only. They help explain real
  versus synthesized input counts, event count, entity count, and lightweight
  entity state; they are not replay input.
- `FrameBundlePush.snapshot.game_state` is the source for the server envelope,
  while `FrameBundlePush.inputs` is replayed locally. After restoring from
  `initialSnapshot.snapshot`, reconciliation treats the server `stateHash.hex`
  as authoritative.

Not covered by this tool or the current demo: production deployment, complex
physics, NavMesh, production AOI, cross-server migration, complete CSV
skill/Buff mapping, real external client integration, formal UI/animation, and
productized prediction/rollback. Real online reconciliation still requires the
operator to confirm the selected dependency set, ticket source, impact scope,
and exact commands before running non-dry-run online mode. PostgreSQL and
`auth-http` are needed only for the external account/ticket path; `game-proxy`
is needed only for a separate formal-entry-path validation.

On mismatch the tool prints the first mismatching frame, server hash, client
hash, tracked entity differences, event differences, and frame inputs.

## Tests

Run the simulation core tests:

```powershell
cargo test --manifest-path packages/sim-core/Cargo.toml
```

Run the lockstep client tests:

```powershell
cargo test --manifest-path tools/lockstep-client/Cargo.toml
```

Run the online orchestration checks without services:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/online-lockstep-reconcile.ps1 -SelfTest
node --test tools/lockstep-client/online-ticket-store.test.mjs
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/online-lockstep-reconcile.ps1 -DryRun
```

## Common failures

- Non-contiguous frame progression: `sim-core` expects each step frame to follow
  the world's current frame exactly. A repeated, skipped, or out-of-range frame
  is rejected instead of mutating the world.
- Hash mismatch during replay: server/client simulation diverged at a frame.
  Inspect the reported frame, inputs, entity counts, and entity diffs.
- Event mismatch during online replay: the world hash may match while emitted
  events differ. Inspect `server_events`, `client_events`, selected skill
  target, cooldown state, and `inputSources`.
- Final hash mismatch: replay completed, but `assertions.finalHash` does not
  match the computed final world hash.
- Invalid input: command fields are rejected by schema or validation, for
  example an empty move direction or a speed greater than
  `config.movement.maxSpeedPerSecondMilli`.
- Scenario version mismatch: `version` must match the schema version supported
  by `sim-core`.
- Unsupported online command: the scenario contains a command supported offline
  but not by `lockstep_sim_demo` online wire, such as `CastSkill` with position
  or direction target.
- Missing initial snapshot: the server did not publish
  `RoomSnapshot.game_state.initialSnapshot`; confirm the room uses
  `--policy lockstep_sim_demo` and that the room has started.
- Server rejection during `player_input`: inspect `PlayerInputRes.error_code`.
  Common codes include `INVALID_SIM_INPUT_ACTION`, `UNSUPPORTED_SIM_INPUT_VERSION`,
  `SIM_INPUT_DIR_OUT_OF_RANGE`, `SIM_INPUT_MOVE_DIR_ZERO`,
  `SIM_INPUT_SPEED_OUT_OF_RANGE`, and
  `SIM_INPUT_TARGET_ENTITY_ID_OUT_OF_RANGE`.

## Updating finalHash

`finalHash` is not auto-blessed. When simulation behavior intentionally changes,
run the affected scenario offline, review the output and diffs, then update the
scenario's `assertions.finalHash` manually as an explicit change. Leave
`0000000000000000` only for temporary pre-bless fixtures.

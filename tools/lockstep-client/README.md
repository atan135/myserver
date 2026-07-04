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

Real online mode requires the MyServer dependencies and game endpoint to be
started by the operator first. It does not start Redis, PostgreSQL, NATS,
`auth-http`, `game-server`, or `game-proxy` itself.

Real online movement replay:

```powershell
cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online `
  --scenario move_straight `
  --server 127.0.0.1:7000 `
  --ticket <ticket-or-local-test-ticket> `
  --room lockstep-online-demo `
  --policy lockstep_sim_demo
```

Real online melee replay:

```powershell
cargo run --manifest-path tools/lockstep-client/Cargo.toml -- --mode online `
  --scenario lockstep_demo_melee `
  --server 127.0.0.1:7000 `
  --ticket <ticket-or-local-test-ticket> `
  --room lockstep-online-demo `
  --policy lockstep_sim_demo
```

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
  carries `frame`, `stateHash`, `events`, `inputSources`, and `debugSummary`.
- `stateHash.hex` is the 16-character server hash for the world after that
  frame. Events are emitted for that frame only and are compared separately
  from the world hash.
- `debugSummary` is diagnostic only. It helps explain real versus synthesized
  input counts, event count, and entity count; it is not replay input.

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

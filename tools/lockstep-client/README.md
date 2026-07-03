# lockstep-client

`lockstep-client` is a local verification tool for shared lockstep simulation
scenarios. The current CLI supports offline replay only: it loads a scenario,
steps both server-side and client-side simulation with the same inputs, and
checks the final frame/hash assertions.

## Offline replay

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
- Final hash mismatch: replay completed, but `assertions.finalHash` does not
  match the computed final world hash.
- Invalid input: command fields are rejected by schema or validation, for
  example an empty move direction or a speed greater than
  `config.movement.maxSpeedPerSecondMilli`.
- Scenario version mismatch: `version` must match the schema version supported
  by `sim-core`.

## Updating finalHash

`finalHash` is not auto-blessed. When simulation behavior intentionally changes,
run the affected scenario offline, review the output and diffs, then update the
scenario's `assertions.finalHash` manually as an explicit change. Leave
`0000000000000000` only for temporary pre-bless fixtures.

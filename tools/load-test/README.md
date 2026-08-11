# MyServer Load Test

`tools/load-test` is an independent Rust package for the player core path.
Stage one implements an offline-safe controller/worker contract, validation,
load scheduling, guardrails, metrics, report generation and deterministic
test doubles. It deliberately does not connect to any application service yet.

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

`validate` performs schema, target and safety checks only. `run` and
`calibrate` require `--dry-run` in stage one and write only an isolated report.
`account-prepare` has a separate result root; `apply`, `verify`, and `export`
are intentionally unavailable until the account-preparation stage.

Before `run` or `calibrate` creates a plan, schedules any player, opens a
transport, or writes a report, it prints a single `preflight=<JSON>` line. The
summary includes the environment, hashed target summaries, stage-one account
batch status, selected and supported load models, effective budget/duration and
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
and `summary.md`. `metrics.json` stores compact mergeable HDR V2+DEFLATE
histogram payloads, so controller/worker merge operates on distributions,
never locally calculated percentiles. Target values are hashed, error samples
are bounded, and unavailable Windows measurements are reported as
`unavailable`, never zero.

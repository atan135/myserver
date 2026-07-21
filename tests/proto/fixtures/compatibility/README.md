# Protocol Binary Compatibility Fixtures

Each `.bin` file is a Protobuf body only. The shared TCP header is deliberately excluded so the fixture stays focused on the message contract in `packages/proto/game.proto`.

`manifest.json` is the readable source: it identifies the message number, field values, expected decoded result, byte count and SHA-256. Every value is deterministic synthetic test data. Identity values must use the `fixture_` or `fake_` prefix; credentials, JWT-shaped strings, Bearer values, email-shaped account values and private keys are rejected by the fixture checker.

The fixtures are written from the reviewed definitions in `tools/generate-proto-compatibility-fixtures.js`:

```powershell
node .\tools\generate-proto-compatibility-fixtures.js --write
npm run check:proto-fixtures
node .\tools\run-node-tests.js tests/proto/proto-compatibility-fixtures.test.mjs
```

Regeneration is an intentional golden-data update, not a normal build step. Review the binary size, manifest digest and readable source together. The `GameMessagePush` fixture has a 64 KiB payload and a 65,630-byte protobuf body, large enough to exercise multi-byte protobuf lengths while remaining below the mock-client's 1 MiB packet guard.

`movement-snapshot-v1.bin` has a deliberately separate historical v1 projection in `legacy-movement-snapshot-v1.mjs`. `movement-snapshot-future-fields.bin` adds unknown enum numbers and field 190. The historical decoder retains fields 1 through 5 and skips the additions, which is the Protobuf unknown-field forward-compatibility property. It does not promise that application-level validation will accept every future enum value.

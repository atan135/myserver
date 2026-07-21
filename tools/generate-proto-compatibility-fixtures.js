import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

import { MESSAGE_TYPE } from "./mock-client/src/constants.js";
import {
  encodeBoolField,
  encodeFloatField,
  encodeInt32Field,
  encodeInt64Field,
  encodeMessageField,
  encodeStringField,
  encodeUInt32Field
} from "./mock-client/src/protocol.js";

const REPOSITORY_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const FIXTURE_DIRECTORY = path.join(REPOSITORY_ROOT, "tests", "proto", "fixtures", "compatibility");
const MANIFEST_PATH = path.join(FIXTURE_DIRECTORY, "manifest.json");
const MAX_SAFE_INT64 = Number.MAX_SAFE_INTEGER;
const MAX_UINT32 = 0xffff_ffff;
const LARGE_PAYLOAD_BYTES = 64 * 1024;
const MAX_FIXTURE_BODY_BYTES = 66 * 1024;

function sha256(body) {
  return `sha256:${createHash("sha256").update(body).digest("hex")}`;
}

function elementValues(fields) {
  return Buffer.concat([
    encodeInt32Field(1, fields.earth),
    encodeInt32Field(2, fields.fire)
  ]);
}

function attrPanel() {
  return Buffer.concat([
    encodeInt64Field(1, MAX_SAFE_INT64),
    encodeInt64Field(2, MAX_SAFE_INT64 - 1),
    encodeInt64Field(3, 42),
    encodeInt64Field(4, 0),
    encodeUInt32Field(5, MAX_UINT32)
  ]);
}

function entityTransform() {
  return Buffer.concat([
    encodeInt64Field(1, MAX_SAFE_INT64),
    encodeStringField(2, "fixture_character_legacy"),
    encodeUInt32Field(3, 7),
    encodeFloatField(4, 1.25),
    encodeFloatField(5, -2.5),
    encodeFloatField(6, 0),
    encodeFloatField(7, 0),
    encodeBoolField(8, true),
    encodeUInt32Field(9, MAX_UINT32)
  ]);
}

function legacyMovementSnapshot() {
  return Buffer.concat([
    encodeStringField(1, "fixture_room_legacy"),
    encodeUInt32Field(2, MAX_UINT32),
    encodeMessageField(3, entityTransform()),
    encodeBoolField(4, false),
    encodeStringField(5, "")
  ]);
}

function largePayloadJson() {
  const prefix = '{"fixture":"';
  const suffix = '"}';
  return `${prefix}${"x".repeat(LARGE_PAYLOAD_BYTES - Buffer.byteLength(prefix) - Buffer.byteLength(suffix))}${suffix}`;
}

const legacyMovementBody = legacyMovementSnapshot();
const largePayload = largePayloadJson();

export const PROTO_COMPATIBILITY_FIXTURES = [
  {
    file: "get-room-data-res-empty.bin",
    messageType: MESSAGE_TYPE.GET_ROOM_DATA_RES,
    protoMessage: "GetRoomDataRes",
    body: Buffer.concat([
      encodeBoolField(1, true),
      encodeStringField(3, "")
    ]),
    source: {
      description: "Explicit empty error_code and omitted empty repeated field_0_list.",
      fields: {
        ok: true,
        field_0_list: [],
        error_code: ""
      }
    },
    expectations: {
      kind: "exact",
      decoded: {
        ok: true,
        field0List: [],
        errorCode: ""
      }
    }
  },
  {
    file: "get-character-elements-int32-boundaries.bin",
    messageType: MESSAGE_TYPE.GET_CHARACTER_ELEMENTS_RES,
    protoMessage: "GetCharacterElementsRes",
    body: Buffer.concat([
      encodeBoolField(1, true),
      encodeStringField(3, "fixture_character_boundary"),
      encodeMessageField(4, encodeMessageField(1, elementValues({ earth: -2147483648, fire: 2147483647 })))
    ]),
    source: {
      description: "Nested ElementValues carries the signed int32 lower and upper bounds.",
      fields: {
        ok: true,
        character_id: "fixture_character_boundary",
        elements: {
          affinity: {
            earth: -2147483648,
            fire: 2147483647,
            water: 0,
            wind: 0
          }
        }
      }
    },
    expectations: {
      kind: "exact",
      decoded: {
        ok: true,
        errorCode: "",
        characterId: "fixture_character_boundary",
        elements: {
          affinity: {
            earth: -2147483648,
            fire: 2147483647,
            water: 0,
            wind: 0
          },
          mastery: null
        }
      }
    }
  },
  {
    file: "attr-change-int64-u32-boundaries.bin",
    messageType: MESSAGE_TYPE.ATTR_CHANGE_PUSH,
    protoMessage: "AttrChangePush",
    body: encodeMessageField(1, attrPanel()),
    source: {
      description: "AttrPanel uses the largest integer exactly representable by the Node mock-client and uint32 max.",
      fields: {
        base: {
          hp: MAX_SAFE_INT64,
          max_hp: MAX_SAFE_INT64 - 1,
          attack: 42,
          defense: 0,
          speed: MAX_UINT32
        },
        bonus: [],
        final: null
      }
    },
    expectations: {
      kind: "exact",
      decoded: {
        base: {
          hp: MAX_SAFE_INT64,
          maxHp: MAX_SAFE_INT64 - 1,
          attack: 42,
          defense: 0,
          speed: MAX_UINT32,
          critRate: 0,
          critDmg: 0
        },
        bonus: [],
        final: null
      }
    }
  },
  {
    file: "movement-snapshot-v1.bin",
    messageType: MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH,
    protoMessage: "MovementSnapshotPush",
    body: legacyMovementBody,
    source: {
      description: "Historical v1 projection with fields 1 through 5 only.",
      fields: {
        room_id: "fixture_room_legacy",
        frame_id: MAX_UINT32,
        entities: [{
          entity_id: MAX_SAFE_INT64,
          character_id: "fixture_character_legacy",
          scene_id: 7,
          x: 1.25,
          y: -2.5,
          dir_x: 0,
          dir_y: 0,
          moving: true,
          last_input_frame: MAX_UINT32
        }],
        full_sync: false,
        reason: ""
      }
    },
    expectations: {
      kind: "exact",
      decoded: {
        roomId: "fixture_room_legacy",
        frameId: MAX_UINT32,
        entities: [{
          entityId: MAX_SAFE_INT64,
          characterId: "fixture_character_legacy",
          sceneId: 7,
          x: 1.25,
          y: -2.5,
          dirX: 0,
          dirY: 0,
          moving: true,
          lastInputFrame: MAX_UINT32
        }],
        fullSync: false,
        reason: "",
        correctionKind: 0,
        reasonCode: 0,
        targetCharacterIds: [],
        referenceFrameId: 0
      }
    }
  },
  {
    file: "movement-snapshot-future-fields.bin",
    messageType: MESSAGE_TYPE.MOVEMENT_SNAPSHOT_PUSH,
    protoMessage: "MovementSnapshotPush",
    body: Buffer.concat([
      legacyMovementBody,
      encodeUInt32Field(6, 99),
      encodeUInt32Field(7, 77),
      encodeStringField(190, "fixture_future_field")
    ]),
    source: {
      description: "v1 body plus a future unknown enum number in fields 6/7 and an unknown length-delimited field 190.",
      extends: "movement-snapshot-v1.bin",
      appended_fields: {
        correction_kind: 99,
        reason_code: 77,
        field_190: "fixture_future_field"
      }
    },
    expectations: {
      kind: "exact",
      decoded: {
        roomId: "fixture_room_legacy",
        frameId: MAX_UINT32,
        entities: [{
          entityId: MAX_SAFE_INT64,
          characterId: "fixture_character_legacy",
          sceneId: 7,
          x: 1.25,
          y: -2.5,
          dirX: 0,
          dirY: 0,
          moving: true,
          lastInputFrame: MAX_UINT32
        }],
        fullSync: false,
        reason: "",
        correctionKind: 99,
        reasonCode: 77,
        targetCharacterIds: [],
        referenceFrameId: 0
      }
    }
  },
  {
    file: "game-message-large-payload.bin",
    messageType: MESSAGE_TYPE.GAME_MESSAGE_PUSH,
    protoMessage: "GameMessagePush",
    body: Buffer.concat([
      encodeStringField(1, "fixture_large_payload"),
      encodeStringField(2, "fixture_room_large"),
      encodeStringField(3, "fixture_character_large"),
      encodeStringField(4, "fixture_action_large"),
      encodeStringField(5, largePayload)
    ]),
    source: {
      description: "A deterministic JSON payload with a 64 KiB UTF-8 body of x characters.",
      fields: {
        event: "fixture_large_payload",
        room_id: "fixture_room_large",
        character_id: "fixture_character_large",
        action: "fixture_action_large",
        payload_json: {
          format: "{\"fixture\":\"<x repeated>\"}",
          utf8_bytes: LARGE_PAYLOAD_BYTES,
          fill: "x"
        }
      }
    },
    expectations: {
      kind: "large_payload",
      decoded: {
        event: "fixture_large_payload",
        roomId: "fixture_room_large",
        characterId: "fixture_character_large",
        action: "fixture_action_large",
        payloadUtf8Bytes: LARGE_PAYLOAD_BYTES,
        payloadPrefix: '{"fixture":"',
        payloadSuffix: '"}'
      }
    }
  }
];

export function buildFixtureManifest() {
  return {
    schema: "myserver.protobuf.binary-fixtures/v1",
    bodyFormat: "protobuf_body_without_tcp_header",
    syntheticData: {
      declared: true,
      identityPrefix: "fixture_",
      statement: "Every fixture value is deterministic synthetic test data; no production payloads or credentials are permitted."
    },
    limits: {
      maxFixtureBodyBytes: MAX_FIXTURE_BODY_BYTES,
      largePayloadReason: "The payload is 64 KiB; its 65,630-byte protobuf body stays under this 66 KiB cap and well below the 1 MiB mock-client packet guard."
    },
    integerCompatibility: {
      int32: "Uses both signed int32 endpoints.",
      int64: `Uses ${MAX_SAFE_INT64}, the largest integer the current Node mock-client can round-trip exactly.`,
      uint32: `Uses ${MAX_UINT32}, the uint32 endpoint.`
    },
    fixtures: PROTO_COMPATIBILITY_FIXTURES.map((fixture) => ({
      file: fixture.file,
      proto: {
        file: "packages/proto/game.proto",
        message: fixture.protoMessage
      },
      messageType: fixture.messageType,
      byteLength: fixture.body.length,
      sha256: sha256(fixture.body),
      source: fixture.source,
      expectations: fixture.expectations
    }))
  };
}

function writeFixtures() {
  mkdirSync(FIXTURE_DIRECTORY, { recursive: true });
  for (const fixture of PROTO_COMPATIBILITY_FIXTURES) {
    writeFileSync(path.join(FIXTURE_DIRECTORY, fixture.file), fixture.body);
  }
  writeFileSync(MANIFEST_PATH, `${JSON.stringify(buildFixtureManifest(), null, 2)}\n`);
  console.log(`Wrote ${PROTO_COMPATIBILITY_FIXTURES.length} protobuf compatibility fixtures to ${path.relative(REPOSITORY_ROOT, FIXTURE_DIRECTORY)}`);
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  if (process.argv.length !== 3 || process.argv[2] !== "--write") {
    console.error("Usage: node tools/generate-proto-compatibility-fixtures.js --write");
    process.exitCode = 1;
  } else {
    writeFixtures();
  }
}

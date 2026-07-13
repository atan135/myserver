import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  buildGrantMailAttachmentsPayload,
  computeGrantRequestFingerprint,
  normalizeGrantItems
} from "./game-admin-client.js";
import { buildMailNotificationEvent, validateMailNotificationEvent } from "./notification-outbox.js";

const fixtureUrl = new URL("../../../tests/fixtures/mail-cross-service-v1.json", import.meta.url);

async function readFixture() {
  return JSON.parse(await readFile(fixtureUrl, "utf8"));
}

test("Node notification builder matches the shared mail v1 fixture", async () => {
  const fixture = await readFixture();
  const contract = fixture.mail_notification_v1;
  const event = buildMailNotificationEvent(contract.mail_input, {
    eventId: contract.build_options.event_id,
    occurredAt: contract.build_options.occurred_at,
    traceId: contract.build_options.trace_id
  });

  validateMailNotificationEvent(event);
  assert.deepEqual(event, contract.expected_event);
  assert.equal(event.event_type, "mail.created");
  assert.equal(event.version, 1);
  assert.ok(Buffer.byteLength(event.event_id) <= contract.limits.event_id_max_bytes);
  assert.ok(Buffer.byteLength(event.player_id) <= contract.limits.player_id_max_bytes);
  assert.ok(Buffer.byteLength(event.mail.mail_id) <= contract.limits.mail_id_max_bytes);
  assert.ok(Buffer.byteLength(event.mail.title) <= contract.limits.title_max_bytes);
  assert.ok(Buffer.byteLength(event.mail.from_player_id) <= contract.limits.sender_id_max_bytes);
  assert.ok(Buffer.byteLength(event.mail.from_name) <= contract.limits.sender_name_max_bytes);
  assert.ok(Buffer.byteLength(event.mail.mail_type) <= contract.limits.mail_type_max_bytes);
  assert.equal(Buffer.byteLength(event.trace_id), contract.limits.trace_id_hex_bytes);
  assert.equal(Buffer.byteLength(JSON.stringify(event)), contract.expected_json_bytes);
  assert.ok(Buffer.byteLength(JSON.stringify(event)) <= contract.limits.max_payload_bytes);
});

test("Node grant encoder, normalization, canonical JSON, and SHA-256 match the shared fixture", async () => {
  const fixture = await readFixture();
  const contract = fixture.mail_attachment_grant_v1;
  const input = contract.input;
  const normalized = normalizeGrantItems(input.attachments);
  const fingerprint = computeGrantRequestFingerprint(input.mail_id, input.character_id, input.attachments);
  const canonical = JSON.stringify({
    mail_id: input.mail_id,
    character_id: input.character_id,
    source: input.source,
    items: normalized.map((item) => ({
      item_id: item.itemId,
      count: item.count,
      binded: item.binded
    }))
  });
  const encoded = buildGrantMailAttachmentsPayload(
    input.character_id,
    input.request_id,
    input.attachments,
    input.reason,
    {
      mailId: input.mail_id,
      requestFingerprint: fingerprint,
      traceId: input.trace_id,
      routeGeneration: input.route_generation,
      routeToken: input.route_token
    }
  );

  assert.equal(input.request_id, `mail_claim:${input.mail_id}`);
  assert.equal(input.source, "mail-claim");
  assert.ok(Buffer.byteLength(input.request_id) <= contract.limits.request_id_max_bytes);
  assert.ok(Buffer.byteLength(input.mail_id) <= contract.limits.mail_id_max_bytes);
  assert.ok(Buffer.byteLength(input.character_id) <= contract.limits.character_id_max_bytes);
  assert.ok(Buffer.byteLength(input.source) <= contract.limits.source_max_bytes);
  assert.ok(Buffer.byteLength(input.reason) <= contract.limits.reason_max_bytes);
  assert.equal(Buffer.byteLength(input.trace_id), contract.limits.trace_id_hex_bytes);
  assert.equal(Buffer.byteLength(input.route_token), contract.limits.route_token_hex_bytes);
  assert.ok(normalized.every((item) => item.count <= contract.limits.item_count_max));
  assert.deepEqual(normalized, contract.expected_normalized_items);
  assert.equal(canonical, contract.expected_canonical_json);
  assert.equal(fingerprint, contract.expected_fingerprint);
  assert.deepEqual(JSON.parse(encoded.toString("utf8")), contract.expected_request);
});

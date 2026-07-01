import assert from "node:assert/strict";
import { test } from "node:test";

import {
  buildChatOnlineRouteKey,
  buildInstanceMailSubject,
  buildLegacyMailSubject,
  PubSubClient
} from "../../apps/mail-service/src/pubsub-client.js";
import { configureLogger } from "../../apps/mail-service/src/logger.js";

configureLogger({
  appName: "mail-pubsub-routing-test",
  logLevel: "fatal",
  logEnableConsole: false,
  logEnableFile: false
});

function createMail(overrides = {}) {
  return {
    mail_id: "mail_001",
    sender_id: "system",
    title: "Reward",
    mail_type: "system",
    created_at: 1700000000,
    ...overrides
  };
}

test("PubSubClient publishes to chat instance subject when online route exists", async () => {
  const published = [];
  const redis = {
    async get(key) {
      assert.equal(key, "chat:online:player_001");
      return "chat-server-002";
    }
  };
  const nats = {
    async publishJson(subject, payload) {
      published.push({ subject, payload });
    }
  };

  const client = new PubSubClient(nats, redis);
  await client.publishMailNotification("player_001", createMail());

  assert.equal(published.length, 1);
  assert.equal(published[0].subject, buildInstanceMailSubject("chat-server-002"));
  assert.equal(published[0].payload.player_id, "player_001");
});

test("PubSubClient falls back to legacy player subject when route is missing", async () => {
  const published = [];
  const redis = {
    async get() {
      return null;
    }
  };
  const nats = {
    async publishJson(subject, payload) {
      published.push({ subject, payload });
    }
  };

  const client = new PubSubClient(nats, redis);
  await client.publishMailNotification("player_001", createMail());

  assert.equal(published.length, 1);
  assert.equal(published[0].subject, buildLegacyMailSubject("player_001"));
});

test("PubSubClient falls back to legacy player subject when route lookup fails", async () => {
  const published = [];
  const redis = {
    async get() {
      throw new Error("redis down");
    }
  };
  const nats = {
    async publishJson(subject, payload) {
      published.push({ subject, payload });
    }
  };

  const client = new PubSubClient(nats, redis);
  await client.publishMailNotification("player_001", createMail());

  assert.equal(published.length, 1);
  assert.equal(published[0].subject, buildLegacyMailSubject("player_001"));
});

test("mail route helpers use stable key and subject formats", () => {
  assert.equal(buildChatOnlineRouteKey("player_001"), "chat:online:player_001");
  assert.equal(buildChatOnlineRouteKey("player_001", "dev:"), "dev:chat:online:player_001");
  assert.equal(buildLegacyMailSubject("player.001"), "myserver.mail.notify.cGxheWVyLjAwMQ");
  assert.equal(
    buildInstanceMailSubject("chat.server.001"),
    "myserver.mail.notify.instance.Y2hhdC5zZXJ2ZXIuMDAx"
  );
});

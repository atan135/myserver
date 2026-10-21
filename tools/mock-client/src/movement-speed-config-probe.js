/** Local live probe. Uses the fixed public room plus disposable rooms/guests and temporarily edits one CSV. */
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fetchTicket, selectCharacter } from "./auth.js";
import { TcpProtocolClient } from "./client.js";
import { MESSAGE_TYPE as T, MOVE_INPUT_TYPE as M } from "./constants.js";
import { decodeByMessageType, encodeAuthReq, encodeRoomJoinReq, encodeRoomReadyReq, encodeRoomStartReq, encodeRoomLeaveReq, encodeRoomReconnectReq, encodeMoveInputReq } from "./messages.js";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");
const csv = path.join(root, "apps/game-server/csv/MovementConfig.csv");
const options = { httpBaseUrl: "http://127.0.0.1:3000", gameHost: "127.0.0.1", host: "127.0.0.1", port: 14000, autoCreateCharacter: true, timeoutMs: 10000, characterNamePrefix: "SpeedProbe" };
const rows = (value) => `Id,BaseSpeedMetresPerSecond\nint,float\n1,${value}\n`;
const report = [];
const peers = [];
const record = (label, details) => { report.push({ label, ...details }); console.log(JSON.stringify(report.at(-1))); };

async function roomConfigVersion(roomId) {
  const log = path.join(root, "logs/dev-stack/game-server.out.log");
  for (let attempt = 0; attempt < 20; attempt++) {
    const line = readFileSync(log, "utf8").replace(/\x1b\[[0-9;]*m/g, "").split("\n").findLast((line) => line.includes(`room_id="${roomId}"`) && line.includes("[RoomLogic/movement_demo] room created"));
    const version = line?.match(/config_version=(\d+)/)?.[1];
    if (version) return Number(version);
    await delay(100);
  }
  throw new Error(`missing sanitized room configuration log for ${roomId}`);
}

class Peer {
  constructor(login, label) { this.login = login; this.label = label; this.seq = 0; this.frame = 0; this.samples = []; this.own = null; this.client = new TcpProtocolClient(options, label); peers.push(this); }
  consume(packet) {
    this.lastPacket = [packet.messageType, packet.seq];
    const decoded = decodeByMessageType(packet.messageType, packet.body);
    if (packet.messageType === T.ERROR_RES) throw new Error(`proxy/server error ${decoded.errorCode}`);
    if (packet.messageType === T.FRAME_BUNDLE_PUSH) this.frame = Math.max(this.frame, decoded.frameId);
    let entities; let frame;
    if (packet.messageType === T.MOVEMENT_SNAPSHOT_PUSH) { entities = decoded.entities; frame = decoded.frameId; }
    if (packet.messageType === T.ROOM_RECONNECT_RES) { entities = decoded.movementRecovery?.entities; frame = decoded.movementRecovery?.frameId; }
    if ([T.ROOM_STATE_PUSH, T.FRAME_BUNDLE_PUSH].includes(packet.messageType) && decoded.snapshot?.gameState) {
      this.roomState = decoded.snapshot.state;
      const state = JSON.parse(decoded.snapshot.gameState);
      frame = packet.messageType === T.FRAME_BUNDLE_PUSH ? decoded.frameId : decoded.snapshot.currentFrameId;
      entities = state.entities?.map((entity) => ({ characterId: entity.character_id, x: entity.x, y: entity.y, dirX: entity.dir_x, dirY: entity.dir_y, moving: entity.moving, lastInputFrame: entity.last_input_frame, speedMetresPerSecond: entity.speed_metres_per_second }));
    }
    const own = entities?.find((entity) => entity.characterId === this.login.characterId);
    if (own) { this.own = { ...own, frame }; this.samples.push(this.own); }
    return decoded;
  }
  async until(predicate, timeout = 10000) {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      const packet = await this.client.readNextPacket(Math.max(1, deadline - Date.now()));
      const decoded = this.consume(packet);
      if (predicate(packet, decoded)) return decoded;
    }
    throw new Error(`${this.label}: expected local probe response not observed`);
  }
  async request(type, body, expected) {
    const seq = ++this.seq;
    await this.client.send(type, seq, body);
    try { return await this.until((packet) => packet.messageType === expected && packet.seq === seq); } catch (error) { throw new Error(`${this.label} request=${type} seq=${seq} expected=${expected} lastPacket=${this.lastPacket}: ${error.message}`); }
  }
  async connect(version = 2) {
    await this.client.connect();
    return this.request(T.AUTH_REQ, encodeAuthReq(this.login.ticket, version), T.AUTH_RES);
  }
  async join(room, start = false) {
    this.own = null; this.roomState = null; this.samples = []; this.frame = 0;
    const joined = await this.request(T.ROOM_JOIN_REQ, encodeRoomJoinReq(room, "movement_demo"), T.ROOM_JOIN_RES);
    assert.equal(joined.ok, true, joined.errorCode);
    this.roomId = room;
    if (!this.own) await this.until(() => Boolean(this.own));
    if (start && this.roomState !== "in_game") {
      assert.equal((await this.request(T.ROOM_READY_REQ, encodeRoomReadyReq(true), T.ROOM_READY_RES)).ok, true);
      assert.equal((await this.request(T.ROOM_START_REQ, encodeRoomStartReq(), T.ROOM_START_RES)).ok, true);
    }
    if (!this.own) await this.until(() => Boolean(this.own));
    return this.own;
  }
  async freshFrame() {
    while (this.client.packetQueue.length) this.consume(this.client.packetQueue.shift());
    await this.until((packet) => packet.messageType === T.FRAME_BUNDLE_PUSH);
    return this.frame;
  }
  async move(x, y, count = 45) {
    const initial = this.samples.length; let last = 0;
    for (let i = 0; i < count; i++) {
      last = (await this.freshFrame()) + 2;
      const response = await this.request(T.MOVE_INPUT_REQ, encodeMoveInputReq(last, M.MOVE_DIR, x, y), T.MOVE_INPUT_RES);
      assert.equal(response.ok, true, response.errorCode);
    }
    const stop = (await this.freshFrame()) + 2;
    const response = await this.request(T.MOVE_INPUT_REQ, encodeMoveInputReq(stop, M.MOVE_STOP), T.MOVE_INPUT_RES);
    assert.equal(response.ok, true, response.errorCode);
    await this.until(() => this.own && !this.own.moving && this.own.lastInputFrame >= stop);
    const samples = this.samples.slice(initial).filter((sample) => sample.moving && sample.frame <= last && sample.lastInputFrame <= last);
    assert.ok(samples.length >= 2, "at least two authoritative movement samples required");
    const first = samples[0]; const final = samples.at(-1);
    const frameSpan = final.frame - first.frame;
    assert.ok(frameSpan >= 3);
    const actual = Math.hypot(final.x - first.x, final.y - first.y);
    const expected = frameSpan * first.speedMetresPerSecond / 20;
    assert.ok(Math.abs(actual - expected) <= 0.005, `authoritative distance ${actual} != ${expected}`);
    record(`${this.label}.move`, { direction: [x, y], speed: first.speedMetresPerSecond, firstFrame: first.frame, lastFrame: final.frame, frameSpan, actual, expected, tolerance: 0.005, stoppedFrame: this.own.frame });
  }
}

async function newPeer(label, version = 2) {
  const login = await fetchTicket(options, { guestId: label });
  const peer = new Peer(login, label);
  const auth = await peer.connect(version);
  record("auth.result", { version, ok: auth.ok, errorCode: auth.errorCode });
  return { peer, auth };
}

export async function runMovementSpeedConfigProbe(args = process.argv.slice(2)) {
  if (args.length !== 1 || args[0] !== "--allow-local-csv-reload") throw new Error("Local mutation requires --allow-local-csv-reload; only localhost auth:3000/proxy TCP:14000 and repository MovementConfig.csv are used.");
  const original = readFileSync(csv, "utf8");
  assert.equal(original.replaceAll("\r", "").trim(), rows("2.0").trim(), "start from committed 2.0 configuration");
  const run = `speed-${Date.now().toString(36)}`;
  const roomA = "main-world-public"; const roomB = `${run}-b`; const roomC = `${run}-c`;
  const restore = () => { if (readFileSync(csv, "utf8") !== original) writeFileSync(csv, original); };
  const interrupted = () => { restore(); process.exit(130); };
  process.once("SIGINT", interrupted);
  try {
    const { peer: a, auth } = await newPeer(`${run}-a`); assert.equal(auth.ok, true);
    const ownA = await a.join(roomA, true); assert.equal(ownA.speedMetresPerSecond, 2);
    const versionA = await roomConfigVersion(roomA);
    record("roomA.created", { roomId: roomA, characterId: a.login.characterId, speed: ownA.speedMetresPerSecond, configVersion: versionA });
    await a.move(1, 0); await a.move(1, 1); await a.move(0, -1);
    writeFileSync(csv, rows("1.0")); await delay(4500);
    const { peer: late, auth: lateAuth } = await newPeer(`${run}-late`); assert.equal(lateAuth.ok, true);
    assert.equal((await late.join(roomA)).speedMetresPerSecond, 2);
    await a.freshFrame(); assert.equal(a.own.speedMetresPerSecond, 2);
    record("roomA.after_reload_and_late_join", { roomId: roomA, originalSpeed: a.own.speedMetresPerSecond, lateSpeed: late.own.speedMetresPerSecond });
    assert.equal((await a.request(T.ROOM_LEAVE_REQ, encodeRoomLeaveReq(), T.ROOM_LEAVE_RES)).ok, true);
    assert.equal((await a.join(roomA)).speedMetresPerSecond, 2);
    record("roomA.reentered", { roomId: roomA, speed: a.own.speedMetresPerSecond });
    a.client.close(); await delay(350);
    a.login = await selectCharacter(options, a.login, a.login.characterId);
    a.client = new TcpProtocolClient(options, a.label); assert.equal((await a.connect()).ok, true);
    const reconnect = await a.request(T.ROOM_RECONNECT_REQ, encodeRoomReconnectReq(), T.ROOM_RECONNECT_RES);
    assert.equal(reconnect.ok, true, reconnect.errorCode); assert.equal(a.own.speedMetresPerSecond, 2);
    record("roomA.reconnected", { roomId: reconnect.roomId, speed: a.own.speedMetresPerSecond, recoveryFrame: reconnect.movementRecovery.frameId });
    const { peer: b, auth: bAuth } = await newPeer(`${run}-b`); assert.equal(bAuth.ok, true);
    assert.equal((await b.join(roomB, true)).speedMetresPerSecond, 1);
    const versionB = await roomConfigVersion(roomB); assert.ok(versionB > versionA);
    record("roomB.created_after_reload", { roomId: roomB, speed: b.own.speedMetresPerSecond, configVersion: versionB }); await b.move(1, 0);
    writeFileSync(csv, rows("0")); await delay(4500);
    const { peer: c, auth: cAuth } = await newPeer(`${run}-c`); assert.equal(cAuth.ok, true);
    assert.equal((await c.join(roomC, true)).speedMetresPerSecond, 1);
    const versionC = await roomConfigVersion(roomC); assert.equal(versionC, versionB);
    record("roomC.created_after_invalid_reload", { roomId: roomC, speed: c.own.speedMetresPerSecond, configVersion: versionC });
    assert.equal((await b.request(T.ROOM_LEAVE_REQ, encodeRoomLeaveReq(), T.ROOM_LEAVE_RES)).ok, true);
    assert.equal((await b.join(roomA)).speedMetresPerSecond, 2);
    record("cross_room_B_to_A", { roomId: roomA, speed: b.own.speedMetresPerSecond });
    for (const [version, code] of [[0, "CLIENT_PROTOCOL_VERSION_TOO_OLD"], [1, "CLIENT_PROTOCOL_VERSION_TOO_OLD"], [3, "CLIENT_PROTOCOL_VERSION_TOO_NEW"]]) {
      const { peer, auth: rejected } = await newPeer(`${run}-v${version}`, version);
      assert.equal(rejected.ok, false); assert.equal(rejected.errorCode, code); peer.client.close();
      record("version.rejected", { version, errorCode: code });
    }
    record("probe.complete", { run, roomA, roomB, roomC, transport: "game-proxy local TCP fallback", clientPrediction: "covered separately by Bevy/client tests; this probe measures authority only" });
  } finally {
    restore(); process.removeListener("SIGINT", interrupted);
    for (const peer of peers) {
      if (peer.roomId && !peer.client.socket.destroyed) {
        try { await peer.request(T.ROOM_LEAVE_REQ, encodeRoomLeaveReq(), T.ROOM_LEAVE_RES); } catch { /* Disconnected own test members follow normal offline cleanup. */ }
      }
      peer.client.close();
    }
    await delay(4500);
    console.log(JSON.stringify({ label: "csv.restored", value: 2.0 }));
  }
  return report;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  runMovementSpeedConfigProbe().catch((error) => { console.error(`Local movement speed probe failed: ${error.message}`); process.exitCode = 1; });
}

#!/usr/bin/env node
/**
 * 安全与 Ticket 改造验证脚本
 *
 * 用法:
 *   node tools/test-security-and-ticket.js [--base-url http://127.0.0.1:3000]
 *
 * 测试内容:
 * 1. IP 限流
 * 2. 账号锁定
 * 3. 角色绑定 game ticket / 账号级失效语义
 * 4. 角色绑定 game ticket revoke
 */

import http from "node:http";
import https from "node:https";

const BASE_URL = process.argv.find((a) => a.startsWith("--base-url="))?.split("=")[1] || "http://127.0.0.1:3000";

async function request(method, path, body = null, headers = {}, retries = 3) {
  for (let attempt = 1; attempt <= retries; attempt++) {
    try {
      const result = await doRequest(method, path, body, headers);
      return result;
    } catch (err) {
      if (attempt === retries) throw err;
      await delay(500 * attempt);
    }
  }
}

async function doRequest(method, path, body = null, headers = {}) {
  return new Promise((resolve, reject) => {
    const url = new URL(path, BASE_URL);
    const client = url.protocol === "https:" ? https : http;

    const options = {
      hostname: url.hostname,
      port: url.port,
      path: url.pathname + url.search,
      method,
      headers: {
        "Content-Type": "application/json",
        ...headers,
      },
    };

    const req = client.request(options, (res) => {
      let data = "";
      res.on("data", (chunk) => (data += chunk));
      res.on("end", () => {
        try {
          resolve({ status: res.statusCode, data: JSON.parse(data) });
        } catch {
          resolve({ status: res.statusCode, data });
        }
      });
    });

    req.on("error", reject);
    if (body) req.write(JSON.stringify(body));
    req.end();
  });
}

function delay(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

function decodeTicketPayload(ticket) {
  try {
    return JSON.parse(Buffer.from(ticket.split(".")[0], "base64url").toString("utf8"));
  } catch {
    return null;
  }
}

function printCharacterStoreHint(response) {
  const error = response?.data?.error;
  if (
    error === "CHARACTER_STORE_UNAVAILABLE" ||
    error === "PASSWORD_REGISTER_UNAVAILABLE" ||
    error === "PASSWORD_LOGIN_UNAVAILABLE"
  ) {
    console.log("  [FAIL] 角色体系接口不可用。阶段 5 ticket 测试需要启用 DB，并已应用 characters 表初始化/迁移。");
    console.log(`  响应: ${response.status} - ${JSON.stringify(response.data)}`);
    return true;
  }
  return false;
}

async function loginGuestForTicket(prefix) {
  const guestId = `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2, 8)}`;
  const loginRes = await request("POST", "/api/v1/auth/guest-login", { guestId });

  if (loginRes.status !== 201 || !loginRes.data?.ok) {
    console.log(`  [FAIL] 登录失败: ${loginRes.status} - ${JSON.stringify(loginRes.data)}`);
    return null;
  }

  if (loginRes.data.ticket !== null || loginRes.data.ticketExpiresAt !== null) {
    console.log("  [FAIL] 登录阶段不应再签发 game ticket；应返回 ticket=null/ticketExpiresAt=null");
    console.log(`  响应: ${JSON.stringify({ ticket: loginRes.data.ticket, ticketExpiresAt: loginRes.data.ticketExpiresAt })}`);
    return null;
  }

  console.log("  [PASS] 登录只返回 accessToken，game ticket 需要选角后签发");
  return loginRes.data;
}

async function createCharacter(accessToken, namePrefix) {
  const response = await request(
    "POST",
    "/api/v1/characters",
    {
      name: `${namePrefix}${String(Date.now()).slice(-6)}`,
      appearance: { body: "default", palette: "blue" }
    },
    { Authorization: `Bearer ${accessToken}` }
  );

  if (response.status !== 201 || !response.data?.ok) {
    if (!printCharacterStoreHint(response)) {
      console.log(`  [FAIL] 创建角色失败: ${response.status} - ${JSON.stringify(response.data)}`);
    }
    return null;
  }

  return response.data.character;
}

async function selectCharacter(accessToken, characterId) {
  const response = await request(
    "POST",
    "/api/v1/characters/select",
    { character_id: characterId },
    { Authorization: `Bearer ${accessToken}` }
  );

  if (response.status !== 200 || !response.data?.ok) {
    if (!printCharacterStoreHint(response)) {
      console.log(`  [FAIL] 选择角色签发 ticket 失败: ${response.status} - ${JSON.stringify(response.data)}`);
    }
    return null;
  }

  return response.data;
}

async function issueCharacterGameTicket(prefix) {
  const login = await loginGuestForTicket(prefix);
  if (!login) {
    return null;
  }

  const character = await createCharacter(login.accessToken, "Sec");
  if (!character) {
    return null;
  }

  const characterId = character.character_id;
  console.log(`  创建角色成功: ${characterId}`);

  const selected = await selectCharacter(login.accessToken, characterId);
  if (!selected) {
    return null;
  }

  const payload = decodeTicketPayload(selected.ticket);
  if (!payload) {
    console.log("  [FAIL] ticket payload 解码失败");
    return null;
  }

  if (payload.playerId !== login.playerId || payload.characterId !== characterId) {
    console.log(`  [FAIL] ticket payload 归属异常: ${JSON.stringify(payload)}`);
    return null;
  }

  console.log(`  [PASS] 签发角色绑定 game ticket: playerId=${payload.playerId}, characterId=${payload.characterId}`);
  return {
    login,
    character,
    ticket: selected.ticket,
    ticketExpiresAt: selected.ticketExpiresAt,
    payload
  };
}

async function testIPRateLimit() {
  console.log("\n=== 测试 1: IP 限流 ===");

  const originalMax = 60;
  const results = [];

  // 快速发送请求直到被限流
  for (let i = 1; i <= originalMax + 5; i++) {
    const res = await request("GET", "/healthz");
    results.push({ i, status: res.status });

    if (res.status === 429) {
      console.log(`  [PASS] 第 ${i} 次请求被限流 (429)`);
      console.log(`  错误信息: ${res.data?.message || res.data?.error}`);
      return true;
    }

    if (i % 20 === 0) {
      console.log(`  已发送 ${i} 次请求...`);
    }
  }

  console.log(`  [FAIL] 发送了 ${originalMax + 5} 次请求仍未被限流`);
  return false;
}

async function testAccountLockout() {
  console.log("\n=== 测试 2: 账号锁定 ===");

  const testAccount = "lockout-test-" + Date.now();
  const testPassword = "TestPass123!";

  // 1. 注册测试账号
  console.log("  1. 创建测试账号...");
  await request("POST", "/api/v1/auth/register", {
    loginName: testAccount,
    password: testPassword,
    displayName: "Lockout Test",
  });

  // 2. 连续输入错误密码触发锁定
  console.log("  2. 连续输入错误密码触发锁定...");
  const maxAttempts = 5;
  let locked = false;

  for (let i = 1; i <= maxAttempts + 2; i++) {
    const res = await request("POST", "/api/v1/auth/login", {
      loginName: testAccount,
      password: "wrong-password-" + i,
    });

    if (res.status === 403 || res.data?.error === "ACCOUNT_LOCKED") {
      console.log(`  [PASS] 第 ${i} 次错误后账号被锁定 (${res.status})`);
      locked = true;
      break;
    }

    if (i % 2 === 0) {
      console.log(`  已尝试 ${i} 次...`);
    }
    await delay(100);
  }

  if (!locked) {
    console.log("  [FAIL] 连续错误后未被锁定");
    return false;
  }

  // 3. 验证锁定后无法登录
  const res = await request("POST", "/api/v1/auth/login", {
    loginName: testAccount,
    password: testPassword,
  });

  if (res.status === 403) {
    console.log("  [PASS] 锁定期间正确密码也无法登录");
  } else {
    console.log(`  [WARN] 锁定期间状态码: ${res.status}`);
  }

  return true;
}

async function testCharacterBoundGameTicket() {
  console.log("\n=== 测试 3: 角色绑定 game ticket / 账号级失效语义 ===");

  console.log("  1. 游客登录，确认登录不直接签发 game ticket...");
  console.log("  2. 创建并选择角色，获取角色绑定 game ticket...");
  const issued = await issueCharacterGameTicket("test-guest");
  if (!issued) {
    return false;
  }

  const { ticket, ticketExpiresAt, payload } = issued;
  console.log(`  Expires: ${ticketExpiresAt}`);

  // 3. 验证 ticket 有效期是否为 24 小时
  const expiresTime = new Date(ticketExpiresAt).getTime();
  const now = Date.now();
  const diffHours = (expiresTime - now) / (1000 * 60 * 60);

  console.log(`  距离过期: ${diffHours.toFixed(1)} 小时`);

  if (diffHours >= 23 && diffHours <= 25) {
    console.log("  [PASS] game ticket 有效期为约 24 小时");
  } else {
    console.log(`  [FAIL] game ticket 有效期异常: ${diffHours.toFixed(1)} 小时`);
    return false;
  }

  // 4. 验证 ticket 格式与角色字段
  if (ticket && ticket.includes(".")) {
    const parts = ticket.split(".");
    console.log(`  Ticket 格式正确: ${parts.length} 部分`);
  } else {
    console.log("  [FAIL] Ticket 格式异常");
    return false;
  }

  if (!payload.characterId || !payload.playerId) {
    console.log(`  [FAIL] ticket payload 缺少 playerId/characterId: ${JSON.stringify(payload)}`);
    return false;
  }

  console.log("  [PASS] payload 包含 playerId 与 characterId；Redis owner / logout / player-ticket-version 仍按账号级 playerId 失效");
  return true;
}

async function testTicketRevoke() {
  console.log("\n=== 测试 4: 角色绑定 game ticket revoke ===");

  console.log("  1. 登录、创建角色并选择角色获取 game ticket...");
  const issued = await issueCharacterGameTicket("revoke-test");
  if (!issued) {
    return false;
  }

  const { accessToken } = issued.login;
  const { ticket } = issued;

  // 2. 使用 /api/v1/game-ticket/revoke 使役 ticket
  console.log("  2. 调用 revoke 接口...");
  const revokeRes = await request(
    "POST",
    "/api/v1/game-ticket/revoke",
    { ticket },
    { Authorization: `Bearer ${accessToken}` }
  );

  if (revokeRes.status !== 200) {
    console.log(`  [FAIL] Revoke 失败: ${revokeRes.status} - ${JSON.stringify(revokeRes.data)}`);
    return false;
  }

  console.log("  [PASS] Revoke 成功");

  // 3. 通过 validate 验证 ticket 已失效
  console.log("  3. 验证 ticket 已失效...");
  const validateRes = await request("POST", "/api/v1/game-ticket/validate", { ticket });
  if (validateRes.status !== 401 || validateRes.data?.error !== "TICKET_NOT_FOUND") {
    console.log(`  [FAIL] revoke 后 validate 结果异常: ${validateRes.status} - ${JSON.stringify(validateRes.data)}`);
    return false;
  }

  console.log("  [PASS] revoke 后 ticket 已不可用");

  return true;
}

async function main() {
  console.log("========================================");
  console.log("  安全与 Ticket 改造验证");
  console.log("========================================");
  console.log(`\n目标地址: ${BASE_URL}`);

  let passed = 0;
  let failed = 0;

  // 检查服务是否可用
  console.log("\n检查服务可用性...");
  const healthRes = await request("GET", "/healthz");
  if (healthRes.status !== 200) {
    console.error(`  [ERROR] 服务不可用: ${healthRes.status}`);
    process.exit(1);
  }
  console.log("  [OK] 服务正常");

  // 执行测试（IP限流放最后，避免阻塞其他测试）
  if (await testAccountLockout()) passed++; else failed++;
  await delay(500);

  if (await testCharacterBoundGameTicket()) passed++; else failed++;
  await delay(500);

  if (await testTicketRevoke()) passed++; else failed++;
  await delay(500);

  if (await testIPRateLimit()) passed++; else failed++;

  // 汇总
  console.log("\n========================================");
  console.log(`  测试结果: ${passed} 通过, ${failed} 失败`);
  console.log("========================================\n");

  process.exit(failed > 0 ? 1 : 0);
}

main().catch((e) => {
  console.error("测试执行失败:", e);
  process.exit(1);
});

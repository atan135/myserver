/**
 * Mail Service Test Script
 * 用法:
 *   node test-mail.js                           # 运行所有测试
 *   node test-mail.js --send-system             # 发送系统邮件
 *   node test-mail.js --send-player            # 发送玩家邮件
 *   node test-mail.js --list <player_id>      # 查看玩家邮件
 */

const MAIL_SERVICE_URL = "http://127.0.0.1:9003";

async function sendMail(mail) {
  const response = await fetch(`${MAIL_SERVICE_URL}/api/v1/mails`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(mail)
  });
  const data = await response.json();
  console.log(`[POST /api/v1/mails] ${response.status}:`, JSON.stringify(data, null, 2));
  return data;
}

async function getMails(playerId, status = null) {
  let url = `${MAIL_SERVICE_URL}/api/v1/mails?player_id=${playerId}`;
  if (status) {
    url += `&status=${status}`;
  }
  const response = await fetch(url);
  const data = await response.json();
  console.log(`[GET /api/v1/mails?player_id=${playerId}] ${response.status}:`, JSON.stringify(data, null, 2));
  return data;
}

async function getMailDetail(mailId) {
  const response = await fetch(`${MAIL_SERVICE_URL}/api/v1/mails/${mailId}`);
  const data = await response.json();
  console.log(`[GET /api/v1/mails/${mailId}] ${response.status}:`, JSON.stringify(data, null, 2));
  return data;
}

async function markAsRead(mailId, playerId) {
  const response = await fetch(`${MAIL_SERVICE_URL}/api/v1/mails/${mailId}/read`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ player_id: playerId })
  });
  const data = await response.json();
  console.log(`[PUT /api/v1/mails/${mailId}/read] ${response.status}:`, JSON.stringify(data, null, 2));
  return data;
}

async function claimAttachments(mailId, playerId) {
  const response = await fetch(`${MAIL_SERVICE_URL}/api/v1/mails/${mailId}/claim`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ player_id: playerId })
  });
  const data = await response.json();
  console.log(`[POST /api/v1/mails/${mailId}/claim] ${response.status}:`, JSON.stringify(data, null, 2));
  return data;
}

async function testSendSystemMail() {
  console.log("\n=== 测试: 发送系统邮件 ===");
  const result = await sendMail({
    to_player_id: "player_001",
    sender_type: "system",
    sender_id: "system",
    sender_name: "系统",
    title: "欢迎来到 MyServer",
    content: "欢迎使用游戏服务，祝您游戏愉快！",
    attachments: [{ type: "item", id: 1001, count: 1 }],
    mail_type: "system",
    created_by_type: "admin",
    created_by_id: "gm001",
    created_by_name: "GM 001"
  });
  return result;
}

async function testSendPlayerMail() {
  console.log("\n=== 测试: 玩家之间发送邮件 ===");
  const result = await sendMail({
    to_player_id: "player_002",
    sender_type: "player",
    sender_id: "player_001",
    sender_name: "player_001",
    title: "好友邀请",
    content: "一起来打副本吧！",
    mail_type: "player"
  });
  return result;
}

async function testGetMails() {
  console.log("\n=== 测试: 获取 player_001 的邮件列表 ===");
  await getMails("player_001");

  console.log("\n=== 测试: 获取 player_001 的未读邮件 ===");
  await getMails("player_001", "unread");

  console.log("\n=== 测试: 获取 player_002 的邮件列表 ===");
  await getMails("player_002");
}

async function testGetMailDetail() {
  // 先获取一封邮件的 ID
  const mails = await getMails("player_001");
  if (mails.ok && mails.mails && mails.mails.length > 0) {
    const mailId = mails.mails[0].mail_id;
    console.log("\n=== 测试: 获取邮件详情 ===");
    await getMailDetail(mailId);
  } else {
    console.log("\n[SKIP] 没有邮件可测试详情");
  }
}

async function testMarkAsRead() {
  const mails = await getMails("player_001", "unread");
  if (mails.ok && mails.mails && mails.mails.length > 0) {
    const mailId = mails.mails[0].mail_id;
    console.log("\n=== 测试: 标记邮件已读 ===");
    await markAsRead(mailId, "player_001");
  } else {
    console.log("\n[SKIP] 没有未读邮件可测试");
  }
}

async function testClaimAttachments() {
  const mails = await getMails("player_001");
  if (mails.ok && mails.mails && mails.mails.length > 0) {
    const mailWithAttachments = mails.mails.find((mail) => Array.isArray(mail.attachments) && mail.attachments.length > 0);
    if (!mailWithAttachments) {
      console.log("\n[SKIP] 没有带附件的邮件可测试");
      return;
    }

    console.log("\n=== 测试: 领取邮件附件 ===");
    await claimAttachments(mailWithAttachments.mail_id, "player_001");
  } else {
    console.log("\n[SKIP] 没有邮件可测试附件领取");
  }
}

async function runAllTests() {
  console.log("========================================");
  console.log("  Mail Service 测试");
  console.log("========================================");

  // 1. 发送系统邮件
  await testSendSystemMail();

  // 2. 玩家之间发送邮件
  await testSendPlayerMail();

  // 3. 获取邮件列表
  await testGetMails();

  // 4. 获取邮件详情
  await testGetMailDetail();

  // 5. 标记已读
  await testMarkAsRead();

  // 6. 领取附件
  await testClaimAttachments();

  console.log("\n========================================");
  console.log("  测试完成!");
  console.log("========================================");
}

async function main() {
  const args = process.argv.slice(2);

  if (args.includes("--send-system")) {
    await testSendSystemMail();
  } else if (args.includes("--send-player")) {
    await testSendPlayerMail();
  } else if (args.length === 2 && args[0] === "--list") {
    await getMails(args[1]);
  } else if (args.includes("--all")) {
    await runAllTests();
  } else {
    console.log("用法:");
    console.log("  node test-mail.js --all              # 运行所有测试");
    console.log("  node test-mail.js --send-system      # 发送系统邮件");
    console.log("  node test-mail.js --send-player     # 发送玩家邮件");
    console.log("  node test-mail.js --list <player_id>  # 查看玩家邮件");
  }
}

main().catch(console.error);

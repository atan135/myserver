import { MESSAGE_TYPE } from "../constants.js";
import { fetchTicket, formatLoginSummary } from "../auth.js";
import { authenticateChatClient, connectToChatServer } from "./chat.js";

function shouldAutoGuestLogin(options) {
  return !options.ticket && !options.guestId && !options.loginName && !options.password;
}

async function fetchMailLogin(options, suffix) {
  const overrides = shouldAutoGuestLogin(options)
    ? { guestId: `${options.roomId}-${suffix}` }
    : {};
  const login = await fetchTicket(options, overrides);
  console.log("login:", JSON.stringify(formatLoginSummary(login), null, 2));
  return login;
}

function buildMailUrl(baseUrl, pathname, query = {}) {
  const url = new URL(pathname, baseUrl);

  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === "") {
      continue;
    }
    url.searchParams.set(key, String(value));
  }

  return url;
}

async function requestMailJson(url, options, init = {}) {
  const response = await fetch(url, {
    ...init,
    headers: {
      "content-type": "application/json",
      ...(init.headers || {})
    },
    signal: AbortSignal.timeout(options.timeoutMs)
  });

  const text = await response.text();
  let payload = null;

  if (text) {
    try {
      payload = JSON.parse(text);
    } catch {
      payload = { rawText: text };
    }
  }

  return {
    ok: response.ok,
    status: response.status,
    payload
  };
}

function printMailResponse(label, response) {
  console.log(`${label}:`, JSON.stringify({
    status: response.status,
    ok: response.ok,
    payload: response.payload
  }, null, 2));
}

function assertMailOk(label, response) {
  if (!response.ok) {
    const message = response.payload?.message || response.payload?.error || "mail request failed";
    throw new Error(`${label} failed (${response.status}): ${message}`);
  }

  return response.payload;
}

function parseAttachmentsJson(options) {
  if (!options.attachmentsJson) {
    return null;
  }

  const candidates = new Set();
  const raw = options.attachmentsJson.trim();
  candidates.add(raw);
  candidates.add(
    raw
      .replace(/\\:/g, ":")
      .replace(/\\,/g, ",")
      .replace(/\\(?=[A-Za-z_])/g, "")
      .replace(/\\(?=[[\]{}])/g, "")
      .replace(/([{,]\s*)([A-Za-z_][\w-]*)(\s*:)/g, '$1"$2"$3')
      .replace(/(:\s*)([A-Za-z_][\w-]*)(\s*[,}\]])/g, (match, prefix, value, suffix) => {
        if (value === "true" || value === "false" || value === "null") {
          return `${prefix}${value}${suffix}`;
        }

        return `${prefix}"${value}"${suffix}`;
      })
  );

  for (const candidate of candidates) {
    try {
      return JSON.parse(candidate);
    } catch {
      // Try the next candidate.
    }
  }

  try {
    return JSON.parse(raw);
  } catch (error) {
    throw new Error(
      `invalid --attachments-json: ${error.message}. ` +
      `PowerShell 请优先使用单引号，例如 --attachments-json '[{"type":"item","id":1001,"count":1}]'`
    );
  }
}

function requireMailId(options) {
  if (!options.mailId) {
    throw new Error("--mail-id is required");
  }

  return options.mailId;
}

async function resolveMailPlayerId(options, suffix) {
  if (options.mailPlayerId) {
    return { playerId: options.mailPlayerId, login: null };
  }

  const login = await fetchMailLogin(options, suffix);
  return { playerId: login.playerId, login };
}

async function resolveMailRecipientId(options, suffix) {
  if (options.mailToPlayerId) {
    return { playerId: options.mailToPlayerId, login: null };
  }

  const login = await fetchMailLogin(options, suffix);
  return { playerId: login.playerId, login };
}

function buildMailSendBody(options, toPlayerId) {
  const body = {
    to_player_id: toPlayerId,
    title: options.mailTitle,
    content: options.mailContent,
    mail_type: options.mailType,
    sender_type: options.senderType,
    sender_id: options.senderId,
    sender_name: options.senderName,
    created_by_type: options.createdByType,
    created_by_id: options.createdById,
    created_by_name: options.createdByName
  };

  const attachments = parseAttachmentsJson(options);
  if (attachments !== null) {
    body.attachments = attachments;
  }

  return body;
}

export async function runMailSend(options) {
  const { playerId, login } = await resolveMailRecipientId(options, "mail-send");
  const mailUrl = buildMailUrl(options.mailBaseUrl, "/api/v1/mails");
  const body = buildMailSendBody(options, playerId);

  console.log(`mail-base-url: ${options.mailBaseUrl}`);
  console.log(`to_player_id: ${playerId}`);
  if (login) {
    console.log(`recipient login playerId: ${login.playerId}`);
  }

  const response = await requestMailJson(mailUrl, options, {
    method: "POST",
    body: JSON.stringify(body)
  });

  printMailResponse("mail.send", response);
  assertMailOk("mail.send", response);
}

export async function runMailList(options) {
  const { playerId, login } = await resolveMailPlayerId(options, "mail-list");
  const mailUrl = buildMailUrl(options.mailBaseUrl, "/api/v1/mails", {
    player_id: playerId,
    status: options.mailStatus,
    limit: options.limit,
    offset: options.mailOffset
  });

  console.log(`mail-base-url: ${options.mailBaseUrl}`);
  console.log(`player_id: ${playerId}`);
  if (login) {
    console.log(`login playerId: ${login.playerId}`);
  }

  const response = await requestMailJson(mailUrl, options);
  printMailResponse("mail.list", response);
  const payload = assertMailOk("mail.list", response);

  console.log(`mail count: ${payload.mails?.length || 0}, unread_count: ${payload.unread_count ?? 0}`);
}

export async function runMailGet(options) {
  const mailId = requireMailId(options);
  const mailUrl = buildMailUrl(options.mailBaseUrl, `/api/v1/mails/${mailId}`);

  console.log(`mail-base-url: ${options.mailBaseUrl}`);
  console.log(`mail_id: ${mailId}`);

  const response = await requestMailJson(mailUrl, options);
  printMailResponse("mail.get", response);
  assertMailOk("mail.get", response);
}

export async function runMailRead(options) {
  const mailId = requireMailId(options);
  const { playerId, login } = await resolveMailPlayerId(options, "mail-read");
  const mailUrl = buildMailUrl(options.mailBaseUrl, `/api/v1/mails/${mailId}/read`);

  console.log(`mail-base-url: ${options.mailBaseUrl}`);
  console.log(`mail_id: ${mailId}`);
  console.log(`player_id: ${playerId}`);
  if (login) {
    console.log(`login playerId: ${login.playerId}`);
  }

  const response = await requestMailJson(mailUrl, options, {
    method: "PUT",
    body: JSON.stringify({ player_id: playerId })
  });

  printMailResponse("mail.read", response);
  const payload = assertMailOk("mail.read", response);
  console.log(`updated: ${payload.updated}`);
}

export async function runMailClaim(options) {
  const mailId = requireMailId(options);
  const { playerId, login } = await resolveMailPlayerId(options, "mail-claim");
  const mailUrl = buildMailUrl(options.mailBaseUrl, `/api/v1/mails/${mailId}/claim`);

  console.log(`mail-base-url: ${options.mailBaseUrl}`);
  console.log(`mail_id: ${mailId}`);
  console.log(`player_id: ${playerId}`);
  if (login) {
    console.log(`login playerId: ${login.playerId}`);
  }

  const response = await requestMailJson(mailUrl, options, {
    method: "POST",
    body: JSON.stringify({ player_id: playerId })
  });

  printMailResponse("mail.claim", response);
  const payload = assertMailOk("mail.claim", response);
  console.log(`claimed: ${payload.claimed}, already_claimed: ${payload.already_claimed}, status: ${payload.status}`);
}

export async function runMailSendAndNotify(options) {
  const login = await fetchMailLogin(options, "mail-notify");
  const mailUrl = buildMailUrl(options.mailBaseUrl, "/api/v1/mails");
  const chatClient = await connectToChatServer(options);

  try {
    await authenticateChatClient(chatClient, options, login, 1);
    console.log(`watching MAIL_NOTIFY_PUSH for player ${login.playerId}`);

    const response = await requestMailJson(mailUrl, options, {
      method: "POST",
      body: JSON.stringify(buildMailSendBody(options, login.playerId))
    });
    printMailResponse("mail.send", response);
    assertMailOk("mail.send", response);

    const notify = await chatClient.readUntil(
      options.mailWatchSeconds * 1000,
      (packet) => packet.messageType === MESSAGE_TYPE.MAIL_NOTIFY_PUSH,
      "mailNotify"
    );

    console.log("mail.notify:", JSON.stringify(notify, null, 2));
  } finally {
    chatClient.close();
  }
}

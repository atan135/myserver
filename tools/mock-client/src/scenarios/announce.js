function buildAnnounceUrl(baseUrl, pathname, query = {}) {
  const url = new URL(pathname, baseUrl);

  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === "") {
      continue;
    }
    url.searchParams.set(key, String(value));
  }

  return url;
}

async function requestAnnounceJson(url, options, init = {}) {
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

function printAnnounceResponse(label, response) {
  console.log(`${label}:`, JSON.stringify({
    status: response.status,
    ok: response.ok,
    payload: response.payload
  }, null, 2));
}

function assertAnnounceOk(label, response) {
  if (!response.ok) {
    const message =
      response.payload?.message ||
      response.payload?.error ||
      "announce request failed";
    throw new Error(`${label} failed (${response.status}): ${message}`);
  }

  return response.payload;
}

function requireAnnounceId(options) {
  if (!options.announceId) {
    throw new Error("--announce-id is required");
  }

  return options.announceId;
}

function parseOptionalInteger(value, fieldName) {
  if (value === undefined || value === null || value === "") {
    return null;
  }

  const parsed = Number.parseInt(String(value), 10);
  if (!Number.isInteger(parsed)) {
    throw new Error(`${fieldName} must be an integer`);
  }

  return parsed;
}

function buildAnnounceCreateBody(options) {
  const body = {
    title: options.announceTitle || "Mock announcement from mock-client",
    content:
      options.announceContent || "Hello from mock-client announcement!",
    locale: options.announceLocale || "default",
    type: options.announceType || "banner",
    target_group: options.announceTargetGroup || "all"
  };

  const priority = parseOptionalInteger(
    options.announcePriority,
    "announce-priority"
  );
  if (priority !== null) {
    body.priority = priority;
  }

  if (options.announceStartTime) {
    body.start_time = options.announceStartTime;
  }

  if (options.announceEndTime) {
    body.end_time = options.announceEndTime;
  } else {
    body.duration_seconds =
      parseOptionalInteger(
        options.announceDurationSeconds,
        "announce-duration-seconds"
      ) ?? 3600;
  }

  return body;
}

function buildAnnounceUpdateBody(options) {
  const body = {};

  if (options.announceTitle) {
    body.title = options.announceTitle;
  }

  if (options.announceContent) {
    body.content = options.announceContent;
  }

  if (options.announceLocale) {
    body.locale = options.announceLocale;
  }

  if (options.announceType) {
    body.type = options.announceType;
  }

  if (options.announceTargetGroup) {
    body.target_group = options.announceTargetGroup;
  }

  const priority = parseOptionalInteger(
    options.announcePriority,
    "announce-priority"
  );
  if (priority !== null) {
    body.priority = priority;
  }

  if (options.announceStartTime) {
    body.start_time = options.announceStartTime;
  }

  if (options.announceEndTime) {
    body.end_time = options.announceEndTime;
  }

  const durationSeconds = parseOptionalInteger(
    options.announceDurationSeconds,
    "announce-duration-seconds"
  );
  if (durationSeconds !== null) {
    body.duration_seconds = durationSeconds;
  }

  if (Object.keys(body).length === 0) {
    throw new Error(
      "announce-update requires at least one of --announce-title, --announce-content, --announce-locale, --announce-priority, --announce-type, --announce-target-group, --announce-start-time, --announce-end-time, --announce-duration-seconds"
    );
  }

  return body;
}

export async function runAnnounceList(options) {
  const announceUrl = buildAnnounceUrl(
    options.announceBaseUrl,
    "/api/v1/announcements",
    {
      locale: options.announceLocale,
      priority: options.announcePriority,
      target_group: options.announceTargetGroup,
      limit: options.limit,
      offset: options.announceOffset,
      active_only: options.announceActiveOnly
    }
  );

  console.log(`announce-base-url: ${options.announceBaseUrl}`);
  const response = await requestAnnounceJson(announceUrl, options);
  printAnnounceResponse("announce.list", response);
  const payload = assertAnnounceOk("announce.list", response);
  console.log(`announcement count: ${payload.announcements?.length || 0}`);
}

export async function runAnnounceGet(options) {
  const announceId = requireAnnounceId(options);
  const announceUrl = buildAnnounceUrl(
    options.announceBaseUrl,
    `/api/v1/announcements/${announceId}`
  );

  console.log(`announce-base-url: ${options.announceBaseUrl}`);
  console.log(`announce_id: ${announceId}`);

  const response = await requestAnnounceJson(announceUrl, options);
  printAnnounceResponse("announce.get", response);
  assertAnnounceOk("announce.get", response);
}

export async function runAnnounceCreate(options) {
  const announceUrl = buildAnnounceUrl(
    options.announceBaseUrl,
    "/api/v1/announcements"
  );
  const body = buildAnnounceCreateBody(options);

  console.log(`announce-base-url: ${options.announceBaseUrl}`);

  const response = await requestAnnounceJson(announceUrl, options, {
    method: "POST",
    body: JSON.stringify(body)
  });
  printAnnounceResponse("announce.create", response);
  const payload = assertAnnounceOk("announce.create", response);
  console.log(`announce_id: ${payload.announcement?.announce_id || ""}`);
}

export async function runAnnounceUpdate(options) {
  const announceId = requireAnnounceId(options);
  const announceUrl = buildAnnounceUrl(
    options.announceBaseUrl,
    `/api/v1/announcements/${announceId}`
  );
  const body = buildAnnounceUpdateBody(options);

  console.log(`announce-base-url: ${options.announceBaseUrl}`);
  console.log(`announce_id: ${announceId}`);

  const response = await requestAnnounceJson(announceUrl, options, {
    method: "PUT",
    body: JSON.stringify(body)
  });
  printAnnounceResponse("announce.update", response);
  assertAnnounceOk("announce.update", response);
}

export async function runAnnounceDelete(options) {
  const announceId = requireAnnounceId(options);
  const announceUrl = buildAnnounceUrl(
    options.announceBaseUrl,
    `/api/v1/announcements/${announceId}`
  );

  console.log(`announce-base-url: ${options.announceBaseUrl}`);
  console.log(`announce_id: ${announceId}`);

  const response = await requestAnnounceJson(announceUrl, options, {
    method: "DELETE"
  });
  printAnnounceResponse("announce.delete", response);
  const payload = assertAnnounceOk("announce.delete", response);
  console.log(`deleted: ${payload.deleted}`);
}

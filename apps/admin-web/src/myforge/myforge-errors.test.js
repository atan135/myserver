import assert from "node:assert/strict";
import test from "node:test";
import { normalizeMyforgeError } from "../api/myforge-errors.js";

test("MyForge errors distinguish timeout, offline, permission and task 404 states", () => {
  assert.deepEqual(normalizeMyforgeError({ code: "ECONNABORTED" }), {
    title: "接口请求超时",
    description: "MyForge 接口响应超时，请稍后重试。"
  });
  assert.equal(normalizeMyforgeError({}).title, "管理接口不可达");
  assert.equal(normalizeMyforgeError({ response: { status: 403, data: {} } }).title, "无权限访问");
  assert.deepEqual(
    normalizeMyforgeError(
      { response: { status: 404, data: { error: "MYFORGE_TASK_NOT_FOUND" } } },
      { taskDetailLookup: true, notFoundMessage: "指定任务不存在。" }
    ),
    { title: "任务不存在", description: "指定任务不存在。" }
  );
});

test("MyForge errors surface a structured server message before the local fallback", () => {
  const result = normalizeMyforgeError(
    {
      response: {
        status: 400,
        data: { error: "MYFORGE_PROMPT_INVALID", message: "prompt.theme is invalid" }
      }
    },
    { title: "任务创建失败", fallbackMessage: "本地兜底" }
  );
  assert.deepEqual(result, {
    title: "任务创建失败",
    description: "prompt.theme is invalid"
  });
});

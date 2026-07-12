import assert from "node:assert/strict";
import test from "node:test";
import { normalizeFangyuanBlueprintRequest } from "../../../admin-api/src/myforge/myforge-task-input.js";
import {
  ACTIVE_TASK_STATUSES,
  FANGYUAN_BLUEPRINT_TASK_TYPE,
  TERMINAL_TASK_STATUSES,
  buildFangyuanTaskRequest,
  dangerFullAccessState,
  formatDuration,
  formatJson,
  isActiveTaskStatus,
  isCurrentTaskQueryAttempt,
  isTerminalTaskStatus,
  queueReasonLabel,
  renderFangyuanBlueprintPrompt,
  taskDurationMs,
  taskStatusLabel,
  taskTypeLabel,
  utf8ByteLength,
  validateFangyuanTaskForm,
  validateMyforgePath
} from "./task-utils.js";

function validForm(overrides = {}) {
  return {
    agentKey: "dev-pc-001\u0000myforge-local",
    theme: "竹林庭院",
    primitiveLimit: 120,
    bounds: { width: 80, depth: 60, height: 30 },
    requirements: ["保留中心通道", "使用分层屋檐"],
    artifactFile: "artifacts/fangyuan/courtyard/main.ron",
    consumerTargetFile: "project/assets/fangyuan/courtyard/main.ron",
    useRulesFile: true,
    rulesFile: "rules/fangyuan/default/main.md",
    ...overrides
  };
}

const agent = { agentId: "dev-pc-001", projectId: "myforge-local" };

test("MyForge paths accept nested roots and enforce each frozen prefix and suffix", () => {
  assert.equal(validateMyforgePath("artifacts/fangyuan/a/b.ron", "artifactFile"), null);
  assert.equal(validateMyforgePath("project/assets/fangyuan/a/b.ron", "consumerTargetFile"), null);
  assert.equal(validateMyforgePath("", "consumerTargetFile"), null);
  assert.equal(validateMyforgePath("rules/fangyuan/a/b.md", "rulesFile"), null);

  assert.match(validateMyforgePath("artifacts/other/a.ron", "artifactFile"), /artifacts\/fangyuan/);
  assert.match(validateMyforgePath("artifacts/fangyuan/a.md", "artifactFile"), /\.ron/);
  assert.match(validateMyforgePath("project/assets/fangyuan/a.md", "consumerTargetFile"), /\.ron/);
  assert.match(validateMyforgePath("rules/fangyuan/a.ron", "rulesFile"), /\.md/);
});

test("MyForge paths reject traversal, absolute paths, backslashes, devices and trailing dots", () => {
  const invalid = [
    "artifacts/fangyuan/../escape.ron",
    "/artifacts/fangyuan/a.ron",
    "artifacts\\fangyuan\\a.ron",
    "artifacts/fangyuan/CON.ron",
    "artifacts/fangyuan/LPT1/file.ron",
    "artifacts/fangyuan/folder./a.ron",
    "artifacts/fangyuan//a.ron",
    "artifacts/fangyuan/a:bad.ron"
  ];
  for (const value of invalid) {
    assert.notEqual(validateMyforgePath(value, "artifactFile"), null, value);
  }
});

test("MyForge paths count UTF-8 bytes rather than JavaScript code units", () => {
  const validName = "界".repeat(160);
  const tooLongName = "界".repeat(170);
  assert.equal(utf8ByteLength(validName), 480);
  assert.equal(validateMyforgePath(`artifacts/fangyuan/${validName}.ron`, "artifactFile"), null);
  assert.match(
    validateMyforgePath(`artifacts/fangyuan/${tooLongName}.ron`, "artifactFile"),
    /512/
  );
});

test("form validation trims business text and rejects duplicate or excessive requirements", () => {
  const duplicate = validForm({ requirements: [" 同一要求 ", "同一要求"] });
  assert.match(validateFangyuanTaskForm(duplicate, agent).errors.requirements, /重复/);

  const tooMany = validForm({ requirements: Array.from({ length: 33 }, (_, index) => `要求 ${index}`) });
  assert.match(validateFangyuanTaskForm(tooMany, agent).errors.requirements, /1 到 32/);

  const itemTooLong = validForm({ requirements: ["界".repeat(167)] });
  assert.match(validateFangyuanTaskForm(itemTooLong, agent).errors.requirements, /500/);

  const totalTooLarge = validForm({
    requirements: Array.from({ length: 17 }, (_, index) => `${index}-${"x".repeat(497)}`)
  });
  assert.match(validateFangyuanTaskForm(totalTooLarge, agent).errors.requirements, /8192/);
});

test("form validation enforces required fields, numeric bounds and rendered prompt bytes", () => {
  const missing = validForm({ theme: " ", primitiveLimit: 0, bounds: { width: 1.5, depth: 1001, height: 0 } });
  const result = validateFangyuanTaskForm(missing, null);
  assert.equal(result.valid, false);
  assert.deepEqual(Object.keys(result.errors).sort(), [
    "agentKey", "depth", "height", "primitiveLimit", "theme", "width"
  ]);

  const escapedPrompt = validForm({
    requirements: Array.from({ length: 17 }, (_, index) => `${index}${"\\".repeat(480)}`)
  });
  assert.match(validateFangyuanTaskForm(escapedPrompt, agent).errors.requirements, /16384/);
});

test("request builder emits the exact API contract and omits an empty optional consumer target", () => {
  const withConsumer = buildFangyuanTaskRequest(validForm(), agent);
  assert.deepEqual(Object.keys(withConsumer), [
    "agentId", "projectId", "artifactFile", "rulesFile", "prompt", "consumerTargetFile"
  ]);
  assert.equal(withConsumer.prompt.theme, "竹林庭院");

  const withoutConsumer = buildFangyuanTaskRequest(validForm({ consumerTargetFile: "" }), agent);
  assert.equal(Object.prototype.hasOwnProperty.call(withoutConsumer, "consumerTargetFile"), false);
  assert.deepEqual(Object.keys(withoutConsumer.prompt), ["theme", "primitiveLimit", "bounds", "requirements"]);
});

test("request builder always emits rulesFile and mirrors the backend nullable contract", () => {
  const withRules = buildFangyuanTaskRequest(validForm(), agent);
  assert.equal(withRules.rulesFile, "rules/fangyuan/default/main.md");
  assert.equal(
    renderFangyuanBlueprintPrompt(withRules),
    normalizeFangyuanBlueprintRequest(withRules).renderedPrompt
  );

  const withoutRules = buildFangyuanTaskRequest(validForm({ useRulesFile: false }), agent);
  assert.equal(Object.prototype.hasOwnProperty.call(withoutRules, "rulesFile"), true);
  assert.equal(withoutRules.rulesFile, null);
  assert.equal(validateFangyuanTaskForm(validForm({ useRulesFile: false, rulesFile: "" }), agent).valid, true);
  assert.equal(
    renderFangyuanBlueprintPrompt(withoutRules),
    normalizeFangyuanBlueprintRequest(withoutRules).renderedPrompt
  );

  const missingRulesKey = { ...withoutRules };
  delete missingRulesKey.rulesFile;
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(missingRulesKey),
    { code: "INVALID_REQUEST" }
  );
  assert.throws(
    () => buildFangyuanTaskRequest(validForm({ useRulesFile: undefined }), agent),
    /explicit boolean/
  );
});

test("rules path validation only applies when the explicit rule toggle is enabled", () => {
  const enabled = validateFangyuanTaskForm(validForm({ rulesFile: "rules/other/a.md" }), agent);
  assert.match(enabled.errors.rulesFile, /rules\/fangyuan/);

  const missingToggle = validateFangyuanTaskForm(validForm({ useRulesFile: undefined }), agent);
  assert.match(missingToggle.errors.rulesFile, /是否使用规则文件/);
});

test("danger full access display preserves enabled, disabled and pending states", () => {
  assert.deepEqual(dangerFullAccessState(true), {
    key: "enabled",
    label: "整机最高权限",
    tagType: "danger",
    description: "Agent 在本机绕过 Codex 审批与沙箱，以整机最高权限执行。"
  });
  assert.equal(dangerFullAccessState(false).label, "受限权限");
  assert.equal(dangerFullAccessState(null).label, "待调度确认");
  assert.equal(dangerFullAccessState(undefined).key, "pending");
});

test("active and terminal status helpers cover every frozen state without overlap", () => {
  for (const status of ACTIVE_TASK_STATUSES) {
    assert.equal(isActiveTaskStatus(status), true);
    assert.equal(isTerminalTaskStatus(status), false);
  }
  for (const status of TERMINAL_TASK_STATUSES) {
    assert.equal(isTerminalTaskStatus(status), true);
    assert.equal(isActiveTaskStatus(status), false);
  }
  assert.equal(isActiveTaskStatus("unknown"), false);
  assert.equal(isTerminalTaskStatus("unknown"), false);
});

test("task type display uses the frozen backend value", () => {
  assert.equal(FANGYUAN_BLUEPRINT_TASK_TYPE, "fangyuan.blueprint.generate");
  assert.equal(taskTypeLabel(FANGYUAN_BLUEPRINT_TASK_TYPE), "方圆灵构蓝图");
  assert.equal(taskTypeLabel("fangyuan_blueprint"), "fangyuan_blueprint");
  assert.equal(taskTypeLabel(null), "--");
});

test("task query attempts require both the latest request sequence and filter revision", () => {
  const current = { sequence: 8, revision: 3 };
  assert.equal(isCurrentTaskQueryAttempt({ sequence: 8, revision: 3 }, current), true);
  assert.equal(isCurrentTaskQueryAttempt({ sequence: 7, revision: 3 }, current), false);
  assert.equal(isCurrentTaskQueryAttempt({ sequence: 8, revision: 2 }, current), false);
  assert.equal(isCurrentTaskQueryAttempt(null, current), false);
});

test("task display formatting handles nulls, durations, statuses and structured JSON", () => {
  assert.equal(formatDuration(null), "--");
  assert.equal(formatDuration(999), "999 ms");
  assert.equal(formatDuration(1500), "1.5 s");
  assert.equal(formatDuration(65_000), "1 min 5 s");
  assert.equal(taskStatusLabel("completed_with_errors"), "完成但有错误");
  assert.equal(queueReasonLabel("agent_offline"), "Agent 离线，等待连接");
  assert.equal(formatJson(null), "--");
  assert.equal(formatJson({ ok: true }), "{\n  \"ok\": true\n}");

  assert.equal(taskDurationMs({
    createdAt: "2026-07-12T00:00:00.000Z",
    dispatchedAt: "2026-07-12T00:00:01.000Z",
    startedAt: "2026-07-12T00:00:02.000Z",
    completedAt: "2026-07-12T00:00:05.500Z"
  }), 3500);
  assert.equal(taskDurationMs({ createdAt: null, completedAt: null }), null);
});

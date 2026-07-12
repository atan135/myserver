import assert from "node:assert/strict";
import test from "node:test";

import {
  assertEmptyCancelBody,
  normalizeFangyuanBlueprintRequest,
  normalizeTaskListQuery
} from "./myforge-task-input.js";

function validRequest(overrides = {}) {
  return {
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    artifactFile: "artifacts/fangyuan/home.ron",
    consumerTargetFile: "project/assets/fangyuan/home.ron",
    rulesFile: "rules/fangyuan/rules.md",
    prompt: {
      theme: "  fire home  ",
      primitiveLimit: 200,
      bounds: { width: 40, depth: 30, height: 20 },
      requirements: ["  central furnace  ", "three platforms"]
    },
    ...overrides
  };
}

function assertCode(code) {
  return (error) => {
    assert.equal(error.code, code);
    return true;
  };
}

test("fangyuan typed request normalizes business text and renders a fixed non-shell command", () => {
  const normalized = normalizeFangyuanBlueprintRequest(validRequest());
  assert.equal(normalized.prompt.theme, "fire home");
  assert.deepEqual(normalized.prompt.requirements, ["central furnace", "three platforms"]);
  assert.match(normalized.renderedPrompt, /MANDATORY CONSTRAINTS/);
  assert.match(normalized.renderedPrompt, /Use only cube and sphere primitives/);
  assert.match(normalized.renderedPrompt, /Modify only "artifacts\/fangyuan\/home\.ron"/);
  assert.match(normalized.commandPreview, /^codex exec <agent-local-permission-mode> --ephemeral --color never/);
  assert.match(normalized.commandPreview, /danger_full_access=unresolved/);
  assert.doesNotMatch(normalized.commandPreview, /central furnace/);

  const withoutConsumer = normalizeFangyuanBlueprintRequest(validRequest({ consumerTargetFile: undefined }));
  assert.equal(withoutConsumer.consumerTargetFile, null);
  assert.match(withoutConsumer.renderedPrompt, /consumerTargetFile metadata: not provided/);

  const withoutRules = normalizeFangyuanBlueprintRequest(validRequest({ rulesFile: null }));
  assert.equal(withoutRules.rulesFile, null);
  assert.match(withoutRules.renderedPrompt, /No repository rules file was provided/);
  assert.doesNotMatch(withoutRules.renderedPrompt, /Read and follow only the rules copy/);
  const missingRulesField = validRequest();
  delete missingRulesField.rulesFile;
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(missingRulesField),
    assertCode("INVALID_REQUEST")
  );
});

test("fangyuan typed request rejects unknown execution controls at every object boundary", () => {
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({ command: "cmd.exe" })),
    assertCode("INVALID_REQUEST")
  );
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({ profile: "anything" })),
    assertCode("INVALID_REQUEST")
  );
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({ dryRun: true })),
    assertCode("INVALID_REQUEST")
  );
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({ dangerFullAccess: true })),
    assertCode("INVALID_REQUEST")
  );
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({
      prompt: { ...validRequest().prompt, renderedPrompt: "override" }
    })),
    assertCode("MYFORGE_PROMPT_INVALID")
  );
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({
      prompt: {
        ...validRequest().prompt,
        bounds: { width: 1, depth: 1, height: 1, radius: 1 }
      }
    })),
    assertCode("MYFORGE_PROMPT_INVALID")
  );
});

test("fangyuan path validation rejects absolute, traversal, drive, URI, and backslash formats", () => {
  const invalidArtifacts = [
    "/artifacts/fangyuan/home.ron",
    "artifacts/fangyuan/../home.ron",
    "C:/artifacts/fangyuan/home.ron",
    "artifacts\\fangyuan\\home.ron",
    "file:artifacts/fangyuan/home.ron",
    "artifacts//fangyuan/home.ron",
    "artifacts/fangyuan/folder./home.ron",
    "artifacts/fangyuan/CON.ron",
    "artifacts/fangyuan/home.md"
  ];
  for (const artifactFile of invalidArtifacts) {
    assert.throws(
      () => normalizeFangyuanBlueprintRequest(validRequest({ artifactFile })),
      assertCode("MYFORGE_TARGET_PATH_INVALID"),
      artifactFile
    );
  }
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({ rulesFile: "../rules/fangyuan/rules.md" })),
    assertCode("MYFORGE_TARGET_PATH_INVALID")
  );
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({ consumerTargetFile: "project/assets/other/home.ron" })),
    assertCode("MYFORGE_TARGET_PATH_INVALID")
  );
});

test("fangyuan prompt validation enforces integers, bounds, normalized uniqueness, bytes, and controls", () => {
  for (const primitiveLimit of [0, 1001, 1.5, "200"] ) {
    assert.throws(
      () => normalizeFangyuanBlueprintRequest(validRequest({
        prompt: { ...validRequest().prompt, primitiveLimit }
      })),
      assertCode("MYFORGE_PROMPT_INVALID")
    );
  }
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({
      prompt: { ...validRequest().prompt, bounds: { width: 0, depth: 1, height: 1 } }
    })),
    assertCode("MYFORGE_PROMPT_INVALID")
  );
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({
      prompt: { ...validRequest().prompt, requirements: ["same", " same "] }
    })),
    assertCode("MYFORGE_PROMPT_INVALID")
  );
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({
      prompt: { ...validRequest().prompt, requirements: ["bad\nline"] }
    })),
    assertCode("MYFORGE_PROMPT_INVALID")
  );
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({
      prompt: { ...validRequest().prompt, theme: "x".repeat(201) }
    })),
    assertCode("MYFORGE_PROMPT_INVALID")
  );
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest({
      prompt: { ...validRequest().prompt, requirements: Array.from({ length: 17 }, (_, index) => `${index}-${"x".repeat(490)}`) }
    })),
    assertCode("MYFORGE_PROMPT_INVALID")
  );
});

test("rendered prompt and list query limits return explicit request errors", () => {
  assert.throws(
    () => normalizeFangyuanBlueprintRequest(validRequest(), { maxRenderedPromptBytes: 128 }),
    (error) => error.code === "MYFORGE_PROMPT_TOO_LARGE" && error.statusCode === 413
  );
  assert.deepEqual(normalizeTaskListQuery({ limit: "100", offset: "0", status: "running" }), {
    projectId: null,
    agentId: null,
    status: "running",
    limit: 100,
    offset: 0
  });
  assert.throws(() => normalizeTaskListQuery({ limit: "101" }), assertCode("INVALID_REQUEST"));
  assert.throws(() => normalizeTaskListQuery({ limit: "1e2" }), assertCode("INVALID_REQUEST"));
  assert.throws(() => normalizeTaskListQuery({ unknown: "x" }), assertCode("INVALID_REQUEST"));
  assert.doesNotThrow(() => assertEmptyCancelBody(undefined));
  assert.doesNotThrow(() => assertEmptyCancelBody({}));
  assert.throws(() => assertEmptyCancelBody(null), assertCode("INVALID_REQUEST"));
  assert.throws(() => assertEmptyCancelBody({ reason: "free text" }), assertCode("INVALID_REQUEST"));
});

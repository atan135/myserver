import assert from "node:assert/strict";
import test from "node:test";
import { normalizePreflightResponse, normalizeRecordsResponse, preflightErrorSuggestions, recordStatusLabel, recordStatusTag } from "./activity-records.js";

test("activity records normalize pagination and operational statuses", () => {
  assert.equal(normalizeRecordsResponse({ data: { items: [{ status: "retryable_failure" }], total: 2 } }).items.length, 1);
  assert.equal(recordStatusLabel("capacity_exhausted"), "容量不足");
  assert.equal(recordStatusTag("manual_review"), "warning");
  assert.equal(recordStatusLabel("future_state"), "future_state");
});

test("preflight responses and field-level suggestions remain visible", () => {
  assert.equal(normalizePreflightResponse({ data: { valid: true, errors: [] } }).valid, true);
  assert.deepEqual(preflightErrorSuggestions({ details: [{ path: "typeConfig.pool_items[0].weight", code: "INVALID", message: "bad", suggestion: "set positive" }] }), [{ path: "typeConfig.pool_items[0].weight", code: "INVALID", message: "bad", suggestion: "set positive" }]);
  assert.deepEqual(preflightErrorSuggestions({ response: { data: { details: [{ path: "endAt", code: "INVALID_RANGE", message: "end before start" }] } } }), [{ path: "endAt", code: "INVALID_RANGE", message: "end before start", suggestion: "请根据字段要求修正后重试" }]);
});

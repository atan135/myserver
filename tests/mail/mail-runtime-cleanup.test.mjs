import assert from "node:assert/strict";
import { test } from "node:test";

import { createMailAcceptanceDatabase, runWithCleanup } from "../helpers/runtime.mjs";

test("mail acceptance database rejects an unsafe name before connecting", async () => {
  await assert.rejects(
    () => createMailAcceptanceDatabase({
      adminUrl: "postgresql://unused:unused@127.0.0.1:1/postgres",
      databaseName: "myserver_mail",
      migrationPaths: []
    }),
    /myserver_mail_acceptance_<run-id>/
  );
});

test("runWithCleanup preserves both the primary failure and every cleanup failure", async () => {
  const primary = new Error("primary flow failed");
  const firstCleanup = new Error("first cleanup failed");
  const secondCleanup = new Error("second cleanup failed");
  const calls = [];

  await assert.rejects(
    () => runWithCleanup(
      async () => {
        calls.push("work");
        throw primary;
      },
      [
        async () => {
          calls.push("cleanup-1");
          throw firstCleanup;
        },
        async () => {
          calls.push("cleanup-2");
          throw secondCleanup;
        },
        async () => {
          calls.push("cleanup-3");
        }
      ],
      "test cleanup failed"
    ),
    (error) => {
      assert.ok(error instanceof AggregateError);
      assert.equal(error.cause, primary);
      assert.deepEqual(error.errors, [primary, firstCleanup, secondCleanup]);
      assert.match(error.message, /primary flow failed/);
      assert.match(error.message, /test cleanup failed/);
      return true;
    }
  );

  assert.deepEqual(calls, ["work", "cleanup-1", "cleanup-2", "cleanup-3"]);
});

test("runWithCleanup preserves the original error when cleanup succeeds", async () => {
  const primary = new Error("primary only");

  await assert.rejects(
    () => runWithCleanup(async () => {
      throw primary;
    }, [async () => {}]),
    (error) => error === primary
  );
});

test("runWithCleanup reports cleanup-only failures after a successful flow", async () => {
  const cleanupError = new Error("cleanup only");

  await assert.rejects(
    () => runWithCleanup(async () => "ok", [async () => {
      throw cleanupError;
    }], "test cleanup failed"),
    (error) => {
      assert.ok(error instanceof AggregateError);
      assert.deepEqual(error.errors, [cleanupError]);
      return true;
    }
  );
});

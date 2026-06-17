#!/usr/bin/env node

import { decodeGlobalIdInput, GlobalIdError } from "../packages/global-id/node/index.js";

const input = process.argv[2];

if (!input) {
  console.error("Usage: node tools/global-id-decode.js <global-id>");
  process.exit(2);
}

try {
  const decoded = decodeGlobalIdInput(input);
  console.log(JSON.stringify(decoded, null, 2));
} catch (error) {
  if (error instanceof GlobalIdError) {
    console.error(JSON.stringify({
      ok: false,
      error_code: error.code,
      message: error.message,
      details: error.details
    }, null, 2));
    process.exit(1);
  }
  throw error;
}

import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import { getConfig } from "../src/config.js";
import { createMySqlPool } from "../src/mysql-client.js";
import { MySqlAuthStore } from "../src/mysql-store.js";
import {
  assertValidLoginName,
  createPasswordSalt,
  hashPassword
} from "../src/password-utils.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const authHttpRoot = path.resolve(__dirname, "..");

const DEFAULT_ACCOUNTS = [
  {
    loginName: "test001",
    password: "Passw0rd!",
    displayName: "Test User 001"
  },
  {
    loginName: "test002",
    password: "Passw0rd!",
    displayName: "Test User 002"
  },
  {
    loginName: "gm001",
    password: "AdminPass123!",
    displayName: "Game Master 001"
  }
];

function printUsage() {
  console.log(`Usage:
  npm run seed:test-accounts
  npm run seed:test-accounts -- --account test003 --password Passw0rd! --display-name "Test User 003"
  npm run seed:test-accounts -- --file ./scripts/test-accounts.example.json`);
}

function parseArgs(argv) {
  const parsed = {
    help: false,
    file: null,
    account: null,
    password: null,
    displayName: null,
    status: "active"
  };

  for (let index = 0; index < argv.length; index += 1) {
    const token = argv[index];

    switch (token) {
      case "--help":
      case "-h":
        parsed.help = true;
        break;
      case "--file":
        parsed.file = argv[index + 1] || null;
        index += 1;
        break;
      case "--account":
        parsed.account = argv[index + 1] || null;
        index += 1;
        break;
      case "--password":
        parsed.password = argv[index + 1] || null;
        index += 1;
        break;
      case "--display-name":
        parsed.displayName = argv[index + 1] || null;
        index += 1;
        break;
      case "--status":
        parsed.status = argv[index + 1] || "active";
        index += 1;
        break;
      default:
        throw new Error(`Unknown argument: ${token}`);
    }
  }

  return parsed;
}

function resolveInputFile(filePath) {
  if (!filePath) {
    return null;
  }

  if (path.isAbsolute(filePath)) {
    return filePath;
  }

  return path.resolve(process.env.INIT_CWD || process.cwd(), filePath);
}

async function loadAccountsFromFile(filePath) {
  const fileContent = await fs.readFile(filePath, "utf8");
  const payload = JSON.parse(fileContent);
  if (!Array.isArray(payload)) {
    throw new Error("account seed file must be a JSON array");
  }

  return payload;
}

function validateSeedAccount(account, index) {
  const loginName = assertValidLoginName(account.loginName);
  const password = String(account.password || "");
  const displayName = account.displayName ? String(account.displayName) : null;
  const status = account.status ? String(account.status) : "active";

  if (password.length < 6) {
    throw new Error(`accounts[${index}].password must be at least 6 characters`);
  }

  if (displayName && displayName.length > 64) {
    throw new Error(`accounts[${index}].displayName must be at most 64 characters`);
  }

  return {
    loginName,
    password,
    displayName,
    status
  };
}

async function buildSeedAccounts(options) {
  if (options.file) {
    return loadAccountsFromFile(resolveInputFile(options.file));
  }

  if (options.account || options.password || options.displayName) {
    if (!options.account || !options.password) {
      throw new Error("--account and --password must be provided together");
    }

    return [
      {
        loginName: options.account,
        password: options.password,
        displayName: options.displayName,
        status: options.status
      }
    ];
  }

  return DEFAULT_ACCOUNTS;
}

async function main() {
  process.chdir(authHttpRoot);

  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    printUsage();
    return;
  }

  const rawAccounts = await buildSeedAccounts(options);
  const accounts = rawAccounts.map(validateSeedAccount);

  const config = getConfig();
  config.mysqlEnabled = true;

  const pool = await createMySqlPool(config);
  if (!pool) {
    throw new Error("MySQL pool is unavailable");
  }

  const store = new MySqlAuthStore(pool);

  try {
    for (const account of accounts) {
      const passwordSalt = createPasswordSalt();
      const passwordHash = hashPassword(account.password, passwordSalt);
      const result = await store.upsertPasswordAccount({
        loginName: account.loginName,
        displayName: account.displayName,
        status: account.status,
        passwordAlgo: "scrypt",
        passwordSalt,
        passwordHash
      });

      await store.appendAuthAudit({
        playerId: result.playerId,
        eventType: "seed_password_account",
        details: {
          loginName: result.loginName,
          displayName: result.displayName,
          created: result.created
        }
      });

      console.log(
        `${result.created ? "CREATED" : "UPDATED"} loginName=${result.loginName} playerId=${result.playerId}`
      );
    }
  } finally {
    await pool.end();
  }
}

if (process.argv[1] === __filename) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}

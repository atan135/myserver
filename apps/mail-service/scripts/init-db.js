import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "url";

import dotenv from "dotenv";
import pg from "pg";

const { Client } = pg;

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// Load .env
dotenv.config({ path: path.resolve(__dirname, "../.env") });

const databaseUrl =
  process.env.DATABASE_URL ||
  "postgres://postgres:password@127.0.0.1:5432/myserver_mail";

function getDatabaseName(url) {
  const dbName = url.pathname.replace(/^\//, "");
  if (!dbName) {
    throw new Error("DATABASE_URL must include a database name");
  }
  return dbName;
}

function getMaintenanceDatabaseUrl(url) {
  const maintenanceUrl = new URL(url);
  maintenanceUrl.pathname = "/postgres";
  return maintenanceUrl.toString();
}

async function initDatabase() {
  console.log("Initializing mail service database...");
  console.log(`Database URL: ${databaseUrl}`);

  const url = new URL(databaseUrl);
  const dbName = getDatabaseName(url);
  let adminClient;
  let client;
  try {
    adminClient = new Client({ connectionString: getMaintenanceDatabaseUrl(url) });
    await adminClient.connect();

    const existing = await adminClient.query(
      "SELECT 1 FROM pg_database WHERE datname = $1",
      [dbName]
    );
    if (existing.rowCount === 0) {
      await adminClient.query(`CREATE DATABASE "${dbName.replaceAll('"', '""')}"`);
    }
    console.log(`Database '${dbName}' created or already exists`);

    const sqlPath = path.resolve(__dirname, "../db/init.sql");
    const sqlContent = fs.readFileSync(sqlPath, "utf8");

    client = new Client({ connectionString: databaseUrl });
    await client.connect();
    await client.query(sqlContent);

    console.log("\nDatabase initialization complete!");
  } catch (error) {
    console.error("Failed to initialize database:", error.message);
    process.exit(1);
  } finally {
    if (client) {
      await client.end();
    }
    if (adminClient) {
      await adminClient.end();
    }
  }
}

initDatabase();

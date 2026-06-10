#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const MIGRATION_PATTERN = /^(\d{4,})_([a-z0-9][a-z0-9_]*?)\.sql$/;

function parseArgs(argv) {
  const args = {
    mode: "migrate",
    migrationsDir: path.resolve("db/migrations"),
    mysqlUrl: process.env.MYSQL_URL || process.env.DATABASE_URL || ""
  };

  for (const arg of argv) {
    if (arg === "--check") {
      args.mode = "check";
    } else if (arg === "--dry-run") {
      args.mode = "dry-run";
    } else if (arg === "--list") {
      args.mode = "list";
    } else if (arg.startsWith("--dir=")) {
      args.migrationsDir = path.resolve(arg.slice("--dir=".length));
    } else if (arg.startsWith("--mysql-url=")) {
      args.mysqlUrl = arg.slice("--mysql-url=".length);
    } else if (arg === "--help" || arg === "-h") {
      args.mode = "help";
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }

  return args;
}

function usage() {
  console.log(`Usage: node tools/db-migrate.js [--check|--dry-run|--list] [--dir=db/migrations] [--mysql-url=mysql://...]

Modes:
  --check    Validate migration names, order, duplicate versions, and checksums without DB access.
  --dry-run  Print migrations in execution order with checksums without DB access.
  --list     Alias of --dry-run.
  default    Apply unapplied migrations to MYSQL_URL or DATABASE_URL.`);
}

function readMigrations(migrationsDir) {
  if (!fs.existsSync(migrationsDir)) {
    throw new Error(`migrations directory not found: ${migrationsDir}`);
  }

  const entries = fs.readdirSync(migrationsDir, { withFileTypes: true });
  const migrations = entries
    .filter((entry) => entry.isFile() && entry.name.endsWith(".sql"))
    .map((entry) => {
      const match = MIGRATION_PATTERN.exec(entry.name);
      if (!match) {
        throw new Error(
          `invalid migration filename "${entry.name}", expected "0001_descriptive_name.sql"`
        );
      }

      const filePath = path.join(migrationsDir, entry.name);
      const sql = fs.readFileSync(filePath, "utf8");
      const checksum = crypto.createHash("sha256").update(sql, "utf8").digest("hex");
      return {
        version: match[1],
        name: match[2],
        fileName: entry.name,
        filePath,
        sql,
        checksum
      };
    })
    .sort((left, right) => left.fileName.localeCompare(right.fileName));

  validateMigrations(migrations);
  return migrations;
}

function validateMigrations(migrations) {
  if (migrations.length === 0) {
    throw new Error("no migration files found");
  }

  const versions = new Set();
  const names = new Set();
  let previousFileName = "";
  for (const migration of migrations) {
    if (versions.has(migration.version)) {
      throw new Error(`duplicate migration version: ${migration.version}`);
    }
    if (names.has(migration.name)) {
      throw new Error(`duplicate migration name: ${migration.name}`);
    }
    if (previousFileName && previousFileName.localeCompare(migration.fileName) >= 0) {
      throw new Error(`migration order is not strictly increasing at ${migration.fileName}`);
    }
    if (migration.sql.trim().length === 0) {
      throw new Error(`migration is empty: ${migration.fileName}`);
    }
    versions.add(migration.version);
    names.add(migration.name);
    previousFileName = migration.fileName;
  }
}

function printMigrations(migrations, label) {
  console.log(`${label}: ${migrations.length} migration(s)`);
  for (const migration of migrations) {
    console.log(`${migration.version} ${migration.name} ${migration.checksum} ${migration.fileName}`);
  }
}

async function loadMysql() {
  try {
    return await import("mysql2/promise");
  } catch (error) {
    if (error.code === "ERR_MODULE_NOT_FOUND") {
      throw new Error(
        "mysql2 is required for real migrations. Install workspace dependencies or run --check/--dry-run."
      );
    }
    throw error;
  }
}

async function applyMigrations(migrations, mysqlUrl) {
  if (!mysqlUrl) {
    throw new Error("MYSQL_URL or DATABASE_URL is required for real migrations");
  }

  const mysql = await loadMysql();
  const connection = await mysql.createConnection({
    uri: mysqlUrl,
    multipleStatements: true
  });

  try {
    await connection.beginTransaction();
    await ensureSchemaMigrationsTable(connection, migrations);

    const [rows] = await connection.query(
      "SELECT version, name, checksum FROM schema_migrations ORDER BY version ASC"
    );
    const applied = new Map(rows.map((row) => [row.version, row]));
    const pending = [];

    for (const migration of migrations) {
      const existing = applied.get(migration.version);
      if (!existing) {
        pending.push(migration);
        continue;
      }
      if (existing.name !== migration.name || existing.checksum !== migration.checksum) {
        throw new Error(
          `applied migration ${migration.version} does not match local file ${migration.fileName}`
        );
      }
    }

    for (const migration of pending) {
      console.log(`applying ${migration.fileName}`);
      await connection.query(migration.sql);
      await connection.query(
        "INSERT INTO schema_migrations (version, name, checksum) VALUES (?, ?, ?)",
        [migration.version, migration.name, migration.checksum]
      );
    }

    await connection.commit();
    console.log(`database migrations complete: ${pending.length} applied, ${applied.size} already applied`);
  } catch (error) {
    await connection.rollback();
    throw error;
  } finally {
    await connection.end();
  }
}

async function ensureSchemaMigrationsTable(connection, migrations) {
  const bootstrap = migrations.find((migration) => migration.version === "0001");
  if (!bootstrap) {
    throw new Error("0001_create_schema_migrations.sql is required to bootstrap migrations");
  }
  await connection.query(bootstrap.sql);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.mode === "help") {
    usage();
    return;
  }

  const migrations = readMigrations(args.migrationsDir);
  if (args.mode === "check") {
    printMigrations(migrations, "migration check passed");
    return;
  }
  if (args.mode === "dry-run" || args.mode === "list") {
    printMigrations(migrations, "migration dry-run");
    return;
  }

  await applyMigrations(migrations, args.mysqlUrl);
}

main().catch((error) => {
  console.error(`db migration failed: ${error.message}`);
  process.exit(1);
});

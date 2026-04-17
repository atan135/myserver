import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "url";

import dotenv from "dotenv";
import mysql from "mysql2/promise";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

dotenv.config({ path: path.resolve(__dirname, "../.env") });

const mysqlUrl =
  process.env.MYSQL_URL ||
  "mysql://root:password@127.0.0.1:3306/myserver_announce";

async function initDatabase() {
  console.log("Initializing announce service database...");
  console.log(`MySQL URL: ${mysqlUrl}`);

  const url = new URL(mysqlUrl);
  const connectionConfig = {
    host: url.hostname,
    port: url.port ? Number.parseInt(url.port, 10) : 3306,
    user: decodeURIComponent(url.username),
    password: decodeURIComponent(url.password),
    multipleStatements: true
  };

  const dbName = url.pathname.replace(/^\//, "");

  let connection;
  try {
    connection = await mysql.createConnection(connectionConfig);
    await connection.query(
      `CREATE DATABASE IF NOT EXISTS \`${dbName}\` DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci`
    );
    await connection.query(`USE \`${dbName}\``);

    const sqlPath = path.resolve(__dirname, "../db/init.sql");
    const sqlContent = fs.readFileSync(sqlPath, "utf8");
    const statements = sqlContent
      .split(";")
      .map((statement) => statement.trim())
      .filter((statement) => statement.length > 0 && !statement.startsWith("--"));

    for (const statement of statements) {
      await connection.query(statement);
    }

    console.log("Announce service database initialization complete.");
  } catch (error) {
    console.error("Failed to initialize announce database:", error.message);
    process.exit(1);
  } finally {
    if (connection) {
      await connection.end();
    }
  }
}

initDatabase();

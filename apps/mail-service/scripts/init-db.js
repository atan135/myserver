import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "url";

import dotenv from "dotenv";
import mysql from "mysql2/promise";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// Load .env
dotenv.config({ path: path.resolve(__dirname, "../.env") });

const mysqlUrl = process.env.MYSQL_URL || "mysql://root:password@127.0.0.1:3306/myserver_mail";

async function initDatabase() {
  console.log("Initializing mail service database...");
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
    // Connect without database first
    connection = await mysql.createConnection(connectionConfig);
    console.log(`Connected to MySQL server`);

    // Create database
    await connection.query(`CREATE DATABASE IF NOT EXISTS \`${dbName}\` DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci`);
    console.log(`Database '${dbName}' created or already exists`);

    // Use the database
    await connection.query(`USE \`${dbName}\``);

    // Read and execute init.sql
    const sqlPath = path.resolve(__dirname, "../db/init.sql");
    const sqlContent = fs.readFileSync(sqlPath, "utf8");

    // Split by semicolon and filter empty statements
    const statements = sqlContent
      .split(";")
      .map((s) => s.trim())
      .filter((s) => s.length > 0 && !s.startsWith("--"));

    for (const statement of statements) {
      try {
        await connection.query(statement);
        console.log(`Executed: ${statement.substring(0, 60)}...`);
      } catch (err) {
        console.error(`Failed to execute: ${statement.substring(0, 60)}...`);
        console.error(`Error: ${err.message}`);
      }
    }

    console.log("\nDatabase initialization complete!");
  } catch (error) {
    console.error("Failed to initialize database:", error.message);
    process.exit(1);
  } finally {
    if (connection) {
      await connection.end();
    }
  }
}

initDatabase();

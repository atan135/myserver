#!/usr/bin/env node
/**
 * 读取 apps/port.txt 并更新各服务目录下 .env 的端口配置
 *
 * 规则:
 * - 固定端口 (fixed): 自动更新对应服务的 .env
 * - 动态端口 (dynamic): 跳过，提示通过启动参数配置
 *
 * 用法: node update-ports.js
 */

import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const APPS_DIR = __dirname;
const PORT_FILE = path.join(APPS_DIR, "port.txt");

// 服务名 -> { envPath, portVars }
// portVars: { 变量名: 端口号 } 或 { 变量名: "从port.txt读取的port" }
const SERVICE_CONFIG = {
  "auth-http": {
    envPath: "auth-http/.env",
    portVar: "PORT",
  },
  "admin-api": {
    envPath: "admin-api/.env",
    portVar: "PORT",
  },
  "game-proxy": {
    envPath: "game-proxy/.env",
    portVar: "PROXY_PORT",
  },
  "game-server": {
    envPath: "game-server/.env",
    portVar: "GAME_PORT",
  },
  "game-server-admin": {
    // game-server-admin 的配置写入 game-server/.env 的 ADMIN_PORT
    envPath: "game-server/.env",
    portVar: "ADMIN_PORT",
  },
};

function updateEnvFile(envPath, portVar, port) {
  const content = fs.readFileSync(envPath, "utf8");
  const lines = content.split("\n");
  let found = false;
  let foundExact = false;

  const newLines = lines.map((line) => {
    const trimmed = line.trim();
    const equalsIdx = trimmed.indexOf("=");
    if (equalsIdx === -1) return line;

    const varName = trimmed.slice(0, equalsIdx).trim();
    if (varName === portVar) {
      found = true;
      const currentValue = trimmed.slice(equalsIdx + 1).trim();
      if (currentValue === String(port)) {
        foundExact = true;
      }
      return `${portVar}=${port}`;
    }
    return line;
  });

  if (!found) {
    // 变量不存在，添加
    newLines.push(`${portVar}=${port}`);
    fs.writeFileSync(envPath, newLines.join("\n"));
    return { action: "added", from: undefined, to: port };
  }

  if (!foundExact) {
    fs.writeFileSync(envPath, newLines.join("\n"));
    const currentValue = lines.find((l) => {
      const t = l.trim();
      const ei = t.indexOf("=");
      return ei !== -1 && t.slice(0, ei).trim() === portVar;
    });
    const from = currentValue ? currentValue.split("=")[1].trim() : undefined;
    return { action: "updated", from, to: port };
  }

  return { action: "unchanged", from: port, to: port };
}

function parsePortFile(content) {
  const ports = {};
  const lines = content.split("\n");

  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;

    // 支持格式:
    // service: port
    // service: port:fixed
    // service: port:dynamic
    const parts = trimmed.split(":");
    const server = parts[0].trim();
    const port = parseInt(parts[1].trim(), 10);
    const type = parts[2] ? parts[2].trim() : "fixed";

    if (server && !isNaN(port)) {
      ports[server] = { port, type };
    }
  }

  return ports;
}

function main() {
  console.log("========================================");
  console.log("  MyServer 端口配置更新工具");
  console.log("========================================");
  console.log();

  if (!fs.existsSync(PORT_FILE)) {
    console.error(`错误: port.txt 未找到 (${PORT_FILE})`);
    process.exit(1);
  }

  const portContent = fs.readFileSync(PORT_FILE, "utf8");
  const ports = parsePortFile(portContent);

  if (Object.keys(ports).length === 0) {
    console.error("错误: port.txt 中没有有效的端口配置");
    process.exit(1);
  }

  console.log("读取到的端口配置:");
  for (const [server, { port, type }] of Object.entries(ports)) {
    const typeLabel = type === "fixed" ? "固定" : "动态";
    console.log(`  ${server}: ${port} (${typeLabel})`);
  }
  console.log();

  const updatedServices = new Set();
  let updatedCount = 0;
  let skippedCount = 0;

  for (const [server, { port, type }] of Object.entries(ports)) {
    if (type === "dynamic") {
      console.log(`[跳过] ${server}: ${port} (动态端口，通过启动参数配置)`);
      skippedCount++;
      continue;
    }

    const config = SERVICE_CONFIG[server];
    if (!config) {
      console.log(`[跳过] ${server}: 未配置更新规则`);
      skippedCount++;
      continue;
    }

    const envPath = path.join(APPS_DIR, config.envPath);
    if (!fs.existsSync(envPath)) {
      console.log(`[跳过] ${server}: ${config.envPath} 不存在`);
      skippedCount++;
      continue;
    }

    const result = updateEnvFile(envPath, config.portVar, port);
    updatedServices.add(config.envPath);

    if (result.action === "added") {
      console.log(
        `[新增] ${server} -> ${config.envPath}: ${config.portVar}=${port}`,
      );
      updatedCount++;
    } else if (result.action === "updated") {
      console.log(
        `[更新] ${server} -> ${config.envPath}: ${config.portVar}: ${result.from} -> ${result.to}`,
      );
      updatedCount++;
    } else {
      console.log(`[无需更新] ${server}: ${config.portVar}=${port}`);
    }
  }

  console.log();
  console.log("========================================");
  console.log(`完成: ${updatedCount} 个配置已更新, ${skippedCount} 个已跳过`);
  console.log("========================================");
}

main();

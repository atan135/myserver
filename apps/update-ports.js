#!/usr/bin/env node
/**
 * 读取 apps/port.txt 并更新各服务目录下 .env 的 GAME_PORT
 * 用法: node update-ports.js
 */

const fs = require('fs');
const path = require('path');

const APPS_DIR = path.join(__dirname);
const PORT_FILE = path.join(APPS_DIR, 'port.txt');

function parsePortFile(content) {
  const ports = {};
  const lines = content.split('\n');
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;
    const colonIdx = trimmed.indexOf(':');
    if (colonIdx === -1) continue;
    const server = trimmed.slice(0, colonIdx).trim();
    const port = parseInt(trimmed.slice(colonIdx + 1).trim(), 10);
    if (server && !isNaN(port)) {
      ports[server] = port;
    }
  }
  return ports;
}

function updateEnvFile(envPath, newPort) {
  const content = fs.readFileSync(envPath, 'utf8');
  const lines = content.split('\n');
  let updated = false;
  const newLines = lines.map(line => {
    if (line.startsWith('GAME_PORT=')) {
      const newLine = `GAME_PORT=${newPort}`;
      if (line !== newLine) {
        updated = true;
        return newLine;
      }
    }
    return line;
  });
  if (updated) {
    fs.writeFileSync(envPath, newLines.join('\n'));
    return true;
  }
  return false;
}

function main() {
  if (!fs.existsSync(PORT_FILE)) {
    console.error(`Error: port.txt not found at ${PORT_FILE}`);
    process.exit(1);
  }

  const portContent = fs.readFileSync(PORT_FILE, 'utf8');
  const ports = parsePortFile(portContent);

  if (Object.keys(ports).length === 0) {
    console.error('Error: no valid port entries found in port.txt');
    process.exit(1);
  }

  console.log('Parsed ports:', ports);
  console.log();

  let updatedCount = 0;
  let skippedCount = 0;

  for (const [server, port] of Object.entries(ports)) {
    const envPath = path.join(APPS_DIR, server, '.env');
    if (!fs.existsSync(envPath)) {
      console.log(`[SKIP] ${server}: .env not found`);
      skippedCount++;
      continue;
    }

    const changed = updateEnvFile(envPath, port);
    if (changed) {
      console.log(`[UPDATE] ${server}: GAME_PORT set to ${port}`);
      updatedCount++;
    } else {
      console.log(`[OK] ${server}: GAME_PORT already ${port}`);
    }
  }

  console.log();
  console.log(`Done: ${updatedCount} updated, ${skippedCount} skipped`);
}

main();

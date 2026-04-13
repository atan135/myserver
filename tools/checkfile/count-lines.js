#!/usr/bin/env node
/**
 * 代码行数统计工具
 * 扫描 apps/ 目录下的源代码文件，按行数排序输出
 * 用法: node count-lines.js [--threshold <行数>]
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..', '..');

// 源代码后缀及目录排除规则
const SOURCE_EXTENSIONS = ['.js', '.ts', '.rs', '.go', '.py', '.java', '.cs', '.cpp', '.c', '.h'];
const EXCLUDE_DIRS = ['target', 'node_modules', 'Library', 'PackageCache', 'build', '.git', 'dist', 'out'];
const EXCLUDE_FILES = ['package-lock.json', 'yarn.lock', 'pnpm-lock.yaml'];

const THRESHOLD = parseInt(process.argv.find(arg => arg === '--threshold') ? process.argv[process.argv.indexOf('--threshold') + 1] : '0');

function walk(dir, files = []) {
  if (!fs.existsSync(dir)) return files;

  const entries = fs.readdirSync(dir, { withFileTypes: true });
  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (!EXCLUDE_DIRS.includes(entry.name)) {
        walk(fullPath, files);
      }
    } else if (entry.isFile()) {
      const ext = path.extname(entry.name).toLowerCase();
      if (SOURCE_EXTENSIONS.includes(ext) && !EXCLUDE_FILES.includes(entry.name)) {
        files.push(fullPath);
      }
    }
  }
  return files;
}

function countLines(filePath) {
  const content = fs.readFileSync(filePath, 'utf-8');
  return content.split('\n').length;
}

function relativePath(filePath) {
  return path.relative(ROOT, filePath);
}

const files = walk(path.join(ROOT, 'apps'));

const stats = files.map(f => ({
  path: f,
  relPath: relativePath(f),
  lines: countLines(f),
}));

stats.sort((a, b) => b.lines - a.lines);

const total = stats.reduce((sum, s) => sum + s.lines, 0);
const maxLines = stats[0]?.lines || 0;

// 打印统计表
console.log('\n  代码行数统计\n');
console.log(`  ${'行数'.padEnd(8)} ${'文件路径'.padEnd(60)}`);
console.log(`  ${'─'.repeat(8)} ${'─'.repeat(60)}`);

for (const s of stats) {
  const flag = THRESHOLD > 0 && s.lines >= THRESHOLD ? '  >>>' : '';
  console.log(`  ${String(s.lines).padEnd(8)} ${s.relPath.padEnd(60)}${flag}`);
}

console.log(`  ${'─'.repeat(8)} ${'─'.repeat(60)}`);
console.log(`  ${'总计: ' + total + ' 行'.padEnd(8)} ${stats.length + ' 个文件'}`);
console.log(`  ${'最大文件: ' + maxLines + ' 行'.padEnd(8)} ${stats[0]?.relPath || ''}`);
console.log();

// 超阈值的文件
if (THRESHOLD > 0) {
  const largeFiles = stats.filter(s => s.lines >= THRESHOLD);
  if (largeFiles.length > 0) {
    console.log(`  ⚠ 超过 ${THRESHOLD} 行的文件 (建议拆分重构):\n`);
    for (const s of largeFiles) {
      console.log(`    - ${s.relPath} (${s.lines} 行)`);
    }
    console.log();
  } else {
    console.log(`  ✓ 没有文件超过 ${THRESHOLD} 行\n`);
  }
}

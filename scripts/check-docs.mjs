// 文档自检：围栏闭合、内链可达、表格列数一致。
// 用法：node scripts/check-docs.mjs
import { readFileSync, readdirSync, existsSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const docsDir = join(root, "docs");
const files = [
  join(root, "README.md"),
  ...readdirSync(docsDir)
    .filter((name) => name.endsWith(".md"))
    .map((name) => join(docsDir, name)),
];

let problems = 0;
const report = (file, line, message) => {
  problems += 1;
  console.log(file.replace(root + "/", "") + ":" + line + " " + message);
};

for (const file of files) {
  const text = readFileSync(file, "utf8");
  const lines = text.split("\n");

  let fence = false;
  lines.forEach((line) => {
    if (line.trimStart().startsWith("```")) fence = !fence;
  });
  if (fence) report(file, lines.length, "代码围栏未闭合");

  // 表格：连续以 | 开头的行视为一个表，列数必须一致
  let block = [];
  const flush = () => {
    if (block.length >= 2) {
      const base = block[0].cells;
      block.forEach((item) => {
        if (item.cells !== base) report(file, item.line, "表格列数 " + item.cells + " 与表头 " + base + " 不一致");
      });
    }
    block = [];
  };
  lines.forEach((line, index) => {
    const trimmed = line.trim();
    if (trimmed.startsWith("|")) {
      const cells = trimmed.replace(/\\\|/g, "").replace(/^\||\|$/g, "").split("|").length;
      block.push({ line: index + 1, cells });
    } else {
      flush();
    }
  });
  flush();

  // 内链：只查相对路径与图片
  const linkRe = /!?\[[^\]]*\]\(([^)\s]+)\)/g;
  let match;
  while ((match = linkRe.exec(text)) !== null) {
    const target = match[1];
    if (/^(https?:|mailto:|#)/.test(target)) continue;
    const clean = decodeURIComponent(target.split("#")[0]);
    if (!clean) continue;
    const path = resolve(dirname(file), clean);
    if (!existsSync(path) || !statSync(path).isFile()) {
      const line = text.slice(0, match.index).split("\n").length;
      report(file, line, "内链文件不存在: " + target);
    }
  }
}

if (problems) {
  console.log("共 " + problems + " 处问题");
  process.exit(1);
}
console.log("docs ok: " + files.length + " files");

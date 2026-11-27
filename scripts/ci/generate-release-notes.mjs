#!/usr/bin/env node
/**
 * Generate GitHub Release notes with categorized commit history as the primary
 * content, followed by concise, up-to-date installation instructions.
 *
 * Usage:
 *   node scripts/ci/generate-release-notes.mjs --version 1.0.1 --out release-notes.md
 *   node scripts/ci/generate-release-notes.mjs --version 1.0.1 --sign-macos false --from v1.0.0 --to HEAD
 */

import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const SEMVER_RE =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][A-Za-z0-9-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][A-Za-z0-9-]*))*))?(?:\+[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*)?$/;

const CONVENTIONAL_RE =
  /^([a-zA-Z]+)(?:\(([^)]+)\))?(!)?\s*[:：]\s*(.+)$/;

const SECTIONS = [
  { key: "feat", heading: "### ✨ 新增功能" },
  { key: "fix", heading: "### 🐛 问题修复" },
  { key: "perf", heading: "### ⚡ 优化与体验" },
  { key: "other", heading: "### 📝 其他变更" },
  { key: "chore", heading: "### 🔧 工程与维护" },
];

export function parseSemver(raw) {
  const cleaned = String(raw || "").trim().replace(/^v/, "");
  const match = SEMVER_RE.exec(cleaned);
  if (!match) return null;
  return {
    raw: cleaned,
    major: BigInt(match[1]),
    minor: BigInt(match[2]),
    patch: BigInt(match[3]),
    pre: match[4] ? match[4].split(".") : [],
  };
}

function compareIdentifier(a, b) {
  if (a === b) return 0;
  const an = /^\d+$/.test(a);
  const bn = /^\d+$/.test(b);
  if (an && bn) return BigInt(a) < BigInt(b) ? -1 : 1;
  if (an !== bn) return an ? -1 : 1;
  return a < b ? -1 : 1;
}

export function compareSemver(a, b) {
  const va = typeof a === "string" ? parseSemver(a) : a;
  const vb = typeof b === "string" ? parseSemver(b) : b;
  if (!va || !vb) return 0;
  for (const field of ["major", "minor", "patch"]) {
    if (va[field] !== vb[field]) return va[field] < vb[field] ? -1 : 1;
  }
  if (va.pre.length === 0 || vb.pre.length === 0) {
    return Number(va.pre.length === 0) - Number(vb.pre.length === 0);
  }
  const len = Math.min(va.pre.length, vb.pre.length);
  for (let i = 0; i < len; i += 1) {
    const order = compareIdentifier(va.pre[i], vb.pre[i]);
    if (order) return order;
  }
  return va.pre.length - vb.pre.length;
}

/** GitHub's Latest follows the same stable SemVer ordering as the update feed. */
export function shouldMarkLatest(version, releases) {
  const target = parseSemver(version);
  if (!target || target.pre.length) return false;
  return releases.every(release => {
    const published = parseSemver(release.tag_name);
    return release.draft || release.prerelease || !published || published.pre.length > 0
      || compareSemver(target, published) > 0;
  });
}

/**
 * Pick the best previous reachable SemVer tag for `targetVersion`.
 * - Stable releases compare against the latest reachable stable tag (< targetVersion),
 *   so changes introduced in intermediate RCs are still included in the stable release.
 * - Prereleases compare against the latest reachable tag of any channel (< targetVersion).
 */
export function selectPreviousTag(tags, targetVersion) {
  const target = parseSemver(targetVersion);
  if (!target) return null;
  const targetIsStable = target.pre.length === 0;

  const candidates = [];
  for (const rawTag of tags) {
    const tag = String(rawTag || "").trim();
    if (!tag) continue;
    const parsed = parseSemver(tag);
    if (!parsed) continue;
    if (compareSemver(parsed, target) >= 0) continue;
    if (targetIsStable && parsed.pre.length > 0) continue;
    candidates.push({ tag, parsed });
  }
  if (candidates.length === 0) return null;
  candidates.sort((a, b) => compareSemver(b.parsed, a.parsed));
  return candidates[0].tag;
}

export function parseCommitSubject(subject) {
  const raw = String(subject || "").trim();
  if (!raw) return null;

  const match = CONVENTIONAL_RE.exec(raw);
  if (match) {
    const type = match[1].toLowerCase();
    const scope = match[2] ? match[2].trim() : "";
    const breaking = Boolean(match[3]);
    const description = match[4].trim();

    let bucket = "other";
    if (type === "feat") {
      bucket = "feat";
    } else if (type === "fix") {
      bucket = "fix";
    } else if (["perf", "refactor", "style"].includes(type)) {
      bucket = "perf";
    } else if (["chore", "ci", "build", "docs", "doc", "test", "revert"].includes(type)) {
      bucket = "chore";
    }
    return { bucket, type, scope, breaking, description };
  }

  for (const [prefix, bucket] of [
    ["新增：", "feat"],
    ["新增:", "feat"],
    ["新增 ", "feat"],
    ["修复：", "fix"],
    ["修复:", "fix"],
    ["修复 ", "fix"],
    ["优化：", "perf"],
    ["优化:", "perf"],
    ["优化 ", "perf"],
  ]) {
    if (raw.startsWith(prefix)) {
      return {
        bucket,
        type: bucket,
        scope: "",
        breaking: false,
        description: raw.slice(prefix.length).trim(),
      };
    }
  }

  return {
    bucket: "other",
    type: "other",
    scope: "",
    breaking: false,
    description: raw,
  };
}

function formatCommitBullet(commit, repo) {
  const parsed = parseCommitSubject(commit.subject);
  if (!parsed || !parsed.description) return null;

  let line = parsed.description;
  if (parsed.scope) {
    line = `**${parsed.scope}**：${line}`;
  }
  if (parsed.breaking) {
    line = `**[破坏性变更]** ${line}`;
  }

  const shortHash = String(commit.shortHash || "").trim() || String(commit.hash || "").slice(0, 7);
  const fullHash = String(commit.hash || shortHash).trim();
  if (shortHash && repo) {
    line += ` ([${shortHash}](https://github.com/${repo}/commit/${fullHash}))`;
  } else if (shortHash) {
    line += ` (${shortHash})`;
  }
  return { bucket: parsed.bucket, line: `- ${line}` };
}

export function buildReleaseNotes({
  version,
  signMacos = false,
  repo = "Joftal/HIVECAST",
  prevTag = null,
  commits = [],
  totalCommits = commits.length,
}) {
  const cleanVersion = String(version || "").trim().replace(/^v/, "");
  const groups = new Map(SECTIONS.map((s) => [s.key, []]));

  for (const commit of commits) {
    const formatted = formatCommitBullet(commit, repo);
    if (!formatted) continue;
    groups.get(formatted.bucket).push(formatted.line);
  }

  const lines = ["## 更新内容", ""];
  let hasAnyCommit = false;
  for (const section of SECTIONS) {
    const items = groups.get(section.key);
    if (!items || items.length === 0) continue;
    hasAnyCommit = true;
    lines.push(section.heading, ...items, "");
  }

  if (!hasAnyCommit) {
    lines.push("- 本次发布无新增代码提交（重新构建或发布流程更新）。", "");
  }

  if (prevTag) {
    const compareUrl = `https://github.com/${repo}/compare/${prevTag}...v${cleanVersion}`;
    lines.push(`**完整提交对比**：[${prevTag}...v${cleanVersion}](${compareUrl})`, "");
  } else if (totalCommits > commits.length) {
    lines.push(`> 共 ${totalCommits} 条提交，以上展示最近 ${commits.length} 条。`, "");
  }

  const macosSignNote = signMacos
    ? "已使用 Apple Developer ID 签名并通过公证"
    : "未公证版本首次被拦截可在「系统设置 → 隐私与安全性」点击「仍要打开」";

  lines.push(
    "---",
    "",
    "## 安装说明",
    "",
    `- **macOS（Universal 通用版，支持 M 芯片与 Intel）**：打开 \`HIVECAST-v${cleanVersion}-macOS-universal.dmg\` 拖入「应用程序」（${macosSignNote}）。`,
    `- **Windows（x64）**：运行 \`HIVECAST-v${cleanVersion}-Windows-x64-setup.exe\` 安装或覆盖升级（新安装优先使用安装目录，升级沿用已有档案；卸载保留录像，可选择清理此安装的账号设置），或解压 \`HIVECAST-v${cleanVersion}-Windows-x64-portable.zip\` 运行。`,
    "- **使用提示**：录制需本机安装 FFmpeg；文件校验见 `SHA256SUMS.txt`；应用内「设置 → 关于」支持自动检测新版本。",
    "",
  );

  return lines.join("\n");
}

function readReachableTags(toRef) {
  try {
    const output = execFileSync("git", ["tag", "--merged", toRef, "--list", "v*"], {
      encoding: "utf8",
    });
    return output
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean);
  } catch {
    return [];
  }
}

function readGitCommits(fromRef, toRef, maxCommits) {
  const range = fromRef ? `${fromRef}..${toRef}` : toRef;
  const output = execFileSync(
    "git",
    ["log", range, "--no-merges", "--format=%H%x1f%h%x1f%s"],
    { encoding: "utf8", maxBuffer: 10 * 1024 * 1024 },
  );
  const all = output
    .split(/\r?\n/)
    .filter(Boolean)
    .map((line) => {
      const [hash = "", shortHash = "", ...rest] = line.split("\x1f");
      return { hash, shortHash, subject: rest.join("\x1f") };
    })
    .filter((c) => c.subject.trim().length > 0);

  if (!fromRef && maxCommits > 0 && all.length > maxCommits) {
    return { commits: all.slice(0, maxCommits), totalCommits: all.length };
  }
  return { commits: all, totalCommits: all.length };
}

function parseArgs(argv) {
  const opts = {
    version: "",
    out: null,
    signMacos: false,
    repo: process.env.GITHUB_REPOSITORY || "Joftal/HIVECAST",
    from: null,
    to: "HEAD",
    maxCommits: 80,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = argv[i + 1];
    if (arg === "--version" && next) {
      opts.version = next;
      i += 1;
    } else if (arg === "--out" && next) {
      opts.out = next;
      i += 1;
    } else if (arg === "--sign-macos" && next) {
      opts.signMacos = next === "true";
      i += 1;
    } else if (arg === "--repo" && next) {
      opts.repo = next;
      i += 1;
    } else if (arg === "--from" && next) {
      opts.from = next;
      i += 1;
    } else if (arg === "--to" && next) {
      opts.to = next;
      i += 1;
    } else if (arg === "--max-commits" && next) {
      opts.maxCommits = Number.parseInt(next, 10) || 80;
      i += 1;
    }
  }
  return opts;
}

function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (!opts.version || !parseSemver(opts.version)) {
    console.error("Usage: generate-release-notes.mjs --version <semver> [--out <file>]");
    process.exit(1);
  }
  const prevTag = opts.from || selectPreviousTag(readReachableTags(opts.to), opts.version);
  const { commits, totalCommits } = readGitCommits(prevTag, opts.to, opts.maxCommits);
  const markdown = buildReleaseNotes({
    version: opts.version,
    signMacos: opts.signMacos,
    repo: opts.repo,
    prevTag,
    commits,
    totalCommits,
  });
  if (opts.out) {
    writeFileSync(resolve(opts.out), markdown, "utf8");
  } else {
    process.stdout.write(markdown);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main();
}

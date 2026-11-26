#!/usr/bin/env node
/**
 * Generate the static HIVECAST changelog website (`index.html`),
 * structured feed (`changelog.json` — consumed by desktop app via jsDelivr / GitHub Pages),
 * and minimal version descriptor (`latest.json`).
 *
 * Usage:
 *   node scripts/ci/generate-changelog-site.mjs --out dist-site
 *   node scripts/ci/generate-changelog-site.mjs --input fixtures.json --out dist-site
 */

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const REPO = process.env.GITHUB_REPOSITORY || "Joftal/HIVECAST";
const API_URL = `https://api.github.com/repos/${REPO}/releases?per_page=30`;
const REPO_URL = `https://github.com/${REPO}`;
const RELEASES_URL = `${REPO_URL}/releases`;
const ISSUES_URL = `${REPO_URL}/issues`;

function parseArgs(argv) {
  let outDir = "dist-site";
  let inputFile = null;
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--out" && argv[i + 1]) {
      outDir = argv[i + 1];
      i += 1;
    } else if (argv[i] === "--input" && argv[i + 1]) {
      inputFile = argv[i + 1];
      i += 1;
    }
  }
  return { outDir: resolve(outDir), inputFile: inputFile ? resolve(inputFile) : null };
}

function stripMarkdownLinks(input) {
  return input
    .replace(/\[([^\]]+)\]\(([^)]+)\)/g, "$1")
    .replace(/\*\*/g, "")
    .trim();
}

function classifyNote(text) {
  const lower = text.toLowerCase();
  const rules = [
    ["feat:", "feat"],
    ["feat(", "feat"],
    ["新增：", "feat"],
    ["新增:", "feat"],
    ["新增 ", "feat"],
    ["fix:", "fix"],
    ["fix(", "fix"],
    ["修复：", "fix"],
    ["修复:", "fix"],
    ["修复 ", "fix"],
    ["perf:", "perf"],
    ["优化：", "perf"],
    ["优化:", "perf"],
    ["优化 ", "perf"],
  ];
  for (const [prefix, kind] of rules) {
    if (lower.startsWith(prefix)) {
      let body = text;
      if (prefix.endsWith("(")) {
        const idx = text.indexOf("):");
        body = idx >= 0 ? text.slice(idx + 2).trim() : text;
      } else {
        body = text.slice(prefix.length).trim();
      }
      return { kind, text: body };
    }
  }
  return { kind: "other", text };
}

function parseReleaseNotes(body) {
  if (!body || typeof body !== "string") return [];
  const notes = [];
  let inInstallGuide = false;
  for (const rawLine of body.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line) continue;
    if (line.startsWith("#")) {
      const lower = line.toLowerCase();
      inInstallGuide =
        lower.includes("安装说明") ||
        lower.includes("macos") ||
        lower.includes("windows") ||
        lower.includes("录制与更新");
      continue;
    }
    if (inInstallGuide) continue;
    let bullet = null;
    if (line.startsWith("- ")) bullet = line.slice(2).trim();
    else if (line.startsWith("* ")) bullet = line.slice(2).trim();
    if (!bullet || bullet.startsWith("Full Changelog:")) continue;
    const cleaned = stripMarkdownLinks(bullet);
    const note = classifyNote(cleaned);
    if (note.text) notes.push(note);
    if (notes.length >= 12) break;
  }
  return notes;
}

function normalizeReleases(rawReleases) {
  const list = Array.isArray(rawReleases) ? rawReleases : [];
  return list
    .filter((r) => r && !r.draft && typeof r.tag_name === "string")
    .map((r) => {
      const version = r.tag_name.trim().replace(/^v/, "");
      const publishedAt =
        typeof r.published_at === "string" ? r.published_at.slice(0, 10) : "";
      const assets = Array.isArray(r.assets)
        ? r.assets.map((a) => ({
            name: String(a.name || ""),
            size: Number(a.size || 0),
            downloadUrl: String(a.browser_download_url || ""),
          }))
        : [];
      return {
        version,
        name: String(r.name || `HIVECAST v${version}`),
        publishedAt,
        url: String(r.html_url || RELEASES_URL),
        prerelease: Boolean(r.prerelease),
        notes: parseReleaseNotes(r.body || ""),
        assets,
      };
    });
}

function fallbackReleases() {
  const pkg = JSON.parse(readFileSync(resolve("package.json"), "utf8"));
  const ver = pkg.version || "0.1.0";
  return [
    {
      version: ver,
      name: `HIVECAST v${ver}`,
      publishedAt: new Date().toISOString().slice(0, 10),
      url: RELEASES_URL,
      prerelease: ver.includes("-"),
      notes: [
        {
          kind: "feat",
          text: "支持 Bilibili、斗鱼、虎牙、抖音四平台直播监控、自动录制、弹幕同步与本地画廊回放。",
        },
        {
          kind: "feat",
          text: "双端液态玻璃极简安装器（macOS Retina DMG 与 Windows 独立现代安装/卸载器）。",
        },
        {
          kind: "fix",
          text: "FLV 合并完整性包数核验与实测时长校准，差值超过 3 秒时提示核对原片。",
        },
      ],
      assets: [],
    },
  ];
}

async function fetchReleases(inputFile) {
  if (inputFile) {
    return normalizeReleases(JSON.parse(readFileSync(inputFile, "utf8")));
  }
  const headers = {
    Accept: "application/vnd.github+json",
    "User-Agent": "HIVECAST-Changelog-Generator",
  };
  if (process.env.GITHUB_TOKEN) {
    headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  }
  const resp = await fetch(API_URL, { headers });
  if (!resp.ok) {
    throw new Error(`GitHub Releases API returned HTTP ${resp.status}`);
  }
  const data = await resp.json();
  return normalizeReleases(data);
}

function escapeHtml(str) {
  return String(str)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes <= 0) return "";
  const mb = bytes / (1024 * 1024);
  return mb >= 1 ? `${mb.toFixed(1)} MB` : `${Math.round(bytes / 1024)} KB`;
}

function renderHtml(feed) {
  const embeddedJson = JSON.stringify(feed).replace(/</g, "\\u003c");
  const firstVer = feed.latestStable || feed.latestAll || "0.1.0";
  return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>HIVECAST Changelog · 更新日志</title>
<meta name="description" content="HIVECAST 多平台直播录制与弹幕同步桌面端更新日志与版本发布记录" />
<style>
  :root {
    --bg: #f6f7f9;
    --surface: #ffffff;
    --raised: #f1f3f6;
    --line: #e5e8ed;
    --line-strong: #d2d7df;
    --ink: #14171c;
    --ink-soft: #39404b;
    --muted: #57616e;
    --faint: #7b8593;
    --accent: #2f6bff;
    --accent-ink: #ffffff;
    --accent-soft: #eef3ff;
    --accent-line: #c9d8ff;
    --accent-text: #2456d6;
    --ok: #0e9f5f;
    --ok-soft: rgba(14, 159, 95, 0.11);
    --warn: #c07a06;
    --warn-soft: rgba(192, 122, 6, 0.12);
    --shadow: 0 1px 2px rgba(16, 24, 40, 0.04), 0 12px 30px -22px rgba(16, 24, 40, 0.22);
  }
  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) {
      --bg: #131211;
      --surface: #1a1817;
      --raised: #232019;
      --line: #2b2722;
      --line-strong: #3c362e;
      --ink: #f2ede6;
      --ink-soft: #d8cfc3;
      --muted: #b3a695;
      --faint: #8b8073;
      --accent: #f0a23c;
      --accent-ink: #221603;
      --accent-soft: rgba(240, 162, 60, 0.14);
      --accent-line: rgba(240, 162, 60, 0.30);
      --accent-text: #f6b862;
      --ok: #3dd68c;
      --ok-soft: rgba(61, 214, 140, 0.14);
      --warn: #ffb43a;
      --warn-soft: rgba(255, 180, 58, 0.15);
      --shadow: 0 1px 0 rgba(255, 255, 255, 0.03) inset;
    }
  }
  [data-theme="dark"] {
    --bg: #131211;
    --surface: #1a1817;
    --raised: #232019;
    --line: #2b2722;
    --line-strong: #3c362e;
    --ink: #f2ede6;
    --ink-soft: #d8cfc3;
    --muted: #b3a695;
    --faint: #8b8073;
    --accent: #f0a23c;
    --accent-ink: #221603;
    --accent-soft: rgba(240, 162, 60, 0.14);
    --accent-line: rgba(240, 162, 60, 0.30);
    --accent-text: #f6b862;
    --ok: #3dd68c;
    --ok-soft: rgba(61, 214, 140, 0.14);
    --warn: #ffb43a;
    --warn-soft: rgba(255, 180, 58, 0.15);
    --shadow: 0 1px 0 rgba(255, 255, 255, 0.03) inset;
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body {
    font-family: "Inter", -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", "Microsoft YaHei", sans-serif;
    background: var(--bg);
    color: var(--ink);
    min-height: 100vh;
    padding: 36px 20px 64px;
    -webkit-font-smoothing: antialiased;
  }
  .wrap { max-width: 780px; margin: 0 auto; }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
    margin-bottom: 28px;
  }
  .brand { display: flex; align-items: center; gap: 12px; }
  .logo-badge {
    width: 44px; height: 44px; border-radius: 12px;
    background: linear-gradient(135deg, var(--accent), #60a5fa);
    color: #fff; font-weight: 800; font-size: 18px;
    display: flex; align-items: center; justify-content: center;
    box-shadow: 0 8px 20px -8px rgba(47, 107, 255, 0.45);
  }
  h1 { font-size: 21px; font-weight: 750; letter-spacing: -0.02em; }
  .sub { font-size: 12.5px; color: var(--muted); margin-top: 2px; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
  .btn {
    height: 32px; padding: 0 12px; border-radius: 8px;
    border: 1px solid var(--line-strong); background: var(--surface);
    color: var(--ink); font-size: 12.5px; font-weight: 600;
    cursor: pointer; text-decoration: none;
    display: inline-flex; align-items: center; gap: 6px;
  }
  .btn:hover { background: var(--raised); }
  .btn.primary { border-color: transparent; background: var(--accent); color: var(--accent-ink); }
  .timeline {
    position: relative; padding-left: 24px;
    display: flex; flex-direction: column; gap: 18px;
  }
  .timeline::before {
    content: ""; position: absolute; left: 6px; top: 10px; bottom: 10px;
    width: 2px; background: var(--line-strong);
  }
  .card {
    position: relative; border-radius: 14px;
    border: 1px solid var(--line); background: var(--surface);
    padding: 20px 22px; box-shadow: var(--shadow);
  }
  .card::before {
    content: ""; position: absolute; left: -23px; top: 24px;
    width: 12px; height: 12px; border-radius: 50%;
    background: var(--line-strong); border: 3px solid var(--bg);
  }
  .card.latest::before { background: var(--accent); }
  .card-head {
    display: flex; align-items: center; justify-content: space-between;
    gap: 10px; flex-wrap: wrap; margin-bottom: 12px;
  }
  .ver {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 17px; font-weight: 750; display: inline-flex; align-items: center; gap: 8px;
  }
  .pill {
    font-family: inherit; font-size: 10.5px; font-weight: 650;
    padding: 2px 8px; border-radius: 999px;
    background: var(--accent-soft); color: var(--accent-text);
    border: 1px solid var(--accent-line);
  }
  .pill.pre { background: var(--warn-soft); color: var(--warn); border-color: transparent; }
  .date { font-family: ui-monospace, monospace; font-size: 12px; color: var(--faint); }
  ul.notes { list-style: none; display: flex; flex-direction: column; gap: 7px; font-size: 13px; line-height: 1.6; color: var(--ink-soft); }
  ul.notes li { display: flex; align-items: baseline; gap: 8px; }
  .tag {
    font-size: 10.5px; font-weight: 700; padding: 1px 6px; border-radius: 4px; flex-shrink: 0;
  }
  .tag.feat { background: var(--accent-soft); color: var(--accent-text); }
  .tag.fix { background: var(--ok-soft); color: var(--ok); }
  .tag.perf { background: var(--warn-soft); color: var(--warn); }
  .tag.other { background: var(--raised); color: var(--muted); }
  .assets { margin-top: 14px; padding-top: 12px; border-top: 1px solid var(--line); display: flex; gap: 8px; flex-wrap: wrap; }
  .asset-btn {
    height: 28px; padding: 0 10px; border-radius: 7px;
    border: 1px solid var(--line-strong); background: var(--bg);
    color: var(--ink); font-size: 11.5px; font-weight: 550;
    text-decoration: none; display: inline-flex; align-items: center; gap: 6px;
  }
  .asset-btn:hover { border-color: var(--accent-line); color: var(--accent-text); }
  .asset-size { color: var(--faint); font-family: ui-monospace, monospace; font-size: 10.5px; }
  footer {
    margin-top: 32px; padding-top: 16px; border-top: 1px solid var(--line);
    display: flex; justify-content: space-between; flex-wrap: wrap; gap: 10px;
    font-size: 12px; color: var(--faint);
  }
  footer a { color: var(--accent-text); text-decoration: none; }
</style>
</head>
<body>
<div class="wrap">
  <header>
    <div class="brand">
      <div class="logo-badge">H</div>
      <div>
        <h1>HIVECAST Changelog</h1>
        <div class="sub">多平台直播录制与弹幕同步 · 当前最新 v${escapeHtml(firstVer)}</div>
      </div>
    </div>
    <div class="actions">
      <button class="btn" id="themeBtn" type="button">切换主题</button>
      <a class="btn" href="${escapeHtml(ISSUES_URL)}" target="_blank" rel="noopener">Bug 反馈 ↗</a>
      <a class="btn primary" href="${escapeHtml(RELEASES_URL)}" target="_blank" rel="noopener">GitHub Releases ↗</a>
    </div>
  </header>

  <main class="timeline" id="timeline"></main>

  <footer>
    <span>由 GitHub Actions 自动同步 Release · 支持全球 CDN 更新检测</span>
    <span><a href="./changelog.json">changelog.json</a> · <a href="./latest.json">latest.json</a> · <a href="${escapeHtml(REPO_URL)}">GitHub</a></span>
  </footer>
</div>

<script>
const FEED = ${embeddedJson};
const TAG_LABEL = { feat: "新增", fix: "修复", perf: "优化", other: "变更" };

function esc(s) {
  return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;");
}
function fmtBytes(b) {
  if (!b || b <= 0) return "";
  const mb = b / (1024 * 1024);
  return mb >= 1 ? mb.toFixed(1) + " MB" : Math.round(b / 1024) + " KB";
}

const root = document.getElementById("timeline");
root.innerHTML = FEED.releases.map((r, idx) => {
  const isLatest = idx === 0;
  const pill = r.prerelease
    ? '<span class="pill pre">Pre-release</span>'
    : (isLatest ? '<span class="pill">Latest</span>' : '');
  const notesHtml = r.notes && r.notes.length
    ? '<ul class="notes">' + r.notes.map((n) =>
        '<li><span class="tag ' + esc(n.kind || "other") + '">' + esc(TAG_LABEL[n.kind] || "变更") + '</span><span>' + esc(n.text) + '</span></li>'
      ).join("") + '</ul>'
    : '<p style="font-size:12.5px;color:var(--faint);">完整发行说明请查看 GitHub Release 页面。</p>';
  const assetsHtml = r.assets && r.assets.length
    ? '<div class="assets">' + r.assets.map((a) =>
        '<a class="asset-btn" href="' + esc(a.downloadUrl) + '" target="_blank" rel="noopener">' +
        '<span>' + esc(a.name) + '</span>' +
        (a.size ? '<span class="asset-size">' + fmtBytes(a.size) + '</span>' : '') +
        '</a>'
      ).join("") + '</div>'
    : '';
  return '<article class="card ' + (isLatest ? 'latest' : '') + '">' +
    '<div class="card-head">' +
      '<a class="ver" style="color:inherit;text-decoration:none;" href="' + esc(r.url) + '" target="_blank" rel="noopener">v' + esc(r.version) + ' ' + pill + '</a>' +
      '<span class="date">' + esc(r.publishedAt || "") + '</span>' +
    '</div>' +
    notesHtml +
    assetsHtml +
  '</article>';
}).join("");

document.getElementById("themeBtn").onclick = () => {
  const cur = document.documentElement.getAttribute("data-theme");
  const isDark = cur === "dark" || (!cur && matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.setAttribute("data-theme", isDark ? "light" : "dark");
};
</script>
</body>
</html>
`;
}

async function main() {
  const { outDir, inputFile } = parseArgs(process.argv.slice(2));
  let releases = await fetchReleases(inputFile);
  if (releases.length === 0) {
    releases = fallbackReleases();
  }

  const latestStable = releases.find((r) => !r.prerelease) || null;
  const latestAll = releases[0] || null;
  const generatedAt = new Date().toISOString();

  const changelogFeed = {
    schemaVersion: 1,
    generatedAt,
    repo: REPO,
    latestStable: latestStable ? latestStable.version : null,
    latestAll: latestAll ? latestAll.version : null,
    releases: releases.map((r) => ({
      version: r.version,
      publishedAt: r.publishedAt,
      url: r.url,
      prerelease: r.prerelease,
      notes: r.notes,
      assets: r.assets,
    })),
  };

  const latestFeed = latestStable || latestAll;
  const latestPayload = {
    schemaVersion: 1,
    generatedAt,
    version: latestFeed ? latestFeed.version : "0.1.0",
    publishedAt: latestFeed ? latestFeed.publishedAt : "",
    url: latestFeed ? latestFeed.url : RELEASES_URL,
    prerelease: latestFeed ? latestFeed.prerelease : false,
    notes: latestFeed ? latestFeed.notes : [],
  };

  mkdirSync(outDir, { recursive: true });
  writeFileSync(join(outDir, "changelog.json"), JSON.stringify(changelogFeed, null, 2) + "\n");
  writeFileSync(join(outDir, "latest.json"), JSON.stringify(latestPayload, null, 2) + "\n");
  writeFileSync(join(outDir, "index.html"), renderHtml(changelogFeed));
  writeFileSync(join(outDir, ".nojekyll"), "");

  console.log(
    `changelog site generated in ${outDir}: ${releases.length} release(s), latest=${latestPayload.version}`,
  );
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});

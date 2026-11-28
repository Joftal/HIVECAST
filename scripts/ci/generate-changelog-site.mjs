#!/usr/bin/env node
/**
 * Generate the static HIVECAST changelog website (`index.html`, `icon.png`),
 * structured feed (`changelog.json` — consumed by desktop app via jsDelivr / GitHub Pages),
 * and minimal version descriptor (`latest.json`).
 *
 * Usage:
 *   node scripts/ci/generate-changelog-site.mjs --out dist-site
 *   node scripts/ci/generate-changelog-site.mjs --input fixtures.json --out dist-site
 */

import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const REPO = process.env.GITHUB_REPOSITORY || "Joftal/HIVECAST";
const API_URL = `https://api.github.com/repos/${REPO}/releases?per_page=100`;
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
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\s*\([0-9a-f]{7,40}\)\s*$/i, "")
    .trim();
}

function classifyHeadingKind(title) {
  const lower = title.toLowerCase();
  if (lower.includes("新增") || lower.includes("feat") || lower.includes("feature")) return "feat";
  if (lower.includes("修复") || lower.includes("fix")) return "fix";
  if (lower.includes("优化") || lower.includes("perf") || lower.includes("体验") || lower.includes("改进")) return "perf";
  return "other";
}

function classifyNote(text, fallbackKind = "other") {
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
  return { kind: fallbackKind, text };
}

function parseReleaseNotes(body) {
  if (!body || typeof body !== "string") return [];
  const notes = [];
  let guideDepth = 0;
  let sectionKind = "other";
  for (const rawLine of body.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line) continue;
    if (line.startsWith("#")) {
      const [, marks, title] = /^(#+)\s*(.*)$/.exec(line);
      if (!guideDepth || marks.length <= guideDepth) {
        guideDepth = ["安装说明", "installation", "installation instructions", "录制与更新"].includes(title.toLowerCase()) ? marks.length : 0;
        if (!guideDepth) {
          sectionKind = classifyHeadingKind(title);
        }
      }
      continue;
    }
    if (guideDepth) continue;
    let bullet = null;
    if (line.startsWith("- ")) bullet = line.slice(2).trim();
    else if (line.startsWith("* ")) bullet = line.slice(2).trim();
    if (!bullet || bullet.startsWith("Full Changelog:")) continue;
    const cleaned = stripMarkdownLinks(bullet);
    const note = classifyNote(cleaned, sectionKind);
    if (note.text) notes.push(note);
    if (notes.length >= 80) break;
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

const versionPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][A-Za-z0-9-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][A-Za-z0-9-]*))*))?(?:\+[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*)?$/;

function compareIdentifier(a, b) {
  if (a === b) return 0;
  const an = /^\d+$/.test(a), bn = /^\d+$/.test(b);
  if (an && bn) return BigInt(a) < BigInt(b) ? -1 : 1;
  if (an !== bn) return an ? -1 : 1;
  return a < b ? -1 : 1;
}

function compareVersion(a, b) {
  const x = versionPattern.exec(a.version), y = versionPattern.exec(b.version);
  for (let i = 1; i <= 3; i++) {
    const order = compareIdentifier(x[i], y[i]);
    if (order) return order;
  }
  if (!x[4] || !y[4]) return Number(!x[4]) - Number(!y[4]);
  const left = x[4].split("."), right = y[4].split(".");
  for (let i = 0; i < Math.min(left.length, right.length); i++) {
    const order = compareIdentifier(left[i], right[i]);
    if (order) return order;
  }
  return left.length - right.length;
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
  const releases = [];
  for (let page = 1; ; page++) {
    const resp = await fetch(API_URL + "&page=" + page, { headers });
    if (!resp.ok) throw new Error("GitHub Releases API returned HTTP " + resp.status);
    releases.push(...normalizeReleases(await resp.json()));
    if (!resp.headers.get("link")?.includes('rel="next"')) return releases;
  }
}

function escapeHtml(str) {
  return String(str)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function renderHtml(feed) {
  const embeddedJson = JSON.stringify(feed).replace(/</g, "\\u003c");
  const latestVer = feed.latestStable || feed.latestAll;
  return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>HIVECAST Changelog · 更新日志</title>
<meta name="description" content="HIVECAST 多平台直播录制与弹幕同步桌面端更新日志与版本发布记录" />
<link rel="icon" type="image/png" href="./icon.png" />
<style>
  :root {
    --c-canvas: #f4f6fa;
    --c-surface: rgba(255, 255, 255, 0.86);
    --c-surface-solid: #ffffff;
    --c-raised: #eef1f6;
    --c-line: #e4e8ef;
    --c-line-strong: #cfd6e1;
    --c-ink: #14171c;
    --c-ink-soft: #39404b;
    --c-muted: #57616e;
    --c-faint: #7b8593;
    --c-accent: #2f6bff;
    --c-accent-ink: #ffffff;
    --c-accent-soft: #eef3ff;
    --c-accent-line: #c9d8ff;
    --c-accent-text: #2456d6;
    --c-ok: #0e9f5f;
    --c-ok-soft: rgba(14, 159, 95, 0.11);
    --c-warn: #c07a06;
    --c-warn-soft: rgba(192, 122, 6, 0.12);
    --c-rec: #e5484d;
    --hero-bg: linear-gradient(135deg, #f3f7ff 0%, #faf6ff 55%, #fff5f8 100%);
    --blob-1: rgba(47, 107, 255, 0.14);
    --blob-2: rgba(244, 130, 168, 0.14);
    --blob-3: rgba(96, 165, 250, 0.10);
    --shadow-card: 0 1px 2px rgba(16, 24, 40, 0.04), 0 12px 32px -22px rgba(16, 24, 40, 0.22);
    --shadow-hero: 0 2px 4px rgba(16, 24, 40, 0.04), 0 20px 50px -28px rgba(47, 107, 255, 0.22);
  }

  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) {
      --c-canvas: #131211;
      --c-surface: rgba(26, 24, 23, 0.86);
      --c-surface-solid: #1a1817;
      --c-raised: #232019;
      --c-line: #2b2722;
      --c-line-strong: #3c362e;
      --c-ink: #f2ede6;
      --c-ink-soft: #d8cfc3;
      --c-muted: #b3a695;
      --c-faint: #8b8073;
      --c-accent: #f0a23c;
      --c-accent-ink: #221603;
      --c-accent-soft: rgba(240, 162, 60, 0.14);
      --c-accent-line: rgba(240, 162, 60, 0.30);
      --c-accent-text: #f6b862;
      --c-ok: #3dd68c;
      --c-ok-soft: rgba(61, 214, 140, 0.13);
      --c-warn: #ffb43a;
      --c-warn-soft: rgba(255, 180, 58, 0.15);
      --c-rec: #ff6369;
      --hero-bg: linear-gradient(135deg, #211d18 0%, #1a1817 55%, #23191a 100%);
      --blob-1: rgba(240, 162, 60, 0.16);
      --blob-2: rgba(255, 99, 105, 0.11);
      --blob-3: rgba(240, 162, 60, 0.08);
      --shadow-card: 0 1px 0 rgba(255, 255, 255, 0.03) inset;
      --shadow-hero: 0 1px 0 rgba(255, 255, 255, 0.04) inset, 0 24px 60px -28px rgba(0, 0, 0, 0.72);
    }
  }

  [data-theme="dark"] {
    --c-canvas: #131211;
    --c-surface: rgba(26, 24, 23, 0.86);
    --c-surface-solid: #1a1817;
    --c-raised: #232019;
    --c-line: #2b2722;
    --c-line-strong: #3c362e;
    --c-ink: #f2ede6;
    --c-ink-soft: #d8cfc3;
    --c-muted: #b3a695;
    --c-faint: #8b8073;
    --c-accent: #f0a23c;
    --c-accent-ink: #221603;
    --c-accent-soft: rgba(240, 162, 60, 0.14);
    --c-accent-line: rgba(240, 162, 60, 0.30);
    --c-accent-text: #f6b862;
    --c-ok: #3dd68c;
    --c-ok-soft: rgba(61, 214, 140, 0.13);
    --c-warn: #ffb43a;
    --c-warn-soft: rgba(255, 180, 58, 0.15);
    --c-rec: #ff6369;
    --hero-bg: linear-gradient(135deg, #211d18 0%, #1a1817 55%, #23191a 100%);
    --blob-1: rgba(240, 162, 60, 0.16);
    --blob-2: rgba(255, 99, 105, 0.11);
    --blob-3: rgba(240, 162, 60, 0.08);
    --shadow-card: 0 1px 0 rgba(255, 255, 255, 0.03) inset;
    --shadow-hero: 0 1px 0 rgba(255, 255, 255, 0.04) inset, 0 24px 60px -28px rgba(0, 0, 0, 0.72);
  }

  * { box-sizing: border-box; margin: 0; padding: 0; }
  html { scroll-behavior: smooth; }
  body {
    font-family: "Inter", -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", "Microsoft YaHei", sans-serif;
    background-color: var(--c-canvas);
    background-image:
      radial-gradient(520px 360px at 14% -4%, var(--blob-1), transparent 72%),
      radial-gradient(460px 340px at 88% 4%, var(--blob-2), transparent 72%);
    background-attachment: fixed;
    color: var(--c-ink);
    min-height: 100vh;
    -webkit-font-smoothing: antialiased;
    transition: background-color 200ms ease, color 200ms ease;
  }

  /* Sticky Top Navigation (aligned with HIVECAST app topbar) */
  .topbar {
    position: sticky;
    top: 0;
    z-index: 40;
    backdrop-filter: blur(18px);
    -webkit-backdrop-filter: blur(18px);
    background: var(--c-surface);
    border-bottom: 1px solid var(--c-line);
  }
  .topbar-inner {
    max-width: 1040px;
    margin: 0 auto;
    height: 56px;
    padding: 0 24px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .nav-brand {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    text-decoration: none;
    color: var(--c-ink);
  }
  .nav-brand img {
    width: 28px;
    height: 28px;
    border-radius: 7px;
    box-shadow: 0 3px 10px -3px rgba(47, 107, 255, 0.35);
  }
  .nav-brand-name {
    font-size: 15px;
    font-weight: 750;
    letter-spacing: -0.02em;
  }
  .nav-pill {
    font-size: 11px;
    font-weight: 600;
    color: var(--c-accent-text);
    background: var(--c-accent-soft);
    border: 1px solid var(--c-accent-line);
    border-radius: 999px;
    padding: 2px 8px;
  }
  .nav-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .btn {
    height: 32px;
    padding: 0 11px;
    border-radius: 8px;
    border: 1px solid var(--c-line-strong);
    background: var(--c-surface-solid);
    color: var(--c-ink);
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
    text-decoration: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    transition: background 140ms ease, border-color 140ms ease, transform 120ms ease;
    white-space: nowrap;
  }
  .btn:hover { background: var(--c-raised); }
  .btn.icon-only { padding: 0 9px; }
  .btn.primary {
    border-color: transparent;
    background: var(--c-accent);
    color: var(--c-accent-ink);
  }
  .btn.primary:hover { filter: brightness(1.06); transform: translateY(-1px); }

  /* Main Page Container */
  .page {
    max-width: 1040px;
    margin: 0 auto;
    padding: 28px 24px 64px;
  }

  /* Hero Card (matches HIVECAST About page Hero) */
  .hero {
    position: relative;
    overflow: hidden;
    border-radius: 16px;
    border: 1px solid var(--c-line);
    background: var(--hero-bg);
    box-shadow: var(--shadow-hero);
  }
  .hero::before {
    content: "";
    position: absolute;
    inset: 0;
    pointer-events: none;
    background:
      radial-gradient(320px 240px at 90% 0%, var(--blob-1), transparent 72%),
      radial-gradient(280px 220px at 22% 100%, var(--blob-2), transparent 74%);
  }
  .hero-main {
    position: relative;
    z-index: 1;
    padding: 28px 30px 24px;
    display: grid;
    grid-template-columns: 1.25fr 0.95fr;
    gap: 24px;
    align-items: center;
  }
  .brand-row {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }
  .hero-icon {
    width: 48px;
    height: 48px;
    border-radius: 12px;
    box-shadow: 0 6px 16px -6px rgba(47, 107, 255, 0.35);
    display: block;
    flex-shrink: 0;
  }
  .hero-title-group h1 {
    font-size: 23px;
    font-weight: 800;
    letter-spacing: -0.025em;
    line-height: 1.2;
  }
  .badge-row {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 5px;
    flex-wrap: wrap;
  }
  .badge {
    font-size: 11px;
    font-weight: 600;
    padding: 2px 9px;
    border-radius: 999px;
    background: var(--c-accent-soft);
    color: var(--c-accent-text);
    border: 1px solid var(--c-accent-line);
  }
  .badge.ok {
    background: var(--c-ok-soft);
    color: var(--c-ok);
    border-color: transparent;
  }
  .hero-slogan {
    margin-top: 16px;
    font-size: 24px;
    font-weight: 780;
    letter-spacing: -0.025em;
    line-height: 1.3;
  }
  .hero-slogan em {
    font-style: normal;
    color: var(--c-accent);
  }
  .hero-desc {
    margin-top: 8px;
    font-size: 13px;
    line-height: 1.65;
    color: var(--c-muted);
    max-width: 480px;
  }

  /* Latest Release Spotlight Card on Hero Right */
  .spotlight {
    border-radius: 14px;
    border: 1px solid var(--c-line);
    background: var(--c-surface);
    backdrop-filter: blur(16px);
    padding: 16px 18px;
    box-shadow: var(--shadow-card);
  }
  .spot-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 10px;
  }
  .spot-label {
    font-size: 11px;
    font-weight: 650;
    color: var(--c-faint);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .spot-ver {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 22px;
    font-weight: 800;
    letter-spacing: -0.02em;
    color: var(--c-ink);
  }
  .spot-date {
    font-family: ui-monospace, monospace;
    font-size: 11.5px;
    color: var(--c-faint);
  }
  .spot-downloads {
    margin-top: 12px;
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .dl-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 8px 11px;
    border-radius: 9px;
    border: 1px solid var(--c-line);
    background: var(--c-surface-solid);
    text-decoration: none;
    color: var(--c-ink);
    font-size: 12px;
    font-weight: 600;
    transition: border-color 140ms ease, transform 120ms ease, background 140ms ease;
  }
  .dl-row:hover {
    border-color: var(--c-accent-line);
    background: var(--c-accent-soft);
    color: var(--c-accent-text);
    transform: translateY(-1px);
  }
  .dl-row-left {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .dl-meta {
    font-family: ui-monospace, monospace;
    font-size: 11px;
    color: var(--c-faint);
    flex-shrink: 0;
  }

  /* Hero 6-Capability Strip (1:1 aligned with HIVECAST AboutSection) */
  .cap-strip {
    position: relative;
    z-index: 1;
    display: grid;
    grid-template-columns: repeat(6, 1fr);
    background: var(--c-surface-solid);
    border-top: 1px solid var(--c-line);
  }
  .cap-item {
    padding: 13px 8px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    font-size: 11.5px;
    font-weight: 550;
    color: var(--c-ink-soft);
    position: relative;
  }
  .cap-item:not(:last-child)::after {
    content: "";
    position: absolute;
    right: 0;
    top: 22%;
    height: 56%;
    width: 1px;
    background: var(--c-line);
  }

  /* Filter & Search Bar */
  .toolbar {
    margin-top: 22px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
  }
  .seg {
    display: inline-flex;
    padding: 3px;
    border-radius: 9px;
    background: var(--c-surface-solid);
    border: 1px solid var(--c-line);
    gap: 2px;
  }
  .seg button {
    border: none;
    background: transparent;
    color: var(--c-muted);
    font-size: 12px;
    font-weight: 550;
    padding: 5px 12px;
    border-radius: 6px;
    cursor: pointer;
    transition: all 140ms ease;
  }
  .seg button.active {
    background: var(--c-accent-soft);
    color: var(--c-accent-text);
    font-weight: 650;
  }
  .search-wrap {
    position: relative;
    flex: 1;
    max-width: 300px;
    min-width: 200px;
  }
  .search-wrap svg {
    position: absolute;
    left: 11px;
    top: 50%;
    transform: translateY(-50%);
    color: var(--c-faint);
    pointer-events: none;
  }
  .search-input {
    width: 100%;
    height: 34px;
    padding: 0 12px 0 32px;
    border-radius: 9px;
    border: 1px solid var(--c-line);
    background: var(--c-surface-solid);
    color: var(--c-ink);
    font-size: 12.5px;
    outline: none;
    transition: border-color 140ms ease;
  }
  .search-input:focus { border-color: var(--c-accent); }

  /* Two-Column Content: Sticky Version Index + Timeline */
  .content-grid {
    margin-top: 18px;
    display: grid;
    grid-template-columns: 184px 1fr;
    gap: 24px;
    align-items: start;
  }
  .ver-nav {
    position: sticky;
    top: 76px;
    border-radius: 12px;
    border: 1px solid var(--c-line);
    background: var(--c-surface-solid);
    padding: 12px 10px;
    box-shadow: var(--shadow-card);
  }
  .ver-nav-title {
    font-size: 11px;
    font-weight: 650;
    color: var(--c-faint);
    padding: 0 8px 8px;
    letter-spacing: 0.03em;
  }
  .ver-nav-list {
    display: flex;
    flex-direction: column;
    gap: 3px;
    max-height: 60vh;
    overflow-y: auto;
  }
  .ver-nav-link {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 10px 6px 12px;
    border-radius: 7px;
    text-decoration: none;
    color: var(--c-muted);
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 12px;
    font-weight: 600;
    transition: background 140ms ease, color 140ms ease;
  }
  .ver-nav-link::before {
    content: "";
    position: absolute;
    left: 3px;
    top: 7px;
    bottom: 7px;
    width: 3px;
    border-radius: 999px;
    background: var(--c-accent);
    opacity: 0;
    transform: scaleY(0.5);
    transition: opacity 160ms ease, transform 160ms ease;
  }
  .ver-nav-link:hover {
    background: var(--c-raised);
    color: var(--c-ink);
  }
  .ver-nav-link.active {
    background: var(--c-accent-soft);
    color: var(--c-accent-text);
  }
  .ver-nav-link.active::before {
    opacity: 1;
    transform: scaleY(1);
  }
  .ver-nav-link.active .ver-nav-date {
    color: var(--c-accent-text);
    opacity: 0.78;
  }
  .ver-nav-date {
    font-size: 10px;
    font-weight: 500;
    color: var(--c-faint);
  }

  /* Timeline Column */
  .timeline {
    position: relative;
    padding-left: 28px;
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .timeline::before {
    content: "";
    position: absolute;
    left: 9px;
    top: 28px;
    bottom: 28px;
    width: 2px;
    border-radius: 999px;
    background: var(--c-line);
  }
  .release-card {
    position: relative;
    scroll-margin-top: 80px;
    border-radius: 14px;
    border: 1px solid var(--c-line);
    background: var(--c-surface-solid);
    padding: 20px 22px;
    box-shadow: var(--shadow-card);
    transition: border-color 180ms ease, box-shadow 180ms ease;
  }
  .release-card:hover { border-color: var(--c-line-strong); }
  .release-card.is-active {
    border-color: var(--c-accent-line);
  }
  .release-card::before {
    content: "";
    position: absolute;
    /* Pseudo-elements are outside the global * reset; make the 12px ring border-box so it centers on the rail. */
    box-sizing: border-box;
    left: -25px;
    top: 25px;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--c-surface-solid);
    border: 2.5px solid var(--c-line-strong);
    box-shadow: 0 0 0 4px var(--c-canvas);
    transition: background-color 180ms ease, border-color 180ms ease, box-shadow 180ms ease, transform 180ms ease;
  }
  .release-card.latest::before {
    border-color: var(--c-accent);
  }
  .release-card.is-active::before {
    background: var(--c-accent);
    border-color: var(--c-accent);
    box-shadow: 0 0 0 4px var(--c-canvas), 0 0 0 7px var(--c-accent-soft);
    transform: scale(1.08);
  }
  .card-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    flex-wrap: wrap;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--c-line);
    margin-bottom: 14px;
  }
  .card-ver-row {
    display: flex;
    align-items: center;
    gap: 9px;
    flex-wrap: wrap;
  }
  .ver-link {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 18px;
    font-weight: 800;
    letter-spacing: -0.02em;
    color: var(--c-ink);
    text-decoration: none;
  }
  .ver-link:hover { color: var(--c-accent-text); }
  .pill {
    font-size: 10.5px;
    font-weight: 650;
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--c-accent-soft);
    color: var(--c-accent-text);
    border: 1px solid var(--c-accent-line);
  }
  .pill.pre {
    background: var(--c-warn-soft);
    color: var(--c-warn);
    border-color: transparent;
  }
  .card-head-right {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
    color: var(--c-faint);
  }
  .card-gh-link {
    color: var(--c-accent-text);
    text-decoration: none;
    font-weight: 600;
    font-size: 12px;
  }
  .card-gh-link:hover { text-decoration: underline; }

  ul.notes {
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: 13px;
    line-height: 1.65;
    color: var(--c-ink-soft);
  }
  ul.notes li {
    display: flex;
    align-items: baseline;
    gap: 9px;
  }
  .tag {
    font-size: 10.5px;
    font-weight: 700;
    padding: 1px 7px;
    border-radius: 5px;
    flex-shrink: 0;
  }
  .tag.feat { background: var(--c-accent-soft); color: var(--c-accent-text); }
  .tag.fix { background: var(--c-ok-soft); color: var(--c-ok); }
  .tag.perf { background: var(--c-warn-soft); color: var(--c-warn); }
  .tag.other { background: var(--c-raised); color: var(--c-muted); }

  .assets {
    margin-top: 15px;
    padding-top: 13px;
    border-top: 1px solid var(--c-line);
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .asset-chip {
    height: 30px;
    padding: 0 10px;
    border-radius: 8px;
    border: 1px solid var(--c-line);
    background: var(--c-canvas);
    color: var(--c-ink-soft);
    font-size: 11.5px;
    font-weight: 600;
    text-decoration: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    transition: border-color 130ms ease, color 130ms ease, background 130ms ease;
  }
  .asset-chip:hover {
    border-color: var(--c-accent-line);
    background: var(--c-accent-soft);
    color: var(--c-accent-text);
  }
  .asset-size {
    font-family: ui-monospace, monospace;
    font-size: 10.5px;
    color: var(--c-faint);
  }
  .empty-state {
    border-radius: 14px;
    border: 1px dashed var(--c-line-strong);
    padding: 36px 20px;
    text-align: center;
    color: var(--c-muted);
    font-size: 13px;
  }

  /* Footer (matches HIVECAST About page footer) */
  footer {
    margin-top: 40px;
    padding-top: 18px;
    border-top: 1px solid var(--c-line);
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
    font-size: 12px;
    color: var(--c-faint);
    line-height: 1.65;
  }
  .footer-sign {
    color: var(--c-ink-soft);
    font-weight: 550;
  }
  .footer-links {
    display: flex;
    gap: 14px;
    flex-wrap: wrap;
    margin-top: 4px;
  }
  .footer-links a {
    color: var(--c-accent-text);
    text-decoration: none;
    font-weight: 550;
  }
  .footer-links a:hover { text-decoration: underline; }

  @media (max-width: 860px) {
    .hero-main { grid-template-columns: 1fr; padding: 22px 20px; }
    .cap-strip { grid-template-columns: repeat(3, 1fr); }
    .content-grid { grid-template-columns: 1fr; }
    .ver-nav { display: none; }
  }
  @media (max-width: 560px) {
    .page { padding: 18px 14px 48px; }
    .topbar-inner { padding: 0 14px; }
    .nav-pill, .hide-sm { display: none; }
  }
</style>
</head>
<body>
<header class="topbar">
  <div class="topbar-inner">
    <a class="nav-brand" href="./">
      <img src="./icon.png" alt="HIVECAST" />
      <span class="nav-brand-name">HIVECAST</span>
      <span class="nav-pill" data-i18n="navPill">更新日志</span>
    </a>
    <div class="nav-actions">
      <button class="btn icon-only" id="langBtn" type="button">EN</button>
      <button class="btn icon-only" id="themeBtn" type="button" title="Toggle theme" aria-label="Toggle theme"></button>
      <a class="btn hide-sm" href="${escapeHtml(ISSUES_URL)}" target="_blank" rel="noopener">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m8 2 1.88 1.88"/><path d="M14.12 3.88 16 2"/><path d="M9 7.13v-1a3 3 0 1 1 6 0v1"/><path d="M12 20c-3.3 0-6-2.7-6-6v-3a4 4 0 0 1 4-4h4a4 4 0 0 1 4 4v3c0 3.3-2.7 6-6 6"/><path d="M12 20v-9"/><path d="M6.53 9C4.6 8.8 3 7.1 3 5"/><path d="M6 13H2"/><path d="M3 21c0-2.1 1.7-3.9 3.8-4"/><path d="M20.97 5c0 2.1-1.6 3.8-3.5 4"/><path d="M22 13h-4"/><path d="M17.2 17c2.1.1 3.8 1.9 3.8 4"/></svg>
        <span data-i18n="btnIssues">Bug 反馈</span>
      </a>
      <a class="btn primary" href="${escapeHtml(RELEASES_URL)}" target="_blank" rel="noopener">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d="M12 .5C5.65.5.5 5.65.5 12c0 5.08 3.29 9.39 7.86 10.91.57.1.78-.25.78-.55v-1.95c-3.2.7-3.87-1.36-3.87-1.36-.52-1.33-1.28-1.69-1.28-1.69-1.05-.72.08-.7.08-.7 1.16.08 1.77 1.19 1.77 1.19 1.03 1.76 2.7 1.25 3.36.96.1-.75.4-1.25.73-1.54-2.55-.29-5.24-1.28-5.24-5.68 0-1.25.45-2.28 1.18-3.08-.12-.29-.51-1.46.11-3.04 0 0 .97-.31 3.16 1.18a10.9 10.9 0 0 1 5.76 0c2.2-1.49 3.16-1.18 3.16-1.18.63 1.58.23 2.75.11 3.04.74.8 1.18 1.83 1.18 3.08 0 4.41-2.69 5.39-5.25 5.67.41.36.78 1.06.78 2.14v3.17c0 .31.21.66.79.55A11.5 11.5 0 0 0 23.5 12C23.5 5.65 18.35.5 12 .5z"/></svg>
        <span data-i18n="btnReleases">GitHub Releases</span>
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>
      </a>
    </div>
  </div>
</header>

<div class="page">
  <section class="hero">
    <div class="hero-main">
      <div>
        <div class="brand-row">
          <img class="hero-icon" src="./icon.png" alt="HIVECAST" />
          <div class="hero-title-group">
            <h1>HIVECAST</h1>
            <div class="badge-row">
              <span class="badge" data-i18n="ossBadge">自由 · 开源</span>
              ${latestVer ? '<span class="badge ok">v' + escapeHtml(latestVer) + '</span>' : ''}
            </div>
          </div>
        </div>
        <div class="hero-slogan" id="heroSlogan"><em>专注录制</em>，持续进化</div>
        <p class="hero-desc" data-i18n="heroDesc">多平台直播自动监控、无损分段录制、实时弹幕同步与本地画廊回放。此页面由 GitHub Actions 自动同步每次正式发布与更新日志。</p>
      </div>

      <div class="spotlight" id="spotlight"></div>
    </div>

    <div class="cap-strip">
      <div class="cap-item">
        <span style="color:#3b82f6;"><svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9"><rect x="2" y="4" width="20" height="14" rx="2"/><path d="M8 21h8M12 18v3"/></svg></span>
        <span data-i18n="cap1">多平台监控</span>
      </div>
      <div class="cap-item">
        <span style="color:#ef4444;"><svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9"><circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="3.5" fill="currentColor"/></svg></span>
        <span data-i18n="cap2">开播自动录</span>
      </div>
      <div class="cap-item">
        <span style="color:#8b5cf6;"><svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/><path d="M8 9h8M8 13h5"/></svg></span>
        <span data-i18n="cap3">弹幕同步</span>
      </div>
      <div class="cap-item">
        <span style="color:#f59e0b;"><svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9"><path d="m16 3 4 4-4 4"/><path d="M20 7H4"/><path d="m8 21-4-4 4-4"/><path d="M4 17h16"/></svg></span>
        <span data-i18n="cap4">分段无损合</span>
      </div>
      <div class="cap-item">
        <span style="color:#10b981;"><svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9"><polygon points="6 4 20 12 6 20 6 4"/></svg></span>
        <span data-i18n="cap5">本地回放</span>
      </div>
      <div class="cap-item">
        <span style="color:#06b6d4;"><svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg></span>
        <span data-i18n="cap6">隐私本地存</span>
      </div>
    </div>
  </section>

  <div class="toolbar">
    <div class="seg" id="channelSeg">
      <button class="active" data-ch="all" data-i18n="chAll">全部版本</button>
      <button data-ch="stable" data-i18n="chStable">正式版</button>
      <button data-ch="pre" data-i18n="chPre">预发布</button>
    </div>
    <div class="search-wrap">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>
      <input class="search-input" id="searchInput" type="search" placeholder="搜索版本号或更新内容…" />
    </div>
  </div>

  <div class="content-grid">
    <aside class="ver-nav">
      <div class="ver-nav-title" data-i18n="verNavTitle">版本索引</div>
      <nav class="ver-nav-list" id="verNavList"></nav>
    </aside>

    <main class="timeline" id="timeline"></main>
  </div>

  <footer>
    <div>
      <div class="footer-sign" style="display:flex;align-items:center;gap:6px;">
        <svg width="13" height="13" viewBox="0 0 24 24" fill="var(--c-accent)" stroke="var(--c-accent)" stroke-width="2"><path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/></svg>
        <span data-i18n="footerSign">为热爱的直播瞬间打造，也在开源社区持续演进</span>
      </div>
      <div data-i18n="footerSub">开源免费软件 · 所有凭据与录像仅存储于您的本地设备 · 支持全球 CDN 自动更新检测</div>
      <div class="footer-links">
        <a href="${escapeHtml(REPO_URL)}" target="_blank" rel="noopener" data-i18n="linkRepo">源码仓库 ↗</a>
        <a href="./changelog.json">changelog.json</a>
        <a href="./latest.json">latest.json</a>
      </div>
    </div>
    <div style="text-align:right;">
      <div>Copyright © 2026 HIVECAST</div>
      <div style="opacity:0.8;">HIVECAST · Capture Every Live Moment</div>
    </div>
  </footer>
</div>

<script>
const FEED = ${embeddedJson};
const RELEASES_URL = ${JSON.stringify(RELEASES_URL)};

const I18N = {
  zh: {
    navPill: "更新日志",
    btnIssues: "Bug 反馈",
    btnReleases: "GitHub Releases",
    ossBadge: "自由 · 开源",
    sloganHtml: "<em>专注录制</em>，持续进化",
    heroDesc: "多平台直播自动监控、无损分段录制、实时弹幕同步与本地画廊回放。此页面由 GitHub Actions 自动同步每次发布记录。",
    cap1: "多平台监控",
    cap2: "开播自动录",
    cap3: "弹幕同步",
    cap4: "分段无损合",
    cap5: "本地回放",
    cap6: "隐私本地存",
    chAll: "全部版本",
    chStable: "正式版",
    chPre: "预发布",
    searchPh: "搜索版本号或更新内容…",
    verNavTitle: "版本索引",
    spotLabel: "最新可用版本",
    spotEmpty: "等待首个正式 Release 发布后自动展示安装包直链",
    viewOnGh: "Release 详情 ↗",
    noNotes: "完整发行说明请查看 GitHub Release 页面。",
    noReleases: "暂无发布版本。",
    emptySearch: "没有匹配该筛选条件的版本记录。",
    footerSign: "为热爱的直播瞬间打造，也在开源社区持续演进",
    footerSub: "开源免费软件 · 所有凭据与录像仅存储于您的本地设备 · 支持全球 CDN 自动更新检测",
    linkRepo: "源码仓库 ↗",
    tags: { feat: "新增", fix: "修复", perf: "优化", other: "变更" },
    assetMacUniversal: "macOS 通用版 · M 芯片 / Intel (.dmg)",
    assetMacArm: "macOS Apple Silicon (.dmg)",
    assetMacX64: "macOS Intel (.dmg)",
    assetMac: "macOS (.dmg)",
    assetWinSetup: "Windows x64 安装版 (.exe)",
    assetWinZip: "Windows x64 绿色版 (.zip)",
    assetSha: "SHA256 校验清单"
  },
  en: {
    navPill: "Changelog",
    btnIssues: "Report Bug",
    btnReleases: "GitHub Releases",
    ossBadge: "Free · Open Source",
    sloganHtml: "<em>Focused Capture</em>, Always Evolving",
    heroDesc: "Multi-platform live monitoring, lossless segmented recording, synchronized danmaku, and local playback. Automatically synced from GitHub Releases.",
    cap1: "Multi-Platform",
    cap2: "Auto Record",
    cap3: "Danmaku Sync",
    cap4: "Lossless Merge",
    cap5: "Local Player",
    cap6: "Local Privacy",
    chAll: "All Releases",
    chStable: "Stable",
    chPre: "Pre-release",
    searchPh: "Search version or release notes…",
    verNavTitle: "Versions",
    spotLabel: "Latest Release",
    spotEmpty: "Direct installer links will appear here once the first release is published.",
    viewOnGh: "View Release ↗",
    noNotes: "See the GitHub Release page for full details.",
    noReleases: "No releases have been published.",
    emptySearch: "No releases match your filter.",
    footerSign: "Crafted for live moments, growing with the open-source community",
    footerSub: "Free open-source software · All recordings stay local · Global CDN update feed",
    linkRepo: "Source Repo ↗",
    tags: { feat: "New", fix: "Fix", perf: "Perf", other: "Note" },
    assetMacUniversal: "macOS Universal · Apple Silicon / Intel (.dmg)",
    assetMacArm: "macOS Apple Silicon (.dmg)",
    assetMacX64: "macOS Intel (.dmg)",
    assetMac: "macOS (.dmg)",
    assetWinSetup: "Windows x64 Installer (.exe)",
    assetWinZip: "Windows x64 Portable (.zip)",
    assetSha: "SHA256 Checksums"
  }
};

const state = {
  lang: (localStorage.getItem("hivecast-cl-lang") || (navigator.language.startsWith("zh") ? "zh" : "en")) === "en" ? "en" : "zh",
  theme: localStorage.getItem("hivecast-cl-theme") || (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"),
  channel: "all",
  query: "",
  activeVersion: null
};

function t(k) { return I18N[state.lang][k]; }
function esc(s) {
  return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;");
}
function fmtBytes(b) {
  if (!b || b <= 0) return "";
  if (b < 1024) return b + " B";
  const mb = b / (1024 * 1024);
  return mb >= 1 ? mb.toFixed(1) + " MB" : Math.round(b / 1024) + " KB";
}
function friendlyAssetLabel(name) {
  const lower = name.toLowerCase();
  if (lower.endsWith(".dmg")) {
    if (lower.includes("universal")) return t("assetMacUniversal");
    if (lower.includes("arm64") || lower.includes("aarch64")) return t("assetMacArm");
    if (lower.includes("x64") || lower.includes("x86_64") || lower.includes("intel")) return t("assetMacX64");
    return t("assetMac");
  }
  if (lower.endsWith("setup.exe")) return t("assetWinSetup");
  if (lower.endsWith("portable.zip")) return t("assetWinZip");
  if (lower.includes("sha256")) return t("assetSha");
  return name;
}

const SUN_SVG = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"/></svg>';
const MOON_SVG = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/></svg>';
const DL_SVG = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>';
const CHECK_SVG = '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="var(--c-ok)" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></svg>';

function applyTheme() {
  document.documentElement.setAttribute("data-theme", state.theme);
  document.getElementById("themeBtn").innerHTML = state.theme === "dark" ? SUN_SVG : MOON_SVG;
}

function renderSpotlight() {
  const latest = FEED.releases.find((r) => r.version === (FEED.latestStable || FEED.latestAll));
  const el = document.getElementById("spotlight");
  if (!latest) { el.innerHTML = ""; return; }
  const dlAssets = (latest.assets || []).filter((a) => !a.name.toLowerCase().includes("sha256")).slice(0, 6);
  const shaAsset = (latest.assets || []).find((a) => a.name.toLowerCase().includes("sha256"));
  let rowsHtml = "";
  if (dlAssets.length > 0) {
    rowsHtml = dlAssets.map((a) =>
      '<a class="dl-row" href="' + esc(a.downloadUrl) + '" target="_blank" rel="noopener">' +
        '<span class="dl-row-left">' + DL_SVG + '<span>' + esc(friendlyAssetLabel(a.name)) + '</span></span>' +
        '<span class="dl-meta">' + esc(fmtBytes(a.size)) + '</span>' +
      '</a>'
    ).join("");
    if (shaAsset) {
      rowsHtml += '<a class="dl-row" href="' + esc(shaAsset.downloadUrl) + '" target="_blank" rel="noopener">' +
        '<span class="dl-row-left">' + CHECK_SVG + '<span>' + esc(t("assetSha")) + '</span></span>' +
        '<span class="dl-meta">SHA-256</span>' +
      '</a>';
    }
  } else {
    rowsHtml = '<div style="font-size:12px;color:var(--c-muted);padding:10px 12px;border-radius:9px;border:1px dashed var(--c-line-strong);background:var(--c-canvas);line-height:1.5;">' +
      esc(t("spotEmpty")) + '</div>';
  }
  el.innerHTML =
    '<div class="spot-head">' +
      '<span class="spot-label">' + esc(t("spotLabel")) + '</span>' +
      '<span class="spot-date">' + esc(latest.publishedAt || "") + '</span>' +
    '</div>' +
    '<div class="spot-ver">v' + esc(latest.version) + '</div>' +
    '<div class="spot-downloads">' + rowsHtml + '</div>';
}

function readHashVersion() {
  if (typeof location === "undefined" || !location.hash) return null;
  const m = /^#rel-(.+)$/.exec(location.hash);
  if (!m) return null;
  try { return decodeURIComponent(m[1]); } catch { return m[1]; }
}

function setActiveVersion(ver) {
  if (!ver || state.activeVersion === ver) return;
  state.activeVersion = ver;
  document.querySelectorAll(".ver-nav-link").forEach((el) => {
    el.classList.toggle("active", el.dataset.ver === ver);
  });
  document.querySelectorAll(".release-card").forEach((el) => {
    el.classList.toggle("is-active", el.dataset.ver === ver);
  });
}

function syncActiveFromScroll() {
  if (typeof window === "undefined") return;
  const cards = Array.from(document.querySelectorAll(".release-card"));
  if (cards.length === 0) return;
  const docEl = document.documentElement;
  const atBottom =
    docEl &&
    typeof window.scrollY === "number" &&
    typeof window.innerHeight === "number" &&
    window.scrollY > 40 &&
    window.scrollY + window.innerHeight >= docEl.scrollHeight - 24;
  if (atBottom) {
    setActiveVersion(cards[cards.length - 1].dataset.ver);
    return;
  }
  const anchorY = 140;
  let current = cards[0].dataset.ver;
  for (const card of cards) {
    if (typeof card.getBoundingClientRect !== "function") break;
    if (card.getBoundingClientRect().top <= anchorY) {
      current = card.dataset.ver;
    } else {
      break;
    }
  }
  setActiveVersion(current);
}

let scrollRaf = 0;
let clickLockUntil = 0;
if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
  window.addEventListener("scroll", () => {
    if (Date.now() < clickLockUntil) return;
    if (scrollRaf) return;
    scrollRaf = requestAnimationFrame(() => {
      scrollRaf = 0;
      syncActiveFromScroll();
    });
  }, { passive: true });
  window.addEventListener("hashchange", () => {
    const hashVer = readHashVersion();
    if (!hashVer) return;
    clickLockUntil = Date.now() + 450;
    setActiveVersion(hashVer);
    const target = document.getElementById("rel-" + hashVer);
    if (target && typeof target.scrollIntoView === "function") {
      target.scrollIntoView({ behavior: "smooth", block: "start" });
    }
  });
}

function renderTimeline() {
  const q = state.query.trim().toLowerCase();
  const filtered = FEED.releases.filter((r) => {
    if (state.channel === "stable" && r.prerelease) return false;
    if (state.channel === "pre" && !r.prerelease) return false;
    if (!q) return true;
    if (r.version.toLowerCase().includes(q)) return true;
    return (r.notes || []).some((n) => (n.text || "").toLowerCase().includes(q));
  });

  let hashMatched = false;
  if (filtered.length === 0) {
    state.activeVersion = null;
  } else if (!filtered.some((r) => r.version === state.activeVersion)) {
    const hashVer = readHashVersion();
    if (hashVer && filtered.some((r) => r.version === hashVer)) {
      state.activeVersion = hashVer;
      hashMatched = true;
    } else {
      state.activeVersion = filtered[0].version;
    }
  }

  const nav = document.getElementById("verNavList");
  nav.innerHTML = filtered.map((r) =>
    '<a class="ver-nav-link ' + (r.version === state.activeVersion ? 'active' : '') + '" data-ver="' + esc(r.version) + '" href="#rel-' + esc(r.version) + '">' +
      '<span>v' + esc(r.version) + '</span>' +
      '<span class="ver-nav-date">' + esc((r.publishedAt || "").slice(5)) + '</span>' +
    '</a>'
  ).join("");

  document.querySelectorAll(".ver-nav-link").forEach((link) => {
    link.addEventListener("click", (e) => {
      const ver = link.dataset.ver;
      if (!ver) return;
      clickLockUntil = Date.now() + 650;
      setActiveVersion(ver);
      const target = document.getElementById("rel-" + ver);
      if (target && typeof target.scrollIntoView === "function") {
        e.preventDefault();
        target.scrollIntoView({ behavior: "smooth", block: "start" });
        if (typeof history !== "undefined" && typeof history.replaceState === "function") {
          history.replaceState(null, "", "#rel-" + ver);
        }
      }
    });
  });

  const root = document.getElementById("timeline");
  if (filtered.length === 0) {
    root.innerHTML = '<div class="empty-state">' + esc(t(FEED.releases.length ? "emptySearch" : "noReleases")) + '</div>';
    return;
  }

  const tags = t("tags");
  root.innerHTML = filtered.map((r) => {
    const isLatest = !r.prerelease && r.version === FEED.latestStable;
    const isActive = r.version === state.activeVersion;
    const pill = r.prerelease
      ? '<span class="pill pre">Pre-release</span>'
      : (isLatest ? '<span class="pill">Latest</span>' : '');
    const notesHtml = r.notes && r.notes.length
      ? '<ul class="notes">' + r.notes.map((n) =>
          '<li><span class="tag ' + esc(n.kind || "other") + '">' + esc(tags[n.kind] || tags.other) + '</span><span>' + esc(n.text) + '</span></li>'
        ).join("") + '</ul>'
      : '<p style="font-size:12.5px;color:var(--c-faint);">' + esc(t("noNotes")) + '</p>';
    const assetsHtml = r.assets && r.assets.length
      ? '<div class="assets">' + r.assets.map((a) =>
          '<a class="asset-chip" href="' + esc(a.downloadUrl) + '" target="_blank" rel="noopener">' +
            '<span>' + esc(friendlyAssetLabel(a.name)) + '</span>' +
            (a.size ? '<span class="asset-size">' + esc(fmtBytes(a.size)) + '</span>' : '') +
          '</a>'
        ).join("") + '</div>'
      : '';
    return '<article class="release-card ' + (isLatest ? 'latest ' : '') + (isActive ? 'is-active' : '') + '" data-ver="' + esc(r.version) + '" id="rel-' + esc(r.version) + '">' +
      '<div class="card-head">' +
        '<div class="card-ver-row">' +
          '<a class="ver-link" href="' + esc(r.url) + '" target="_blank" rel="noopener">v' + esc(r.version) + '</a>' +
          pill +
        '</div>' +
        '<div class="card-head-right">' +
          '<span>' + esc(r.publishedAt || "") + '</span>' +
          '<a class="card-gh-link" href="' + esc(r.url) + '" target="_blank" rel="noopener">' + esc(t("viewOnGh")) + '</a>' +
        '</div>' +
      '</div>' +
      notesHtml +
      assetsHtml +
    '</article>';
  }).join("");
  if (hashMatched && state.activeVersion) {
    clickLockUntil = Date.now() + 450;
    const target = document.getElementById("rel-" + state.activeVersion);
    if (target && typeof target.scrollIntoView === "function") {
      target.scrollIntoView({ behavior: "auto", block: "start" });
    }
  } else if (Date.now() >= clickLockUntil) {
    syncActiveFromScroll();
  }
}

function renderAll() {
  applyTheme();
  document.getElementById("langBtn").textContent = state.lang === "zh" ? "EN" : "中";
  document.querySelectorAll("[data-i18n]").forEach((el) => {
    const val = t(el.dataset.i18n);
    if (typeof val === "string") el.textContent = val;
  });
  document.getElementById("heroSlogan").innerHTML = t("sloganHtml");
  document.getElementById("searchInput").placeholder = t("searchPh");
  renderSpotlight();
  renderTimeline();
}

document.getElementById("themeBtn").onclick = () => {
  state.theme = state.theme === "dark" ? "light" : "dark";
  localStorage.setItem("hivecast-cl-theme", state.theme);
  applyTheme();
};
document.getElementById("langBtn").onclick = () => {
  state.lang = state.lang === "zh" ? "en" : "zh";
  localStorage.setItem("hivecast-cl-lang", state.lang);
  renderAll();
};
document.querySelectorAll("#channelSeg button").forEach((b) => {
  b.onclick = () => {
    state.channel = b.dataset.ch;
    document.querySelectorAll("#channelSeg button").forEach((x) => x.classList.toggle("active", x === b));
    renderTimeline();
  };
});
document.getElementById("searchInput").addEventListener("input", (e) => {
  state.query = e.target.value;
  renderTimeline();
});

renderAll();
</script>
</body>
</html>
`;
}

async function main() {
  const { outDir, inputFile } = parseArgs(process.argv.slice(2));
  const releases = await fetchReleases(inputFile);
  const ranked = releases.filter(r => versionPattern.test(r.version)).sort((a, b) => compareVersion(b, a));
  const latestStable = ranked.find(r => !r.prerelease && !versionPattern.exec(r.version)[4]);
  const latestAll = ranked[0];
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
    version: latestFeed ? latestFeed.version : null,
    publishedAt: latestFeed ? latestFeed.publishedAt : "",
    url: latestFeed ? latestFeed.url : RELEASES_URL,
    prerelease: latestFeed ? latestFeed.prerelease : false,
    notes: latestFeed ? latestFeed.notes : [],
  };

  mkdirSync(outDir, { recursive: true });
  const iconCandidates = [
    resolve("src/assets/app-icon.png"),
    resolve("src-tauri/icons/128x128.png"),
  ];
  const iconSrc = iconCandidates.find((p) => existsSync(p));
  if (iconSrc) {
    copyFileSync(iconSrc, join(outDir, "icon.png"));
  }

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

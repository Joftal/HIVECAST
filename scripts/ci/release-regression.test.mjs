import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { reconcileLabels } from "./issue-labels.mjs";
import { buildReleaseNotes, selectPreviousTag, shouldMarkLatest } from "./generate-release-notes.mjs";

function generate(t, releases) {
  const dir = mkdtempSync(join(tmpdir(), "hivecast-site-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const input = join(dir, "input.json");
  writeFileSync(input, JSON.stringify(releases));
  execFileSync(process.execPath, ["scripts/ci/generate-changelog-site.mjs", "--input", input, "--out", dir]);
  const read = name => readFileSync(join(dir, name), "utf8");
  const elements = new Map();
  const document = {
    documentElement: { setAttribute() {} },
    querySelectorAll: () => [],
    getElementById(id) {
      if (!elements.has(id)) elements.set(id, { addEventListener(event, cb) { this[event] = cb; } });
      return elements.get(id);
    },
  };
  runInNewContext(read("index.html").match(/<script>([\s\S]*?)<\/script>/)[1], {
    document, localStorage: { getItem: () => null }, navigator: { language: "zh" },
    matchMedia: () => ({ matches: false }),
  });
  return { feed: JSON.parse(read("changelog.json")), latest: JSON.parse(read("latest.json")), elements };
}

test("backports, RC numbering, platform notes and the filtered Latest badge", t => {
  const releases = ["1.1.9", "2.0.0", "3.0.0-rc.2", "3.0.0-rc.10"].map(tag_name => ({
    tag_name, prerelease: tag_name.includes("-"), body: "## 安装说明\n### Windows（x64）\n- setup\n## Windows fixes\n- fix: capture\n## macOS 修复\n- fix: playback",
  }));
  const { feed, latest, elements } = generate(t, releases);
  assert.equal(feed.latestAll, "3.0.0-rc.10");
  assert.equal(latest.version, "2.0.0");
  assert.deepEqual(latest.notes.map(n => n.text), ["capture", "playback"]);
  assert.match(elements.get("spotlight").innerHTML, /v2\.0\.0/);
  elements.get("searchInput").input({ target: { value: "1.1.9" } });
  assert.match(elements.get("timeline").innerHTML, /1\.1\.9/);
  assert.doesNotMatch(elements.get("timeline").innerHTML, />Latest</);
});

test("no published releases produce an empty site and subscription", t => {
  for (const input of [[], [{ tag_name: "2.0.0", draft: true }]]) {
    const { feed, latest, elements } = generate(t, input);
    assert.equal(latest.version, null);
    assert.deepEqual(feed.releases, []);
    assert.match(elements.get("timeline").innerHTML, /暂无发布/);
    assert.equal(elements.get("spotlight").innerHTML, "");
  }
});

test("form edits and cleared selections preserve manually applied labels", async () => {
  const labels = new Set(["os: windows", "area: playback", "priority: high"]);
  let body = "### 操作系统\nmacOS";
  const url = "https://example.invalid/issues/1";
  async function request(path, options) {
    if (!options.method) return { ok: true, json: () => ({ body, labels: [...labels].map(name => ({ name })) }) };
    if (options.method === "POST") JSON.parse(options.body).labels.forEach(l => labels.add(l));
    else if (options.method === "DELETE") labels.delete(decodeURIComponent(path.split("/").pop()));
    else assert.fail(options.method);
    return { ok: true };
  }
  await reconcileLabels(url, "### 操作系统\nWindows", {}, request);
  assert.deepEqual(labels, new Set(["os: macos", "area: playback", "priority: high"]));
  const previous = body;
  body = "### 操作系统\n_No response_";
  await reconcileLabels(url, previous, {}, request);
  assert.deepEqual(labels, new Set(["area: playback", "priority: high"]));
});

test("selectPreviousTag respects SemVer precedence and stable/prerelease channels", () => {
  const tags = ["v0.9.0", "v1.0.0", "v1.1.0-rc.2", "v1.1.0-rc.10", "v1.1.0", "not-a-tag"];
  assert.equal(selectPreviousTag(tags, "1.1.0"), "v1.0.0");
  assert.equal(selectPreviousTag(tags, "1.1.0-rc.11"), "v1.1.0-rc.10");
  assert.equal(selectPreviousTag(tags, "1.2.0-rc.1"), "v1.1.0");
  assert.equal(selectPreviousTag(["v1.0.0-rc.1"], "1.0.0"), null);
  assert.equal(selectPreviousTag([], "1.0.0"), null);
});

test("generated release notes lead with categorized commits and feed cleanly into changelog site", t => {
  const body = buildReleaseNotes({
    version: "1.0.1",
    signMacos: false,
    repo: "Joftal/HIVECAST",
    prevTag: "v1.0.0",
    commits: [
      { hash: "0e54571abcdef1234567890", shortHash: "0e54571", subject: "feat: Windows 采用安装目录优先布局" },
      { hash: "da080baabcdef1234567890", shortHash: "da080ba", subject: "fix(installer): 浏览目录自动补齐 HIVECAST" },
      { hash: "c0dadcbabcdef1234567890", shortHash: "c0dadcb", subject: "style: 对齐关于页图标视觉" },
      { hash: "da16d54abcdef1234567890", shortHash: "da16d54", subject: "chore: 新增分诊工作流" },
    ],
  });

  assert.ok(body.indexOf("## 更新内容") < body.indexOf("## 安装说明"));
  assert.match(body, /### ✨ 新增功能/);
  assert.match(body, /### 🐛 问题修复/);
  assert.match(body, /### ⚡ 优化与体验/);
  assert.match(body, /### 🔧 工程与维护/);
  assert.match(body, /\*\*installer\*\*：浏览目录自动补齐 HIVECAST/);
  assert.match(body, /compare\/v1\.0\.0\.\.\.v1\.0\.1/);
  assert.match(body, /HIVECAST-v1\.0\.1-macOS-universal\.dmg/);
  assert.doesNotMatch(body, /当前版本请从发布页手动更新/);

  const { latest, elements } = generate(t, [
    {
      tag_name: "v1.0.1",
      prerelease: false,
      body,
      assets: [
        { name: "HIVECAST-v1.0.1-macOS-universal.dmg", size: 30 * 1024 * 1024, browser_download_url: "https://example.invalid/universal.dmg" },
        { name: "HIVECAST-v1.0.1-Windows-x64-setup.exe", size: 15 * 1024 * 1024, browser_download_url: "https://example.invalid/setup.exe" },
        { name: "HIVECAST-v1.0.1-Windows-x64-portable.zip", size: 14 * 1024 * 1024, browser_download_url: "https://example.invalid/portable.zip" },
      ],
    },
    {
      tag_name: "v1.0.0",
      prerelease: false,
      body: "## 更新内容\n### ✨ 新增功能\n- 首个正式版",
      assets: [
        { name: "HIVECAST-v1.0.0-macOS-arm64.dmg", size: 16 * 1024 * 1024, browser_download_url: "https://example.invalid/arm64.dmg" },
      ],
    },
  ]);
  assert.equal(latest.version, "1.0.1");
  assert.match(elements.get("spotlight").innerHTML, /macOS 通用版/);
  assert.match(elements.get("timeline").innerHTML, /macOS Apple Silicon/);
  assert.deepEqual(latest.notes, [
    { kind: "feat", text: "Windows 采用安装目录优先布局" },
    { kind: "fix", text: "installer：浏览目录自动补齐 HIVECAST" },
    { kind: "perf", text: "对齐关于页图标视觉" },
    { kind: "other", text: "新增分诊工作流" },
  ]);

  const manyCommits = Array.from({ length: 24 }, (_, idx) => ({
    hash: `abcdef${String(idx).padStart(2, "0")}1234567890`,
    shortHash: `abc${String(idx).padStart(4, "0")}`,
    subject: `fix: 修复 \`item-${idx + 1}\` 同步异常`,
  }));
  const largeBody = buildReleaseNotes({
    version: "1.0.2",
    prevTag: "v1.0.1",
    commits: manyCommits,
  });
  const { latest: largeLatest } = generate(t, [{ tag_name: "v1.0.2", prerelease: false, body: largeBody }]);
  assert.equal(largeLatest.notes.length, 24);
  assert.equal(largeLatest.notes[0].text, "item-1 同步异常");
  assert.equal(largeLatest.notes[23].text, "item-24 同步异常");
});

test("GitHub Latest skips maintenance releases, drafts and prereleases", () => {
  const releases = [{ tag_name: "v2.0.0" }, { tag_name: "v4.0.0", draft: true },
    { tag_name: "v3.0.1", prerelease: true }, { tag_name: "v4.1.0-rc.1" }];
  assert.equal(shouldMarkLatest("1.1.9", releases), false);
  assert.equal(shouldMarkLatest("2.0.1", releases), true);
  assert.equal(shouldMarkLatest("9.0.0-rc.1", []), false);
  assert.equal(shouldMarkLatest("1.0.0", []), true);
});

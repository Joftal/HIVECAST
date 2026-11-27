import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { reconcileLabels } from "./issue-labels.mjs";
import { buildReleaseNotes, selectPreviousTag } from "./generate-release-notes.mjs";

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
  assert.doesNotMatch(body, /当前版本请从发布页手动更新/);

  const { latest } = generate(t, [{ tag_name: "v1.0.1", prerelease: false, body }]);
  assert.equal(latest.version, "1.0.1");
  assert.deepEqual(latest.notes, [
    { kind: "feat", text: "Windows 采用安装目录优先布局" },
    { kind: "fix", text: "installer：浏览目录自动补齐 HIVECAST" },
    { kind: "perf", text: "对齐关于页图标视觉" },
    { kind: "other", text: "新增分诊工作流" },
  ]);
});

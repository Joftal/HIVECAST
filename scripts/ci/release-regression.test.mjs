import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { reconcileLabels } from "./issue-labels.mjs";

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

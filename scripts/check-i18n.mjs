// CI 用：确保中英两套文案 key 完全一致，避免出现只翻译一半的界面。
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const dir = join(here, "../src/i18n");
const locales = ["zh-CN", "en-US"];

function flatten(value, prefix = "", out = new Set()) {
  for (const [key, child] of Object.entries(value)) {
    const next = prefix ? prefix + "." + key : key;
    if (child && typeof child === "object" && !Array.isArray(child)) flatten(child, next, out);
    else out.add(next);
  }
  return out;
}

const sets = Object.fromEntries(
  locales.map((code) => [code, flatten(JSON.parse(readFileSync(join(dir, code + ".json"), "utf8")))]),
);

let failed = false;
for (const code of locales) {
  for (const other of locales) {
    if (code === other) continue;
    const missing = [...sets[other]].filter((key) => !sets[code].has(key));
    if (missing.length) {
      failed = true;
      console.error(code + " missing " + missing.length + " keys: " + missing.slice(0, 10).join(", "));
    }
  }
}

if (failed) process.exit(1);
console.log("i18n ok: " + sets[locales[0]].size + " keys");

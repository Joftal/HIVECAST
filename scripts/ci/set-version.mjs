#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const version = process.argv[2];
if (!version) {
  console.error("用法: node scripts/ci/set-version.mjs <semver>");
  process.exit(2);
}

// 与 Tauri / GitHub Release 使用的语义化版本保持一致；不带前导 v。
const SEMVER =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?$/;
if (!SEMVER.test(version)) {
  console.error("版本号必须是 SemVer，例如 0.2.0 或 0.2.0-rc.1（不要带 v）");
  process.exit(2);
}

const root = fileURLToPath(new URL("../../", import.meta.url));
const pkgPath = root + "package.json";
const tauriPath = root + "src-tauri/tauri.conf.json";
const cargoPath = root + "src-tauri/Cargo.toml";
const lockPath = root + "src-tauri/Cargo.lock";

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function writeJson(path, value) {
  writeFileSync(path, JSON.stringify(value, null, 2) + "\n");
}

const pkg = readJson(pkgPath);
pkg.version = version;
writeJson(pkgPath, pkg);

const tauri = readJson(tauriPath);
tauri.version = version;
writeJson(tauriPath, tauri);

const cargo = readFileSync(cargoPath, "utf8");
const packageAt = cargo.indexOf("[package]");
if (packageAt < 0) throw new Error("Cargo.toml 缺少 [package]");
const before = cargo.slice(0, packageAt);
const after = cargo.slice(packageAt);
// 只看 [package] 段，别改到依赖里的 version；同版本重跑也必须成功。
const nextSection = after.indexOf("\n[", 1);
const packageBody = nextSection < 0 ? after : after.slice(0, nextSection);
const versionLine = /^version = "[^"]*"$/m;
if (!versionLine.test(packageBody)) throw new Error("Cargo.toml [package] 缺少 version");
const nextBody = packageBody.replace(versionLine, `version = "${version}"`);
writeFileSync(cargoPath, before + nextBody + after.slice(packageBody.length));

const lock = readFileSync(lockPath, "utf8");
const lockPattern = /(\[\[package\]\]\nname = "hivecast"\nversion = ")[^"]+(")/;
if (!lockPattern.test(lock)) throw new Error("Cargo.lock 缺少根包 hivecast");
const nextLock = lock.replace(lockPattern, `$1${version}$2`);
writeFileSync(lockPath, nextLock);

const checks = [
  [pkgPath, readJson(pkgPath).version],
  [tauriPath, readJson(tauriPath).version],
  [cargoPath, /^version = "([^"]+)"/m.exec(readFileSync(cargoPath, "utf8").slice(packageAt))?.[1]],
  [lockPath, /\[\[package\]\]\nname = "hivecast"\nversion = "([^"]+)"/.exec(readFileSync(lockPath, "utf8"))?.[1]],
];
for (const [path, actual] of checks) {
  if (actual !== version) {
    throw new Error(`版本写入失败: ${path} = ${actual ?? "?"}`);
  }
}
console.log(`version: ${version}`);

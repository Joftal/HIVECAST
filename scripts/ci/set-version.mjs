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
const winInstallerCargoPath = root + "src-tauri/installer-win/Cargo.toml";
const winInstallerLockPath = root + "src-tauri/installer-win/Cargo.lock";

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function writeJson(path, value) {
  writeFileSync(path, JSON.stringify(value, null, 2) + "\n");
}

function updateCargoPackageVersion(path, nextVersion) {
  const cargo = readFileSync(path, "utf8");
  const packageAt = cargo.indexOf("[package]");
  if (packageAt < 0) throw new Error(`${path} 缺少 [package]`);
  const before = cargo.slice(0, packageAt);
  const after = cargo.slice(packageAt);
  // 只看 [package] 段，别改到依赖里的 version；同版本重跑也必须成功。
  const nextSection = after.indexOf("\n[", 1);
  const packageBody = nextSection < 0 ? after : after.slice(0, nextSection);
  const versionLine = /^version = "[^"]*"$/m;
  if (!versionLine.test(packageBody)) throw new Error(`${path} [package] 缺少 version`);
  const nextBody = packageBody.replace(versionLine, `version = "${nextVersion}"`);
  writeFileSync(path, before + nextBody + after.slice(packageBody.length));
  return /^version = "([^"]+)"/m.exec(readFileSync(path, "utf8").slice(packageAt))?.[1];
}

function updateCargoLockVersion(path, packageName, nextVersion) {
  const lock = readFileSync(path, "utf8");
  const lockPattern = new RegExp(`(\\[\\[package\\]\\]\\nname = "${packageName}"\\nversion = ")[^"]+(")`);
  if (!lockPattern.test(lock)) throw new Error(`${path} 缺少根包 ${packageName}`);
  const nextLock = lock.replace(lockPattern, `$1${nextVersion}$2`);
  writeFileSync(path, nextLock);
  const verify = new RegExp(`\\[\\[package\\]\\]\\nname = "${packageName}"\\nversion = "([^"]+)"`);
  return verify.exec(readFileSync(path, "utf8"))?.[1];
}

const pkg = readJson(pkgPath);
pkg.version = version;
writeJson(pkgPath, pkg);

const tauri = readJson(tauriPath);
tauri.version = version;
writeJson(tauriPath, tauri);

const checks = [
  [pkgPath, readJson(pkgPath).version],
  [tauriPath, readJson(tauriPath).version],
  [cargoPath, updateCargoPackageVersion(cargoPath, version)],
  [lockPath, updateCargoLockVersion(lockPath, "hivecast", version)],
  [winInstallerCargoPath, updateCargoPackageVersion(winInstallerCargoPath, version)],
  [winInstallerLockPath, updateCargoLockVersion(winInstallerLockPath, "hivecast-installer", version)],
];
for (const [path, actual] of checks) {
  if (actual !== version) {
    throw new Error(`版本写入失败: ${path} = ${actual ?? "?"}`);
  }
}
console.log(`version: ${version}`);

// Writes one semver into every file that carries the app version.
// Usage: bun .github/scripts/set-version.mjs <version | vversion>
import { readFileSync, writeFileSync } from "node:fs";

const version = (process.argv[2] ?? "").replace(/^v/, "");
if (!/^\d+\.\d+\.\d+$/.test(version)) {
  console.error(`Expected a plain X.Y.Z version, got "${process.argv[2] ?? ""}".`);
  process.exit(1);
}

const targets = [
  ["package.json", /^(\s*"version":\s*")[^"]+(")/m],
  ["src-tauri/tauri.conf.json", /^(\s*"version":\s*")[^"]+(")/m],
  ["src-tauri/Cargo.toml", /^(\[package\][^[]*?\nversion\s*=\s*")[^"]+(")/],
  ["src-tauri/Cargo.lock", /(\[\[package\]\]\nname = "database-contatti"\nversion = ")[^"]+(")/],
];

for (const [file, pattern] of targets) {
  const text = readFileSync(file, "utf8");
  if (!pattern.test(text)) {
    console.error(`No version found in ${file}.`);
    process.exit(1);
  }
  writeFileSync(file, text.replace(pattern, `$1${version}$2`));
  console.log(`${file} -> ${version}`);
}

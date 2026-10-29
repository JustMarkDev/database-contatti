// Prints the Tauri updater manifest (latest.json) for the Windows installers in a directory.
// Usage: node .github/scripts/updater-manifest.mjs <tag> <owner/repo> <assets-dir>
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const [tag, repo, dir] = process.argv.slice(2);
if (!/^v\d+\.\d+\.\d+$/.test(tag ?? "") || !/^[\w.-]+\/[\w.-]+$/.test(repo ?? "") || !dir) {
  console.error("Usage: updater-manifest.mjs <vX.Y.Z> <owner/repo> <assets-dir>");
  process.exit(1);
}

const files = readdirSync(dir);
const platform = (suffix) => {
  const matches = files.filter((name) => name.endsWith(suffix));
  if (matches.length !== 1 || !files.includes(`${matches[0]}.sig`)) {
    console.error(`Expected exactly one *${suffix} with a matching .sig in ${dir}.`);
    process.exit(1);
  }
  return {
    signature: readFileSync(join(dir, `${matches[0]}.sig`), "utf8").trim(),
    url: `https://github.com/${repo}/releases/download/${tag}/${encodeURIComponent(matches[0])}`,
  };
};

const nsis = platform("-setup.exe");
const msi = platform(".msi");

// The updater picks the key for the installer type the app was installed with and
// falls back to the plain target key.
console.log(
  JSON.stringify(
    {
      version: tag.slice(1),
      pub_date: new Date().toISOString(),
      platforms: {
        "windows-x86_64": nsis,
        "windows-x86_64-nsis": nsis,
        "windows-x86_64-msi": msi,
      },
    },
    null,
    2,
  ),
);

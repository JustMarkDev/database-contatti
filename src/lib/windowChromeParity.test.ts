/// <reference types="node" />
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vite-plus/test";

/**
 * The pre-paint script in `index.html` runs before the React bundle loads, so
 * it cannot import `windowChrome.ts`. It duplicates the platform/user-agent
 * regexes, the Tauri detector, and the dataset values by hand. These tests pin
 * that duplication so the two copies cannot silently drift.
 *
 * Resolution is relative to this file (not `process.cwd()`) so the test also
 * passes when the runner is invoked from a different directory.
 */
const windowChromeSource = readFileSync(join(import.meta.dirname, "windowChrome.ts"), "utf8");
const indexHtmlSource = readFileSync(join(import.meta.dirname, "..", "..", "index.html"), "utf8");

/** Pull a `const NAME = /regex/;` literal straight out of the TS source. */
function extractRegexLiteral(source: string, name: string): string {
  const match = source.match(new RegExp(`const ${name} = (\\/[^\\n]*?\\/);`));
  const literal = match?.[1];
  if (!literal) throw new Error(`Could not extract ${name} from windowChrome.ts`);
  return literal;
}

const platformRegex = extractRegexLiteral(windowChromeSource, "MAC_PLATFORM");
const userAgentRegex = extractRegexLiteral(windowChromeSource, "MAC_USER_AGENT");

describe("index.html window chrome parity", () => {
  it("keeps the desktop-Mac-only regexes in windowChrome.ts", () => {
    expect(platformRegex).toBe("/Mac/");
    expect(userAgentRegex).toBe("/Macintosh/");
    expect(platformRegex).not.toMatch(/iPhone|iPad|iPod/);
    expect(userAgentRegex).not.toMatch(/iPhone|iPad|iPod/);
  });

  it("mirrors both source regexes in the inline pre-paint script", () => {
    expect(indexHtmlSource).toContain(platformRegex);
    expect(indexHtmlSource).toContain(userAgentRegex);
    expect(indexHtmlSource).toContain(
      `var isMac = ${platformRegex}.test(navigator.platform) || ${userAgentRegex}.test(navigator.userAgent);`,
    );
  });

  it("sets the same dataset chrome values in both copies", () => {
    for (const value of ['"mac-overlay"', '"default"']) {
      expect(windowChromeSource).toContain(value);
      expect(indexHtmlSource).toContain(value);
    }
  });

  it("mirrors the isTauri() Tauri boundary check", () => {
    expect(indexHtmlSource).toContain("!!globalThis.isTauri");
  });

  it("does not regress to the broad iOS-matching platform regex", () => {
    expect(indexHtmlSource).not.toMatch(/iPhone|iPad|iPod/);
  });
});

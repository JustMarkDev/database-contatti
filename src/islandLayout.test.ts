/// <reference types="node" />
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vite-plus/test";

/**
 * Regression guards for the single-island layout, the Windows/default-chrome
 * sidebar-toggle clearance, and the overlay paint order.
 *
 * The island flattening rules must stay UNLAYERED. Tailwind's utilities layer
 * (emitted by `@import "tailwindcss"`) always beats `@layer components`, so a
 * flatten rule moved back inside a layer would be silently overridden by Card's
 * `border`/`rounded-xl` utilities and the island would grow inner card chrome
 * again. These checks parse the CSS source directly (brace balance) so they hold
 * without a browser cascade.
 */

/** `import.meta.dirname` is Node 20.11+; Vitest's SSR transform may omit it. */
const importMetaRecord = import.meta as unknown as Record<string, unknown>;
const importMetaDirname =
  typeof importMetaRecord.dirname === "string" ? importMetaRecord.dirname : undefined;
const moduleDir = importMetaDirname ?? dirname(fileURLToPath(import.meta.url));

/**
 * Resolve App.css relative to THIS module first, so the parser behaves the same
 * whether Vitest runs from the repo root or from a workspace directory, and
 * fall back to `<cwd>/src/App.css` only when the module-relative path is gone.
 */
function resolveAppCssPath(): string {
  const candidates = [join(moduleDir, "App.css"), join(process.cwd(), "src", "App.css")];
  for (const candidate of candidates) {
    if (existsSync(candidate)) return candidate;
  }
  throw new Error(`App.css not found; tried ${candidates.join(", ")}`);
}

const appCssPath = resolveAppCssPath();
const appCssSource = readFileSync(appCssPath, "utf8");

/** Replace every non-newline character with a space, preserving offsets. */
function blank(text: string): string {
  return text.replace(/[^\n]/g, " ");
}

/**
 * Single-pass CSS scanner. Block comments are always blanked. When
 * `maskStrings` is set, string literals are blanked too so brace counting is not
 * fooled by `content: "{"` or `content: "/*"`. Blanking (rather than deleting)
 * keeps every offset aligned with the raw source, so selector lookup runs
 * against the string-preserving variant while brace walking runs against the
 * masked one.
 */
function sanitize(source: string, maskStrings: boolean): string {
  let result = "";
  let i = 0;
  while (i < source.length) {
    const char = source[i];
    if (char === "/" && source[i + 1] === "*") {
      const end = source.indexOf("*/", i + 2);
      const stop = end === -1 ? source.length : end + 2;
      result += blank(source.slice(i, stop));
      i = stop;
      continue;
    }
    if (char === '"' || char === "'") {
      let j = i + 1;
      while (j < source.length) {
        if (source[j] === "\\") {
          j += 2;
          continue;
        }
        if (source[j] === char) {
          j += 1;
          break;
        }
        j += 1;
      }
      result += maskStrings ? blank(source.slice(i, j)) : source.slice(i, j);
      i = j;
      continue;
    }
    result += char;
    i += 1;
  }
  return result;
}

/** Comment-free source; string literals survive for attribute selectors. */
function stripComments(source: string): string {
  return sanitize(source, false);
}

/** Comment- and string-free source used for brace/at-rule walking. */
function stripCommentsAndStrings(source: string): string {
  return sanitize(source, true);
}

const css = stripComments(appCssSource);
const scanCss = stripCommentsAndStrings(appCssSource);

const LAYER_PRELUDE = /^@layer(?:\s|$)/;
const MOBILE_MEDIA_PRELUDE = /@media\s*\(\s*max-width:\s*767px\s*\)/;
const SELECTOR_BOUNDARY_BEFORE = new Set(["{", "}", ";", ","]);

interface BlockFrame {
  isLayer: boolean;
  prelude: string;
}

/**
 * Brace-balance walk. Returns the frames enclosing `offset` (the rule opened at
 * `offset` itself is not included). Pass the masked source so string/comment
 * content cannot unbalance the walk.
 */
function framesAt(source: string, offset: number): BlockFrame[] {
  const stack: BlockFrame[] = [];
  let preludeStart = 0;
  for (let i = 0; i < offset; i += 1) {
    const char = source[i];
    if (char === "{") {
      const prelude = source.slice(preludeStart, i).trim();
      stack.push({ isLayer: LAYER_PRELUDE.test(prelude), prelude });
      preludeStart = i + 1;
    } else if (char === "}") {
      stack.pop();
      preludeStart = i + 1;
    } else if (char === ";") {
      preludeStart = i + 1;
    }
  }
  return stack;
}

/** Ranges of every `@layer name { ... }` block; `end` is the closing brace. */
function layerBlockRanges(source: string): Array<{ start: number; end: number }> {
  const stack: Array<{ isLayer: boolean; start: number }> = [];
  const ranges: Array<{ start: number; end: number }> = [];
  let preludeStart = 0;
  for (let i = 0; i < source.length; i += 1) {
    const char = source[i];
    if (char === "{") {
      stack.push({
        isLayer: LAYER_PRELUDE.test(source.slice(preludeStart, i).trim()),
        start: preludeStart,
      });
      preludeStart = i + 1;
    } else if (char === "}") {
      const frame = stack.pop();
      if (frame?.isLayer) {
        ranges.push({ start: frame.start, end: i });
      }
      preludeStart = i + 1;
    } else if (char === ";") {
      preludeStart = i + 1;
    }
  }
  return ranges;
}

/**
 * Indices where `selector` appears as a complete selector inside a rule prelude
 * (bounded by `{`/`}`/`;`/`,` before and `{`/`,` after). Exact selector bounds
 * stop a prefix like `.content-island > .ai-panel` from matching
 * `.content-island > .ai-panel > [data-slot="card-header"]`.
 */
function ruleSelectorIndices(source: string, selector: string): number[] {
  const indices: number[] = [];
  let from = 0;
  while (from <= source.length) {
    const index = source.indexOf(selector, from);
    if (index === -1) break;
    const before = source.slice(0, index).trimEnd();
    const previous = before[before.length - 1] ?? "";
    const after = source.slice(index + selector.length).trimStart();
    const next = after[0] ?? "";
    const previousOk = previous === "" || SELECTOR_BOUNDARY_BEFORE.has(previous);
    const nextOk = next === "{" || next === ",";
    if (previousOk && nextOk) {
      indices.push(index);
      from = index + selector.length;
    } else {
      from = index + 1;
    }
  }
  return indices;
}

function ruleSelectorIndex(source: string, selector: string): number {
  const [first] = ruleSelectorIndices(source, selector);
  if (first === undefined) throw new Error(`rule selector not found in App.css: ${selector}`);
  return first;
}

/** Index of the opening brace of the `occurrence`-th rule for `selector`. */
function openingBraceAt(source: string, selector: string, occurrence = 0): number {
  const indices = ruleSelectorIndices(source, selector);
  const selectorIndex = indices[occurrence];
  if (selectorIndex === undefined) {
    throw new Error(`missing selector in App.css: ${selector} (occurrence ${occurrence})`);
  }
  const braceIndex = source.indexOf("{", selectorIndex);
  if (braceIndex <= selectorIndex) {
    throw new Error(`missing declaration block for: ${selector}`);
  }
  return braceIndex;
}

/**
 * Declaration body of the `occurrence`-th rule for `selector`. `lookupSource`
 * keeps string literals (attribute selectors) while `scanSource` is masked so a
 * brace inside `content: "{"` cannot truncate or overrun the body; both share
 * offsets.
 */
function ruleBody(
  lookupSource: string,
  scanSource: string,
  selector: string,
  occurrence = 0,
): string {
  const braceIndex = openingBraceAt(lookupSource, selector, occurrence);
  let depth = 0;
  for (let i = braceIndex; i < scanSource.length; i += 1) {
    if (scanSource[i] === "{") {
      depth += 1;
    } else if (scanSource[i] === "}") {
      depth -= 1;
      if (depth === 0) return scanSource.slice(braceIndex + 1, i);
    }
  }
  throw new Error(`unbalanced declaration block for: ${selector}`);
}

function ruleBodyFor(selector: string, occurrence = 0): string {
  return ruleBody(css, scanCss, selector, occurrence);
}

const PANEL_CLASSES = [
  "ai-panel",
  "blacklist-panel",
  "settings-panel",
  "ai-log-panel",
  "verification-panel",
] as const;

const FLATTEN_SELECTORS = PANEL_CLASSES.map((panel) => `.content-island > .${panel}`);
const FLATTEN_CARD_SELECTORS = PANEL_CLASSES.flatMap((panel) => [
  `.content-island > .${panel} > [data-slot="card-header"]`,
  `.content-island > .${panel} > [data-slot="card-content"]`,
]);
const FLATTEN_DETAILS_SELECTORS = [
  ".content-island > .settings-panel > details > summary",
  '.content-island > .settings-panel > details > [data-slot="card-content"]',
];
/** Every rule the divider must be declared after so `border: 0` cannot win. */
const ALL_FLATTEN_SELECTORS = [
  ...FLATTEN_SELECTORS,
  ...FLATTEN_CARD_SELECTORS,
  ...FLATTEN_DETAILS_SELECTORS,
];

describe("static parser hardening", () => {
  it("keeps string and comment masking offset-aligned with the raw source", () => {
    expect(css.length).toBe(appCssSource.length);
    expect(scanCss.length).toBe(appCssSource.length);
  });

  it("does not count braces from content strings or comment markers", () => {
    const snippet = [
      "@layer demo {",
      '  .a::before { content: "{"; }',
      '  .b::after { content: "/* not a comment */"; }',
      "}",
      '/* .decoy { color: "}" } */',
      ".c { border: 0; }",
    ].join("\n");
    const masked = stripCommentsAndStrings(snippet);
    const ranges = layerBlockRanges(masked);
    expect(ranges).toHaveLength(1);
    const braceIndex = openingBraceAt(masked, ".c");
    expect(framesAt(masked, braceIndex)).toHaveLength(0);
    expect(ruleBody(masked, masked, ".c")).toContain("border: 0");
  });

  it("ignores a flatten selector prefix when locating a rule", () => {
    const snippet = '.content-island > .ai-panel > [data-slot="card-header"] { padding: 0; }';
    expect(ruleSelectorIndices(snippet, ".content-island > .ai-panel")).toHaveLength(0);
    expect(
      ruleSelectorIndices(snippet, '.content-island > .ai-panel > [data-slot="card-header"]'),
    ).toHaveLength(1);
  });
});

describe("island flattening stays outside every @layer", () => {
  const layerRanges = layerBlockRanges(scanCss);
  const lastLayerEnd = layerRanges.reduce((max, range) => Math.max(max, range.end), -1);

  it("parses at least one @layer block from App.css", () => {
    expect(layerRanges.length).toBeGreaterThan(0);
    expect(lastLayerEnd).toBeGreaterThan(0);
  });

  it("declares each flatten selector at the unlayered top level", () => {
    for (const selector of ALL_FLATTEN_SELECTORS) {
      const braceIndex = openingBraceAt(css, selector);
      const enclosing = framesAt(scanCss, braceIndex);
      expect(enclosing.length, `${selector} must not be nested in an at-rule`).toBe(0);
      expect(
        enclosing.some((frame) => frame.isLayer),
        selector,
      ).toBe(false);
    }
  });

  it("places the flatten rules after the last @layer block ends", () => {
    for (const selector of ALL_FLATTEN_SELECTORS) {
      expect(ruleSelectorIndex(css, selector), selector).toBeGreaterThan(lastLayerEnd);
    }
  });

  it("keeps the flatten declarations that must beat Card utilities", () => {
    const body = ruleBodyFor(".content-island > .ai-panel");
    for (const declaration of [
      "margin-bottom: 0",
      "border: 0",
      "border-radius: 0",
      "background: transparent",
      "box-shadow: none",
      "padding: var(--space-4) 20px",
    ]) {
      expect(body, declaration).toContain(declaration);
    }
  });

  it("resets Card header/content horizontal padding at the unlayered level", () => {
    const body = ruleBodyFor('.content-island > .ai-panel > [data-slot="card-header"]');
    expect(body).toContain("padding-left: 0");
    expect(body).toContain("padding-right: 0");
  });
});

describe("island section dividers", () => {
  it("declares the divider after every flatten rule so border:0 cannot erase it", () => {
    const dividerIndex = ruleSelectorIndex(css, ".island-section + .island-section");
    const lastFlattenIndex = ALL_FLATTEN_SELECTORS.reduce(
      (max, selector) => Math.max(max, ruleSelectorIndex(css, selector)),
      -1,
    );
    expect(lastFlattenIndex, "last flatten reset rule").toBeGreaterThanOrEqual(0);
    expect(dividerIndex).toBeGreaterThan(lastFlattenIndex);
    expect(ruleBodyFor(".island-section + .island-section")).toContain(
      "border-top: 1px solid var(--border-soft)",
    );
  });

  it("does not draw a divider under a section header", () => {
    expect(ruleBodyFor(".island-section-header + .island-section")).toContain("border-top: 0");
  });
});

describe("default-chrome toggle clearance", () => {
  it("pins the toggle to the top-right corner on default chrome", () => {
    const body = ruleBodyFor(':root[data-window-chrome="default"] .window-traffic-toggle');
    expect(body).toContain("right: var(--sidebar-island-inset)");
    expect(body).toContain("left: auto");
    expect(body).toContain("top: var(--sidebar-island-inset)");
  });

  it("keeps the non-overlay fallback selector in the same rule as the :root default", () => {
    const defaultBody = ruleBodyFor(':root[data-window-chrome="default"] .window-traffic-toggle');
    const fallbackBody = ruleBodyFor(
      ".window-chrome-layer:not(.window-chrome-overlay) .window-traffic-toggle",
    );
    for (const declaration of [
      "top: var(--sidebar-island-inset)",
      "right: var(--sidebar-island-inset)",
      "left: auto",
    ]) {
      expect(defaultBody, declaration).toContain(declaration);
      expect(fallbackBody, declaration).toContain(declaration);
    }
  });

  it("reserves header space for the toggle size plus an explicit clearance gap", () => {
    const body = ruleBodyFor(':root[data-window-chrome="default"] .island-section-header');
    expect(body).toContain("padding-right");
    expect(body).toContain("var(--titlebar-toggle-size)");
    expect(body).toContain("20px");
    // 12px is the deliberate breathing margin documented next to the rule.
    expect(body).toContain("12px");
  });

  it("resets the clearance to the base gutter on mobile", () => {
    const selector = ':root[data-window-chrome="default"] .island-section-header';
    const mobileResets = ruleSelectorIndices(css, selector)
      .map((_, occurrence) => ({
        occurrence,
        braceIndex: openingBraceAt(css, selector, occurrence),
      }))
      .filter(({ braceIndex }) =>
        framesAt(scanCss, braceIndex).some((frame) => MOBILE_MEDIA_PRELUDE.test(frame.prelude)),
      );
    expect(mobileResets.length, "mobile reset rule").toBeGreaterThanOrEqual(1);
    for (const reset of mobileResets) {
      // The clearance rule it overrides is unlayered, so the reset must be too.
      const enclosing = framesAt(scanCss, reset.braceIndex);
      expect(
        enclosing.some((frame) => frame.isLayer),
        "mobile reset must be unlayered",
      ).toBe(false);
      expect(ruleBody(css, scanCss, selector, reset.occurrence)).toContain("padding-right: 20px");
    }
  });

  it("defines the toggle size variable used by the clearance calc", () => {
    expect(css).toMatch(/--titlebar-toggle-size:\s*\d+px/);
  });
});

describe("overlay paint order", () => {
  it("keeps the fixed chrome layer below the z-50 popover/overlay layer", () => {
    const body = ruleBodyFor(".window-chrome-layer");
    const chromeMatch = /z-index:\s*(\d+)/.exec(body);
    expect(chromeMatch, ".window-chrome-layer z-index").not.toBeNull();
    if (!chromeMatch) throw new Error("missing z-index on .window-chrome-layer");
    const chromeZIndex = Number(chromeMatch[1]);

    // Popover/Select/Tooltip/Sheet/AlertDialog/Toast all render at Tailwind z-50.
    const popoverSource = readFileSync(
      join(dirname(appCssPath), "components", "ui", "popover.tsx"),
      "utf8",
    );
    const overlayMatch = /isolate z-(\d+)/.exec(popoverSource);
    expect(overlayMatch, "popover Positioner z-index").not.toBeNull();
    if (!overlayMatch) throw new Error("missing z-index on the popover Positioner");
    const overlayZIndex = Number(overlayMatch[1]);

    expect(chromeZIndex).toBeLessThan(overlayZIndex);
    // Still above the fixed sidebar island (Tailwind z-10).
    expect(chromeZIndex).toBeGreaterThan(10);
  });
});

describe("dead selectors and undefined variables", () => {
  it("does not reference removed panels", () => {
    expect(css).not.toContain(".reviews-panel");
    expect(css).not.toContain(".ai-history-runs");
  });

  it("does not use undefined theme variables", () => {
    expect(css).not.toContain("var(--foreground)");
    expect(css).not.toContain("var(--muted-foreground)");
  });

  it("keeps the dark native color scheme", () => {
    expect(css).toContain("color-scheme: dark");
  });
});

// jsdom's CSSOM does not implement cascade layers (`@layer`) and Vitest runs with
// CSS processing disabled, so `getComputedStyle` cannot prove that the unlayered
// flatten rules beat Card's Tailwind utilities. The brace-order checks above are
// the reliable guard; a computed-style assertion would need a real CSS engine.
describe.skip("computed island flattening (needs a real CSS engine)", () => {
  it("renders a nested island panel Card without its own border", () => {
    // Intentionally not implemented in jsdom; see comment above.
  });
});

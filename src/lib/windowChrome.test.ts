import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const { isTauri } = vi.hoisted(() => ({ isTauri: vi.fn(() => false) }));
vi.mock("@tauri-apps/api/core", () => ({ isTauri }));

import { resolveWindowChrome, usesMacOverlayTitlebar, type WindowChrome } from "./windowChrome";

/**
 * jsdom exposes `platform` and `userAgent` as prototype getters, so own
 * configurable properties shadow them for the duration of a test and deleting
 * the own property restores the real values.
 */
function setNavigatorIdentity(platform: string, userAgent: string) {
  Object.defineProperty(navigator, "platform", { configurable: true, value: platform });
  Object.defineProperty(navigator, "userAgent", { configurable: true, value: userAgent });
}

type ChromeCase = {
  name: string;
  platform: string;
  userAgent: string;
  tauri: boolean;
  overlay: boolean;
  chrome: WindowChrome;
};

const chromeCases: ChromeCase[] = [
  {
    name: "macOS inside Tauri uses the overlay titlebar",
    platform: "MacIntel",
    userAgent:
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15",
    tauri: true,
    overlay: true,
    chrome: "mac-overlay",
  },
  {
    name: "macOS in the browser preview keeps the default frame",
    platform: "MacIntel",
    userAgent:
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    tauri: false,
    overlay: false,
    chrome: "default",
  },
  {
    name: "Windows 11 inside Tauri keeps the default frame",
    platform: "Win32",
    userAgent:
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    tauri: true,
    overlay: false,
    chrome: "default",
  },
  {
    name: "Linux inside Tauri keeps the default frame",
    platform: "Linux x86_64",
    userAgent:
      "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    tauri: true,
    overlay: false,
    chrome: "default",
  },
  {
    name: "iOS inside Tauri keeps the default frame",
    platform: "iPhone",
    userAgent:
      "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1",
    tauri: true,
    overlay: false,
    chrome: "default",
  },
];

describe("window chrome detection", () => {
  beforeEach(() => {
    isTauri.mockReset();
    isTauri.mockReturnValue(false);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    Reflect.deleteProperty(navigator, "platform");
    Reflect.deleteProperty(navigator, "userAgent");
  });

  it.each(chromeCases)("$name", ({ platform, userAgent, tauri, overlay, chrome }) => {
    setNavigatorIdentity(platform, userAgent);
    isTauri.mockReturnValue(tauri);

    expect(usesMacOverlayTitlebar()).toBe(overlay);
    expect(resolveWindowChrome()).toBe(chrome);
  });

  it("treats a desktop Macintosh user agent as Mac even when the platform is not", () => {
    setNavigatorIdentity(
      "Win32",
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15",
    );
    isTauri.mockReturnValue(true);

    expect(usesMacOverlayTitlebar()).toBe(true);
    expect(resolveWindowChrome()).toBe("mac-overlay");
  });

  it("does not treat an iOS user agent as Mac when the platform is not Mac", () => {
    setNavigatorIdentity(
      "Win32",
      "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1",
    );
    isTauri.mockReturnValue(true);

    expect(usesMacOverlayTitlebar()).toBe(false);
    expect(resolveWindowChrome()).toBe("default");
  });

  it("never uses the overlay when navigator is unavailable", () => {
    vi.stubGlobal("navigator", undefined);
    isTauri.mockReturnValue(true);

    expect(usesMacOverlayTitlebar()).toBe(false);
    expect(resolveWindowChrome()).toBe("default");
  });
});

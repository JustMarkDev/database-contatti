import { isTauri } from "@tauri-apps/api/core";

export type WindowChrome = "mac-overlay" | "default";

/**
 * Desktop Macs only (`MacIntel` / `Macintosh`). iPhone-portrait iOS reports
 * `iPhone`/`iPod` (and iPad without desktop mode reports `iPad`), so narrowing
 * the pattern here keeps iOS on the default frame instead of the mac overlay.
 * Note: iPadOS with "Request Desktop Website" reports `MacIntel` + a
 * `Macintosh` UA and is indistinguishable from a desktop Mac by these two
 * signals alone; it stays on `default` in practice because `isTauri()` is
 * false in the browser and this desktop app has no iOS target. A touch check
 * (`navigator.maxTouchPoints`) would be needed if iOS ever matters.
 */
const MAC_PLATFORM = /Mac/;
/**
 * Desktop Mac browser user agents all contain `Macintosh`. The bare
 * `Mac OS X` token is not used because iOS UAs embed it too
 * (`... iPhone OS 17_0 like Mac OS X ...`), which would falsely select the
 * overlay on iPhones and iPads.
 */
const MAC_USER_AGENT = /Macintosh/;

/**
 * True only when the native macOS traffic lights overlay the webview.
 *
 * Both conditions are required: the overlay title bar is configured on macOS
 * alone, so Windows, iOS, and the plain browser preview must keep the native
 * frame and the default (non-overlay) 14px island inset.
 */
export function usesMacOverlayTitlebar() {
  if (typeof navigator === "undefined") return false;
  const isMac = MAC_PLATFORM.test(navigator.platform) || MAC_USER_AGENT.test(navigator.userAgent);
  return isMac && isTauri();
}

/** CSS dataset value consumed by `:root[data-window-chrome]` rules. */
export function resolveWindowChrome(): WindowChrome {
  return usesMacOverlayTitlebar() ? "mac-overlay" : "default";
}

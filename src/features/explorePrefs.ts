const KEY = "explore_prefs";
/** Pages read per site, the home included: each one is a context.dev credit. */
export const PAGES_PER_SITE_OPTIONS = [1, 2, 3, 4, 5, 6];
/** PDFs per school site (role circolari): each one is at least a credit. */
export const SCHOOL_PDF_OPTIONS = [0, 1, 2];

export type ExplorePrefs = { pagesPerSite: number; schoolPdfs: number };

const DEFAULTS: ExplorePrefs = { pagesPerSite: 3, schoolPdfs: 0 };

export function loadExplorePrefs(): ExplorePrefs {
  try {
    const parsed = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return {
      pagesPerSite: PAGES_PER_SITE_OPTIONS.includes(parsed.pagesPerSite)
        ? parsed.pagesPerSite
        : DEFAULTS.pagesPerSite,
      schoolPdfs: SCHOOL_PDF_OPTIONS.includes(parsed.schoolPdfs)
        ? parsed.schoolPdfs
        : DEFAULTS.schoolPdfs,
    };
  } catch {
    return DEFAULTS;
  }
}

export function saveExplorePrefs(prefs: ExplorePrefs) {
  localStorage.setItem(KEY, JSON.stringify(prefs));
}

import type { AiStartRequest } from "../types";

const KEY = "opencode_prefs";
// OpenCode cannot list a model's variants, so these are the known effort levels.
export const EFFORT_LEVELS = ["low", "medium", "high", "xhigh", "max"];

export type OpenCodePrefs = { model: string | null; effort: string | null };

export function loadOpenCodePrefs(): OpenCodePrefs {
  try {
    const parsed = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return { model: parsed.model || null, effort: parsed.effort || null };
  } catch {
    return { model: null, effort: null };
  }
}

export function saveOpenCodePrefs(prefs: OpenCodePrefs) {
  localStorage.setItem(KEY, JSON.stringify(prefs));
}

/** Applies the user's OpenCode choice on top of the default cascade request. */
export function withOpenCodePrefs(request: AiStartRequest): AiStartRequest {
  const { model, effort } = loadOpenCodePrefs();
  return {
    ...request,
    ...(model && { escalationModel: model }),
    ...(effort && { escalationEffort: effort }),
  };
}

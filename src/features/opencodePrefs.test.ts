import { beforeEach, expect, test } from "vite-plus/test";
import { aiChoice, saveOpenCodePrefs } from "./opencodePrefs";

beforeEach(() => localStorage.clear());

test("aiChoice sends only what Settings has saved", () => {
  expect(aiChoice()).toEqual({});
  saveOpenCodePrefs({ model: "opencode-go/muse-spark-1.3-contributor", effort: "high" });
  expect(aiChoice()).toEqual({
    aiModel: "opencode-go/muse-spark-1.3-contributor",
    aiEffort: "high",
  });
  saveOpenCodePrefs({ model: null, effort: null });
  expect(aiChoice()).toEqual({});
});

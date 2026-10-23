import { describe, expect, it } from "vite-plus/test";
import { contactCategories, emptyFilters } from "./filters";

describe("contact filter feature boundary", () => {
  it("creates independent filter state for each page lifecycle", () => {
    const first = emptyFilters();
    const second = emptyFilters();
    first.category.include.push(contactCategories[0]);
    expect(second.category.include).toEqual([]);
    expect(Object.keys(first)).toHaveLength(15);
  });
});

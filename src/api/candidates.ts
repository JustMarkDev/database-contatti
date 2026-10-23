import { invoke } from "@tauri-apps/api/core";
import type { CandidateFilters, CandidateList } from "../types";

const PAGE_SIZE = 100;

export const listCandidates = (filters: CandidateFilters, offset: number) =>
  invoke<CandidateList>("list_candidates", { filters, limit: PAGE_SIZE, offset });

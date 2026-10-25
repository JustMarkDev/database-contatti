import { invoke } from "@tauri-apps/api/core";
import type {
  DiscoveryReport,
  LeadFilterOptions,
  LeadFilters,
  LeadPlan,
  LeadSource,
  SearchLeadList,
  SearchRunSummary,
  SearchRunView,
  SeedListView,
  TypeSafeKeyView,
} from "../types";

/**
 * Cerca lead API.
 *
 * Every import, directory, discovery and crawl writes to the sandbox
 * (`search_leads`, `search_seeds`). Promoting leads into `contacts` is a
 * separate command. Sites are read through context.dev.
 */

export type StartSearchRunInput = {
  query: string;
  seedUrls: string[];
  maxPagesPerDomain: number;
  maxPdfs: number;
  useAi: boolean;
};

export type ExploreRequest = {
  listName: string | null;
  /** Only seeds of this taxonomy and region (from a free-form request). */
  category?: string | null;
  subcategory?: string | null;
  regione?: string | null;
  limit: number;
  maxPages: number;
  maxPdfs: number;
  useAi: boolean;
};

export type DirectoryRequest = {
  sourceId?: string;
  url?: string;
  name?: string;
  category?: string | null;
  subcategory?: string | null;
  maxPages?: number;
  useAi: boolean;
};

export type SearchExportReport = { path: string; rows_written: number };

export type PromoteSearchReport = {
  added: number;
  updated: number;
  skipped_blacklist: number;
  unverified: number;
};

export const startSearchRun = (input: StartSearchRunInput) =>
  invoke<SearchRunSummary>("start_search_run", input);

export const listSearchLeads = (filters: LeadFilters, offset: number) =>
  invoke<SearchLeadList>("list_search_leads", { filters, limit: 100, offset });

/** Facet counts over the runs of the given kinds (every run when empty). */
export const getLeadFilterOptions = (kinds: string[]) =>
  invoke<LeadFilterOptions>("get_lead_filter_options", { kinds });

export const listSearchRuns = () => invoke<SearchRunView[]>("list_search_runs");

export const deleteSearchRun = (runId: string) => invoke<void>("delete_search_run", { runId });

export const listLeadSources = () => invoke<LeadSource[]>("list_lead_sources");

/** Downloads one open-data source into a new sandbox run (sites without email become seeds). */
export const importLeadSource = (sourceId: string) =>
  invoke<SearchRunSummary>("import_lead_source", { sourceId });

export const listSeedLists = () => invoke<SeedListView[]>("list_seed_lists");

export const exploreSeeds = (request: ExploreRequest) =>
  invoke<SearchRunSummary>("explore_seeds", { request });

export const exploreDirectory = (request: DirectoryRequest) =>
  invoke<SearchRunSummary>("explore_directory", { request });

export const cancelLeadCrawl = () => invoke<void>("cancel_lead_crawl");

export const getContextdevApiKey = () => invoke<TypeSafeKeyView>("get_contextdev_api_key");

export const setContextdevApiKey = (apiKey: string) =>
  invoke<TypeSafeKeyView>("set_contextdev_api_key", { apiKey });

export type SearchDiscoveryRequest = {
  query?: string | null;
  category?: string | null;
  subcategory?: string | null;
  regione?: string | null;
  /** Searches at most (one context.dev credit each); the backend default when omitted. */
  maxQueries?: number;
  maxCredits?: number;
};

/** context.dev web search: new sites, filtered and classified, become seeds in "Scoperta: ricerca web". */
export const discoverBySearch = (request: SearchDiscoveryRequest) =>
  invoke<DiscoveryReport>("discover_by_search", { request });

/** Finds the site of organizations known only by name; returns how many can now be read. */
export const findSeedSites = (listName: string | null, limit: number) =>
  invoke<number>("find_seed_sites", { listName, limit });

export const exportSearchLeadsTxt = (filters: LeadFilters, path: string) =>
  invoke<SearchExportReport>("export_search_leads_txt", { filters, path });

/** Merge the filtered sandbox leads into the contacts database. */
export const promoteSearchLeads = (filters: LeadFilters) =>
  invoke<PromoteSearchReport>("promote_search_leads", { filters });

/** The IA reads a free-form request into taxonomy, region, catalog sources and a web search. */
export const planLeadRequest = (request: string) =>
  invoke<LeadPlan>("plan_lead_request", { request });

import { invoke } from "@tauri-apps/api/core";
import type { BlacklistList, BlacklistReport, ExportReport } from "../types";

export const listBlacklist = (query: string, offset: number) =>
  invoke<BlacklistList>("list_blacklist", { query, limit: 100, offset });
export const importBlacklistPaths = (paths: string[]) =>
  invoke<BlacklistReport>("import_blacklist_paths", { paths });
export const exportBlacklistCsv = (query: string, path: string) =>
  invoke<ExportReport>("export_blacklist_csv", { query, path });
export const exportBlacklistEmailsCsv = (query: string, path: string) =>
  invoke<ExportReport>("export_blacklist_emails_csv", { query, path });
export const addBlacklistEntry = (email: string, note?: string) =>
  invoke<boolean>("add_blacklist_entry", { email, note });
export const removeBlacklistEntry = (email: string) => invoke("remove_blacklist_entry", { email });
export const clearBlacklist = () => invoke("clear_blacklist");

import { invoke } from "@tauri-apps/api/core";
import type {
  ContactList,
  DashboardData,
  ExportReport,
  FilterOptions,
  Filters,
  ImportReport,
} from "../types";

export const refreshDashboard = (filters: Filters, includeFacets = true) =>
  invoke<DashboardData>("refresh_dashboard", {
    filters,
    limit: 100,
    offset: 0,
    includeFacets,
  });
export const getFilterOptions = () => invoke<FilterOptions>("get_filter_options");
export const listContacts = (filters: Filters, offset: number, includeTotal = true) =>
  invoke<ContactList>("list_contacts", {
    filters,
    limit: 100,
    offset,
    includeTotal,
  });
export const importContactPath = (path: string) =>
  invoke<ImportReport>("import_contact_path", { path, extraCategory: "" });
export const importContactPaths = (paths: string[]) =>
  invoke<ImportReport>("import_contact_paths", { paths, extraCategory: "" });
export const controlImport = (action: "pause" | "resume" | "cancel") =>
  invoke<void>("control_import", { action });
export const exportContactsCsv = (
  filters: Filters,
  path: string,
  includeUntrustedClassifications: boolean,
) =>
  invoke<ExportReport>("export_contacts_csv", {
    filters,
    path,
    includeUntrustedClassifications,
  });
export const exportContactEmailsCsv = (
  filters: Filters,
  path: string,
  includeUntrustedClassifications: boolean,
) =>
  invoke<ExportReport>("export_contact_emails_csv", {
    filters,
    path,
    includeUntrustedClassifications,
  });
export const deleteContact = (id: string) => invoke("delete_contact", { id });
export const clearContacts = () => invoke("clear_database");

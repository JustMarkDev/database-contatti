import { invoke } from "@tauri-apps/api/core";
import type { DatabaseBackupReport, DatabaseRestoreReport } from "../types";

export const exportDatabaseBackup = (destinationPath: string) =>
  invoke<DatabaseBackupReport>("export_database_backup", { destinationPath });

export const importDatabaseBackup = (sourcePath: string) =>
  invoke<DatabaseRestoreReport>("import_database_backup", { sourcePath });

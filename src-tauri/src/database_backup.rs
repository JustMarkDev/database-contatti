use crate::database::{open_db, reset_database_runtime};
use crate::schema::migrate_database;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use tauri::Manager;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// A real database is far below this; it only stops a zip bomb from filling the disk.
const MAX_RESTORE_BYTES: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BackupReport {
    pub path: String,
    pub size_bytes: u64,
    pub contact_count: usize,
    pub blacklist_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RestoreReport {
    pub contact_count: usize,
    pub blacklist_count: usize,
    pub candidates_count: usize,
}

#[tauri::command]
pub(crate) async fn export_database_backup(
    app: tauri::AppHandle,
    destination_path: String,
) -> Result<BackupReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = data_dir(&app)?;
        let db = open_db(&app)?;
        export_database_backup_impl(&db, &dir, &destination_path)
    })
    .await
    .map_err(|e| format!("Esportazione interrotta: {e}"))?
}

#[tauri::command]
pub(crate) async fn import_database_backup(
    app: tauri::AppHandle,
    source_path: String,
) -> Result<RestoreReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = data_dir(&app)?;
        {
            let db = open_db(&app)?;
            if let Some(busy) = operation_in_progress(&db)? {
                return Err(format!(
                    "Impossibile ripristinare mentre è in corso: {busy}. Attendere o annullare l'operazione."
                ));
            }
        }
        // Drop the pool so no connection keeps the file being replaced open.
        reset_database_runtime();
        let result = import_database_backup_impl(&dir, &source_path);
        // Reopen either way: on failure the previous database is still in place.
        let reopened = open_db(&app);
        let report = result?;
        reopened?;
        Ok(report)
    })
    .await
    .map_err(|e| format!("Importazione interrotta: {e}"))?
}

fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|err| format!("Cartella dati non disponibile: {err}"))
}

/// Names the first long-running job that would write into the file a restore replaces.
fn operation_in_progress(db: &Connection) -> Result<Option<&'static str>, String> {
    if crate::imports::import_running() {
        return Ok(Some("un'importazione"));
    }
    if crate::lead_crawl::crawl_running() {
        return Ok(Some("una ricerca lead"));
    }
    let active = |sql: &str| -> Result<bool, String> {
        db.query_row(sql, [], |row| row.get::<_, bool>(0))
            .map_err(|e| e.to_string())
    };
    if active(
        "SELECT EXISTS(SELECT 1 FROM ai_classification_runs WHERE status IN ('running','pausing'))",
    )? {
        return Ok(Some("un'esecuzione IA"));
    }
    if active(
        "SELECT EXISTS(SELECT 1 FROM mailbox_verification_jobs WHERE status IN ('preparing','running'))",
    )? {
        return Ok(Some("una verifica caselle"));
    }
    Ok(None)
}

/// API keys live in `app_settings`. A backup is a file people copy and send around, so
/// it never carries them; a restore keeps the keys of the database it replaces.
const SECRET_SETTINGS: [&str; 2] = [
    crate::ai_typesafe::TYPESAFE_SETTINGS_KEY,
    crate::tinyfish::SETTINGS_KEY,
];

fn strip_secrets(path: &Path) -> Result<(), String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    for key in SECRET_SETTINGS {
        conn.execute("DELETE FROM app_settings WHERE key=?1", [key])
            .map_err(|e| format!("Impossibile rimuovere le chiavi API dal backup: {e}"))?;
    }
    Ok(())
}

fn carry_over_secrets(from_live: &Path, into: &Path) {
    let Ok(old) =
        Connection::open_with_flags(from_live, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return;
    };
    let Ok(new) = Connection::open(into) else {
        return;
    };
    for key in SECRET_SETTINGS {
        let row = old.query_row(
            "SELECT value,updated_at FROM app_settings WHERE key=?1",
            [key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        if let Ok((value, updated_at)) = row {
            let _ = new.execute(
                "INSERT INTO app_settings(key,value,updated_at) VALUES(?1,?2,?3)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
                rusqlite::params![key, value, updated_at],
            );
        }
    }
}

fn is_zip_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

fn count(db: &Connection, sql: &str) -> usize {
    db.query_row(sql, [], |row| row.get::<_, i64>(0))
        .map_or(0, |value| value as usize)
}

/// Writes next to `dest` and renames, so a failed export never destroys the previous backup.
pub(crate) fn export_database_backup_impl(
    db: &Connection,
    dir: &Path,
    destination_path: &str,
) -> Result<BackupReport, String> {
    let dest_path = PathBuf::from(destination_path);
    let parent = match dest_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent)
        .map_err(|e| format!("Impossibile creare la cartella di destinazione: {e}"))?;
    let name = dest_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "Nome file di esportazione non valido".to_owned())?;
    let partial = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));

    let contact_count = count(
        db,
        "SELECT COUNT(*) FROM contacts WHERE status='valid' AND coalesce(suppression_status,'active')='active'",
    );
    let blacklist_count = count(db, "SELECT COUNT(*) FROM blacklist");

    let written = if is_zip_path(&dest_path) {
        write_zip_backup(db, dir, &partial)
    } else {
        db.execute(
            "VACUUM INTO ?1",
            rusqlite::params![partial.to_string_lossy().to_string()],
        )
        .map_err(|e| format!("Errore durante l'esportazione diretta del database: {e}"))
        .and_then(|_| strip_secrets(&partial))
    };
    let published = written.and_then(|()| {
        std::fs::rename(&partial, &dest_path)
            .map_err(|e| format!("Impossibile scrivere il file di destinazione: {e}"))
    });
    if let Err(error) = published {
        let _ = std::fs::remove_file(&partial);
        return Err(error);
    }

    let size_bytes = std::fs::metadata(&dest_path).map(|m| m.len()).unwrap_or(0);
    Ok(BackupReport {
        path: dest_path.to_string_lossy().to_string(),
        size_bytes,
        contact_count,
        blacklist_count,
    })
}

fn write_zip_backup(db: &Connection, dir: &Path, zip_path: &Path) -> Result<(), String> {
    let snapshot = dir.join(format!("backup_temp_{}.sqlite3", uuid::Uuid::new_v4()));
    let result = (|| {
        db.execute(
            "VACUUM INTO ?1",
            rusqlite::params![snapshot.to_string_lossy().to_string()],
        )
        .map_err(|e| format!("Errore durante la creazione dello snapshot del database: {e}"))?;
        strip_secrets(&snapshot)?;
        let zip_file = File::create(zip_path)
            .map_err(|e| format!("Impossibile creare il file zip {zip_path:?}: {e}"))?;
        let mut zip = ZipWriter::new(BufWriter::new(zip_file));
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .large_file(true);
        zip.start_file("contatti.sqlite3", options)
            .map_err(|e| format!("Errore scrittura zip: {e}"))?;
        let mut source = BufReader::with_capacity(
            1024 * 1024,
            File::open(&snapshot)
                .map_err(|e| format!("Impossibile leggere file temporaneo: {e}"))?,
        );
        std::io::copy(&mut source, &mut zip)
            .map_err(|e| format!("Errore compressione zip: {e}"))?;
        zip.finish()
            .map_err(|e| format!("Errore finalizzazione archivio zip: {e}"))?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&snapshot);
    result
}

fn extract_database(source_path: &Path, target: &Path) -> Result<(), String> {
    if !is_zip_path(source_path) {
        return std::fs::copy(source_path, target)
            .map(|_| ())
            .map_err(|e| format!("Impossibile copiare il database sorgente: {e}"));
    }
    let zip_file =
        File::open(source_path).map_err(|e| format!("Impossibile aprire l'archivio ZIP: {e}"))?;
    let mut archive = ZipArchive::new(BufReader::new(zip_file))
        .map_err(|e| format!("Archivio ZIP non valido: {e}"))?;
    if archive.is_empty() {
        return Err("L'archivio ZIP è vuoto.".to_string());
    }

    let mut target_index = 0;
    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            let name = file.name().to_lowercase();
            if name == "contatti.sqlite3" || name.ends_with("/contatti.sqlite3") {
                target_index = i;
                break;
            }
            if name.ends_with(".sqlite3") || name.ends_with(".db") {
                target_index = i;
            }
        }
    }

    let file_in_zip = archive
        .by_index(target_index)
        .map_err(|e| format!("Impossibile leggere il file dall'archivio: {e}"))?;
    let mut out_file =
        File::create(target).map_err(|e| format!("Impossibile estrarre il database: {e}"))?;
    let written = std::io::copy(&mut file_in_zip.take(MAX_RESTORE_BYTES + 1), &mut out_file)
        .map_err(|e| format!("Errore durante l'estrazione: {e}"))?;
    if written > MAX_RESTORE_BYTES {
        return Err("Il database nell'archivio supera il limite di dimensione.".to_owned());
    }
    Ok(())
}

fn verify_database(path: &Path) -> Result<RestoreReport, String> {
    let mut conn = Connection::open(path)
        .map_err(|e| format!("Impossibile aprire il file di database ripristinato: {e}"))?;
    let check: String = conn
        .query_row("PRAGMA quick_check(1);", [], |row| row.get(0))
        .map_err(|e| format!("Controllo integrità fallito: {e}"))?;
    if !check.eq_ignore_ascii_case("ok") {
        return Err(format!("Database corrotto: {check}"));
    }
    migrate_database(&mut conn, Some(path), false).map_err(|e| {
        format!("Impossibile applicare le migrazioni al database ripristinato: {e}")
    })?;
    Ok(RestoreReport {
        contact_count: count(
            &conn,
            "SELECT COUNT(*) FROM contacts WHERE status='valid' AND coalesce(suppression_status,'active')='active'",
        ),
        blacklist_count: count(&conn, "SELECT COUNT(*) FROM blacklist"),
        candidates_count: count(&conn, "SELECT COUNT(*) FROM email_candidates"),
    })
}

/// The live file and its WAL/SHM. The WAL must move with it: a stale one next to a
/// different database file would corrupt it.
const LIVE_SUFFIXES: [&str; 3] = ["", "-wal", "-shm"];

fn live_path(dir: &Path, suffix: &str, previous: bool) -> PathBuf {
    let tag = if previous { ".pre-restore" } else { "" };
    dir.join(format!("contatti.sqlite3{tag}{suffix}"))
}

/// Replaces the live database with the verified copy, keeping the old one as
/// `contatti.sqlite3.pre-restore` (and puts it back if the swap fails).
fn swap_live_database(dir: &Path, verified: &Path) -> Result<(), String> {
    for suffix in LIVE_SUFFIXES {
        let _ = std::fs::remove_file(live_path(dir, suffix, true));
    }
    for suffix in LIVE_SUFFIXES {
        let live = live_path(dir, suffix, false);
        if live.exists() {
            std::fs::rename(&live, live_path(dir, suffix, true))
                .map_err(|e| format!("Impossibile conservare il database attivo: {e}"))?;
        }
    }
    if let Err(error) = std::fs::rename(verified, live_path(dir, "", false)) {
        for suffix in LIVE_SUFFIXES {
            let previous = live_path(dir, suffix, true);
            if previous.exists() {
                let _ = std::fs::rename(previous, live_path(dir, suffix, false));
            }
        }
        return Err(format!(
            "Impossibile sostituire il database attivo: {error}"
        ));
    }
    Ok(())
}

/// The caller must have closed every connection to the live database.
pub(crate) fn import_database_backup_impl(
    dir: &Path,
    source_path_str: &str,
) -> Result<RestoreReport, String> {
    let source_path = PathBuf::from(source_path_str);
    if !source_path.exists() {
        return Err(format!(
            "Il file specificato non esiste: {}",
            source_path.display()
        ));
    }
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("Impossibile creare cartella dati {dir:?}: {err}"))?;

    let temp_import_path = dir.join(format!("import_temp_{}.sqlite3", uuid::Uuid::new_v4()));
    let outcome = extract_database(&source_path, &temp_import_path)
        .and_then(|()| verify_database(&temp_import_path))
        .and_then(|report| {
            carry_over_secrets(&live_path(dir, "", false), &temp_import_path);
            swap_live_database(dir, &temp_import_path).map(|()| report)
        });
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temp_import_path);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_sample_db(path: &Path, email: &str) {
        let mut conn = Connection::open(path).expect("open db");
        migrate_database(&mut conn, Some(path), false).expect("migrate");
        conn.execute(
            "INSERT INTO contacts (
                id, email, email_local_part, email_domain,
                email_tld, status, validation_notes, source_file, category, contact_kind, paese,
                classification_method, classification_confidence
             ) VALUES (
                'test-id-1', ?1,
                'mario.rossi', 'example.test', '.test', 'valid', '[]', 'test.csv', 'Altro', 'ente_pubblico',
                'Italia', 'rules', 1.0
             )",
            [email],
        )
        .expect("insert contact");
        conn.execute(
            "INSERT INTO blacklist (
                normalized_email, source_file, imported_at, note
             ) VALUES (
                'spam@example.test', 'blacklist.csv', '2026-01-01T00:00:00Z', 'test'
             )",
            [],
        )
        .expect("insert blacklist");
    }

    fn work_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("test_{tag}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create work_dir");
        dir
    }

    fn email_in(path: &Path) -> String {
        Connection::open(path)
            .expect("open")
            .query_row("SELECT email FROM contacts", [], |r| r.get(0))
            .expect("email")
    }

    #[test]
    fn zip_export_and_restore_round_trip_keeps_the_previous_database() {
        let source = work_dir("backup_source");
        let live = work_dir("backup_live");
        create_sample_db(&source.join("contatti.sqlite3"), "nuovo@example.test");
        create_sample_db(&live.join("contatti.sqlite3"), "vecchio@example.test");

        let db = Connection::open(source.join("contatti.sqlite3")).expect("open");
        let zip = source.join("backup.zip");
        let report =
            export_database_backup_impl(&db, &source, zip.to_str().unwrap()).expect("export");
        assert_eq!((report.contact_count, report.blacklist_count), (1, 1));
        drop(db);
        assert_eq!(
            std::fs::read_dir(&source).unwrap().count(),
            2,
            "no temp files left"
        );

        let restored = import_database_backup_impl(&live, zip.to_str().unwrap()).expect("restore");
        assert_eq!(restored.contact_count, 1);
        assert_eq!(
            email_in(&live.join("contatti.sqlite3")),
            "nuovo@example.test"
        );
        assert_eq!(
            email_in(&live.join("contatti.sqlite3.pre-restore")),
            "vecchio@example.test"
        );
    }

    #[test]
    fn backups_carry_no_api_keys_and_restore_keeps_the_live_ones() {
        let source = work_dir("secrets_source");
        let live = work_dir("secrets_live");
        for (dir, key) in [(&source, "chiave-sorgente"), (&live, "chiave-attiva")] {
            let path = dir.join("contatti.sqlite3");
            create_sample_db(&path, "a@example.test");
            Connection::open(&path)
                .unwrap()
                .execute(
                    "INSERT INTO app_settings(key,value,updated_at) VALUES(?1,?2,1)",
                    rusqlite::params![crate::ai_typesafe::TYPESAFE_SETTINGS_KEY, key],
                )
                .unwrap();
        }
        let db = Connection::open(source.join("contatti.sqlite3")).unwrap();
        let backup = source.join("backup.sqlite3");
        export_database_backup_impl(&db, &source, backup.to_str().unwrap()).unwrap();
        let in_backup: i64 = Connection::open(&backup)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM app_settings WHERE key=?1",
                [crate::ai_typesafe::TYPESAFE_SETTINGS_KEY],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(in_backup, 0);

        import_database_backup_impl(&live, backup.to_str().unwrap()).unwrap();
        let kept: String = Connection::open(live.join("contatti.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT value FROM app_settings WHERE key=?1",
                [crate::ai_typesafe::TYPESAFE_SETTINGS_KEY],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, "chiave-attiva");
    }

    #[test]
    fn corrupt_restore_leaves_live_database_untouched() {
        let live = work_dir("corrupt_live");
        create_sample_db(&live.join("contatti.sqlite3"), "vecchio@example.test");
        let fake = live.join("fake.sqlite3");
        std::fs::write(&fake, b"This is not a sqlite database file").expect("write fake");

        let error = import_database_backup_impl(&live, fake.to_str().unwrap()).unwrap_err();
        assert!(!error.is_empty());
        assert_eq!(
            email_in(&live.join("contatti.sqlite3")),
            "vecchio@example.test"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&live)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("import_temp_"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn export_replaces_an_existing_backup_without_leftovers() {
        let dir = work_dir("export_replace");
        create_sample_db(&dir.join("contatti.sqlite3"), "a@example.test");
        let db = Connection::open(dir.join("contatti.sqlite3")).expect("open");
        let dest = dir.join("backup.sqlite3");
        std::fs::write(&dest, b"previous backup").expect("seed");
        export_database_backup_impl(&db, &dir, dest.to_str().unwrap()).expect("export");
        assert_eq!(email_in(&dest), "a@example.test");
        let temp: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .collect();
        assert!(temp.is_empty());
    }
}

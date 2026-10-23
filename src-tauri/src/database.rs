use crate::{
    AI_LEASE_SECONDS, APP_SESSION_ID, blacklist::consolidate_legacy_blacklist,
    schema::migrate_database,
};
use rusqlite::{Connection, TransactionBehavior, params};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use tauri::Manager;

const DB_POOL_SIZE: usize = 4;
const DB_CHECKOUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Runs blocking database or file work on the blocking pool, off the UI thread
/// and the async workers. Every command that touches SQLite or disk goes through it.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| format!("Operazione interrotta: {error}"))?
}

struct DatabasePool {
    available: Mutex<Vec<Connection>>,
    returned: Condvar,
}

pub(crate) struct PooledConnection {
    pool: Arc<DatabasePool>,
    connection: Option<Connection>,
}

impl Deref for PooledConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.connection
            .as_ref()
            .expect("pooled database connection must be checked out")
    }
}

impl DerefMut for PooledConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection
            .as_mut()
            .expect("pooled database connection must be checked out")
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            let mut available = self.pool.available.lock().expect("database pool poisoned");
            available.push(connection);
            self.pool.returned.notify_one();
        }
    }
}

impl DatabasePool {
    fn checkout(self: &Arc<Self>) -> Result<PooledConnection, String> {
        let available = self.available.lock().expect("database pool poisoned");
        let (mut available, timeout) = self
            .returned
            .wait_timeout_while(available, DB_CHECKOUT_TIMEOUT, |available| {
                available.is_empty()
            })
            .expect("database pool poisoned");
        if timeout.timed_out() {
            return Err("Database occupato da altre operazioni: riprovare tra poco".to_owned());
        }
        Ok(PooledConnection {
            pool: Arc::clone(self),
            connection: available.pop(),
        })
    }
}

struct DatabaseRuntime {
    path: PathBuf,
    pool: Arc<DatabasePool>,
}

static DATABASE_RUNTIME: OnceLock<Mutex<Option<Arc<DatabaseRuntime>>>> = OnceLock::new();

fn runtime_slot() -> &'static Mutex<Option<Arc<DatabaseRuntime>>> {
    DATABASE_RUNTIME.get_or_init(|| Mutex::new(None))
}

fn configure_connection(db: &mut Connection, set_wal: bool) -> Result<(), String> {
    db.busy_timeout(std::time::Duration::from_secs(15))
        .map_err(|err| err.to_string())?;
    let pragmas = if set_wal {
        "PRAGMA foreign_keys=ON;
         PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size=-65536;
         PRAGMA mmap_size=268435456;"
    } else {
        "PRAGMA foreign_keys=ON;
         PRAGMA synchronous=NORMAL;
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size=-65536;
         PRAGMA mmap_size=268435456;"
    };
    db.execute_batch(pragmas).map_err(|err| err.to_string())
}

fn initialize_runtime(
    path: &PathBuf,
    dir: &std::path::Path,
) -> Result<Arc<DatabaseRuntime>, String> {
    println!("[db] Inizializzazione database SQLite: {}", path.display());
    let mut primary = Connection::open(path).map_err(|err| err.to_string())?;
    configure_connection(&mut primary, true)?;
    migrate_database(&mut primary, Some(path), false)?;
    let legacy_blacklist_path = dir.join("blacklist.sqlite3");
    consolidate_legacy_blacklist(&mut primary, &legacy_blacklist_path, false)?;
    normalize_orphaned_ai_runs(&primary, chrono::Utc::now().timestamp())?;
    crate::mailbox_verification::normalize_jobs_on_startup(&primary)?;
    // Freshness moves with time: rescore once a day. A failure must not block startup.
    match crate::schema::refresh_scores_if_due(&primary, chrono::Utc::now().timestamp()) {
        Ok(Some(changed)) => println!("[db] Punteggi contatti aggiornati: {changed}"),
        Ok(None) => {}
        Err(error) => eprintln!("[db] Ricalcolo punteggi fallito: {error}"),
    }

    let mut connections = vec![primary];
    for _ in 1..DB_POOL_SIZE {
        let mut reader = Connection::open(path).map_err(|err| err.to_string())?;
        configure_connection(&mut reader, false)?;
        connections.push(reader);
    }
    let pool = Arc::new(DatabasePool {
        available: Mutex::new(connections),
        returned: Condvar::new(),
    });
    Ok(Arc::new(DatabaseRuntime {
        path: path.clone(),
        pool,
    }))
}

pub(crate) fn open_db(app: &tauri::AppHandle) -> Result<PooledConnection, String> {
    // The data directory never changes while the app runs: skip path resolution
    // and directory creation once the pool exists.
    let ready = runtime_slot()
        .lock()
        .expect("database runtime poisoned")
        .clone();
    if let Some(runtime) = ready {
        return runtime.pool.checkout();
    }
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("Cartella dati non disponibile: {err}"))?;
    std::fs::create_dir_all(&dir).map_err(|err| format!("Impossibile creare {:?}: {err}", dir))?;
    let path: PathBuf = dir.join("contatti.sqlite3");
    let runtime = {
        let mut slot = runtime_slot().lock().expect("database runtime poisoned");
        if let Some(runtime) = slot.as_ref().filter(|runtime| runtime.path == path) {
            Arc::clone(runtime)
        } else {
            let runtime = initialize_runtime(&path, &dir)?;
            *slot = Some(Arc::clone(&runtime));
            runtime
        }
    };
    runtime.pool.checkout()
}

pub(crate) fn reset_database_runtime() {
    if let Some(slot) = DATABASE_RUNTIME.get() {
        *slot.lock().expect("database runtime poisoned") = None;
    }
}

pub(crate) fn normalize_orphaned_ai_runs(db: &Connection, now: i64) -> Result<usize, String> {
    // Reclaim running/pausing work left by a previous process even when its lease
    // is still in the future. Only the current session's live lease stays intact.
    db.execute(
        "UPDATE ai_classification_runs
         SET status='paused',interruption_reason='Esecuzione interrotta dalla chiusura precedente',
             owner_session=NULL,lease_expires_at=NULL
         WHERE status IN ('running','pausing')
           AND (
             owner_session IS NULL
             OR owner_session <> ?2
             OR lease_expires_at IS NULL
             OR lease_expires_at < ?1
           )",
        params![now, APP_SESSION_ID.as_str()],
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn renew_ai_lease(
    db: &Connection,
    run_id: &str,
    session: &str,
    now: i64,
    ttl: i64,
) -> Result<bool, String> {
    db.execute(
        "UPDATE ai_classification_runs SET lease_expires_at=?1
         WHERE id=?2 AND owner_session=?3 AND status IN ('running','pausing')",
        params![now + ttl.max(AI_LEASE_SECONDS), run_id, session],
    )
    .map(|changed| changed == 1)
    .map_err(|e| e.to_string())
}

pub(crate) fn claim_ai_run(db: &mut Connection, run_id: &str, now: i64) -> Result<(), String> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    normalize_orphaned_ai_runs(&tx, now)?;
    let other_active: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM ai_classification_runs
             WHERE id<>?1 AND status IN ('running','pausing') AND lease_expires_at>=?2",
            params![run_id, now],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if other_active != 0 {
        return Err("È già attiva un'altra classificazione IA".to_owned());
    }
    let changed = tx
        .execute(
            "UPDATE ai_classification_runs
             SET status='running',owner_session=?1,lease_expires_at=?2,interruption_reason=NULL
             WHERE id=?3 AND status='paused'",
            params![APP_SESSION_ID.as_str(), now + AI_LEASE_SECONDS, run_id],
        )
        .map_err(|e| e.to_string())?;
    if changed != 1 {
        return Err("L'esecuzione non è in pausa o è già stata presa in carico".to_owned());
    }
    tx.commit().map_err(|e| e.to_string())
}

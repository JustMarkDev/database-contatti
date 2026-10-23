//! Read-only MCP server (JSON-RPC 2.0, one message per line on stdio) so an
//! assistant can query the contacts database. It opens the app's SQLite file
//! read-only and reuses the list/filter code of the UI, so results match the app
//! (blacklisted and suppressed contacts never appear).

use crate::{ListFilters, contacts::facet_options, load_contacts};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;
/// Fields an assistant needs to address and personalize a message.
const MAILING_FIELDS: [&str; 17] = [
    "id",
    "email",
    "organization_name",
    "denomination",
    "person_name",
    "role",
    "department",
    "category",
    "subcategory",
    "contact_kind",
    "comune",
    "provincia",
    "regione",
    "paese",
    "website",
    "is_pec",
    "quality_score",
];
const PROTOCOL_VERSION: &str = "2025-06-18";
/// Must match `identifier` in `tauri.conf.json`: Tauri's `app_data_dir`.
const APP_IDENTIFIER: &str = "com.mark.database-contatti";

fn database_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("DATABASE_CONTATTI_DB") {
        return Ok(path.into());
    }
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
    };
    base.map(|base| base.join(APP_IDENTIFIER).join("contatti.sqlite3"))
        .ok_or_else(|| "Cartella dati non trovata: impostare DATABASE_CONTATTI_DB".to_owned())
}

pub(crate) fn open_read_only(path: &std::path::Path) -> Result<Connection, String> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("Impossibile aprire {}: {error}", path.display()))?;
    db.busy_timeout(std::time::Duration::from_secs(15))
        .map_err(|error| error.to_string())?;
    db.execute_batch("PRAGMA query_only=ON;")
        .map_err(|error| error.to_string())?;
    Ok(db)
}

fn field_filter_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "description": description,
        "properties": {
            "include": {"type": "array", "items": {"type": "string"}},
            "exclude": {"type": "array", "items": {"type": "string"}}
        }
    })
}

fn tools() -> Value {
    let filter = |name: &str| field_filter_schema(&format!("Include/exclude values of `{name}`"));
    json!([
        {
            "name": "search_contacts",
            "description": "Search the email contacts database. Free-text `query` plus optional \
                include/exclude filters; use list_filter_options to see valid values. Only \
                deliverable contacts are returned (valid, not blacklisted or suppressed, \
                not automated mailboxes). Returns one page with the total match count; \
                page with `offset` to collect a whole list.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Free text (email, organization, place...)"},
                    "category": filter("category"),
                    "subcategory": filter("subcategory"),
                    "contact_kind": filter("contact_kind"),
                    "paese": filter("paese"),
                    "macroarea": filter("macroarea"),
                    "regione": filter("regione"),
                    "provincia": filter("provincia"),
                    "comune": filter("comune"),
                    "min_score": {"type": "integer", "description": "Minimum quality score (0-100)"},
                    "limit": {"type": "integer", "description": format!("Page size, default {DEFAULT_LIMIT}, max {MAX_LIMIT}")},
                    "offset": {"type": "integer", "description": "Rows to skip"},
                    "full": {"type": "boolean", "description": "Return every column (classification evidence, dates, flags). Default: only the fields needed to address and personalize a message"}
                }
            }
        },
        {
            "name": "list_filter_options",
            "description": "Distinct values (with contact counts) of one filter field, \
                alphabetical order.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "field": {"type": "string", "enum": [
                        "category", "subcategory", "contact_kind", "paese", "macroarea",
                        "regione", "provincia", "comune", "source_file"
                    ]},
                    "limit": {"type": "integer", "description": "Max values, default 100"}
                },
                "required": ["field"]
            }
        }
    ])
}

fn arg_usize(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .map_or(default, |value| value as usize)
}

fn call_tool(db: &Connection, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "search_contacts" => {
            // Unknown keys (limit, offset) are ignored by `ListFilters`.
            let filters: ListFilters = serde_json::from_value(args.clone())
                .map_err(|error| format!("Argomenti non validi: {error}"))?;
            let limit = arg_usize(args, "limit", DEFAULT_LIMIT).min(MAX_LIMIT);
            let list = load_contacts(db, &filters, limit, arg_usize(args, "offset", 0))?;
            let mut out = serde_json::to_value(list).map_err(|error| error.to_string())?;
            // Most of the ~60 columns are empty on any given contact, and the
            // rest (classification evidence, hashes...) is noise for a mailing.
            let full = args["full"].as_bool().unwrap_or(false);
            for contact in out["contacts"].as_array_mut().into_iter().flatten() {
                if let Some(fields) = contact.as_object_mut() {
                    fields.retain(|key, v| {
                        (full || MAILING_FIELDS.contains(&key.as_str()))
                            && !v.is_null()
                            && v != ""
                            && v != &json!([])
                    });
                }
            }
            Ok(out)
        }
        "list_filter_options" => {
            let field = args["field"].as_str().unwrap_or_default();
            let mut options =
                serde_json::to_value(facet_options(db)?).map_err(|error| error.to_string())?;
            let mut values = match options[field].take() {
                Value::Array(values) if field != "pec" => values,
                _ => return Err(format!("Campo non valido: {field:?}")),
            };
            values.truncate(arg_usize(args, "limit", 100));
            Ok(Value::Array(values))
        }
        _ => Err(format!("Strumento sconosciuto: {name}")),
    }
}

/// Answers one JSON-RPC message; `None` for notifications, which get no reply.
pub(crate) fn handle(db: &Connection, message: &Value) -> Option<Value> {
    let id = message.get("id")?.clone();
    let params = &message["params"];
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "database-contatti", "version": env!("CARGO_PKG_VERSION")}
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let outcome = call_tool(
                db,
                params["name"].as_str().unwrap_or_default(),
                &params["arguments"],
            );
            let (text, is_error) = match outcome {
                Ok(value) => (value.to_string(), false),
                Err(error) => (error, true),
            };
            json!({"content": [{"type": "text", "text": text}], "isError": is_error})
        }
        method => {
            return Some(json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("Metodo non supportato: {method}")}
            }));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Serves MCP on stdin/stdout until stdin closes. Logs go to stderr only.
pub fn run_mcp() -> Result<(), String> {
    let path = database_path()?;
    let db = open_read_only(&path)?;
    eprintln!("[mcp] database in sola lettura: {}", path.display());
    let mut stdout = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(&db, &message),
            Err(error) => Some(json!({
                "jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": error.to_string()}
            })),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")
                .and_then(|()| stdout.flush())
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

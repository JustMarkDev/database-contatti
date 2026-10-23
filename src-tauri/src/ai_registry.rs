use crate::ai_opencode::OpenCodeProvider;
use crate::ai_provider::InferenceProvider;
use crate::ai_selection::{PROVIDER_OPENCODE, ProviderSelection};
use crate::ai_typesafe::{TYPESAFE_SETTINGS_KEY, TypeSafeClient, resolve_api_key};
use rusqlite::{Connection, params};
use std::sync::Arc;
use std::time::Duration;

pub(crate) fn load_setting(db: &Connection, key: &str) -> Result<Option<String>, String> {
    db.query_row(
        "SELECT value FROM app_settings WHERE key=?1",
        params![key],
        |row| row.get(0),
    )
    .optional_setting()
}

pub(crate) fn save_setting(db: &Connection, key: &str, value: &str) -> Result<(), String> {
    db.execute(
        "INSERT INTO app_settings(key,value,updated_at) VALUES(?1,?2,?3)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
        params![key, value, chrono::Utc::now().to_rfc3339()],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

trait OptionalSetting<T> {
    fn optional_setting(self) -> Result<Option<T>, String>;
}

impl<T> OptionalSetting<T> for Result<T, rusqlite::Error> {
    fn optional_setting(self) -> Result<Option<T>, String> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }
}

pub(crate) fn build_generative_provider(
    selection: &ProviderSelection,
) -> Result<Arc<dyn InferenceProvider>, String> {
    build_generative_provider_with_effort(selection, selection.generative_effort())
}

pub(crate) fn build_generative_provider_with_effort(
    selection: &ProviderSelection,
    effort: Option<&str>,
) -> Result<Arc<dyn InferenceProvider>, String> {
    let provider_id = selection.generative_provider_id();
    let model = selection.generative_model();
    let timeout = Duration::from_secs(selection_timeout_hint(selection));
    match provider_id {
        PROVIDER_OPENCODE => Ok(Arc::new(OpenCodeProvider::new(model, effort, timeout)?)),
        other => Err(format!("Provider generativo non supportato: {other}")),
    }
}

pub(crate) fn build_typesafe_client(
    db: &Connection,
    selection: &ProviderSelection,
    timeout_seconds: u64,
) -> Result<TypeSafeClient, String> {
    let key = resolve_api_key(load_setting(db, TYPESAFE_SETTINGS_KEY)?).ok_or_else(|| {
        "Chiave API TypeSafe mancante (impostazioni o TYPESAFE_API_KEY)".to_owned()
    })?;
    Ok(TypeSafeClient::new(
        key,
        selection.jev_model.clone(),
        Duration::from_secs(timeout_seconds.max(60)),
    ))
}

fn selection_timeout_hint(_selection: &ProviderSelection) -> u64 {
    600
}

/// `(level, parent semantic key, semantic key)` -> display name.
type TaxonomyNames = std::collections::HashMap<(String, Option<String>, String), String>;

pub(crate) fn load_taxonomy_display_names(
    db: &Connection,
    taxonomy_version_id: i64,
) -> Result<TaxonomyNames, String> {
    db.prepare(
        "SELECT level,semantic_key,display_name,parent_semantic_key
         FROM taxonomy_entries WHERE taxonomy_version_id=?1",
    )
    .map_err(|error| error.to_string())?
    .query_map(params![taxonomy_version_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })
    .map_err(|error| error.to_string())?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|error| error.to_string())
    .map(|rows| {
        rows.into_iter()
            .map(|(level, key, display, parent)| ((level, parent, key), display))
            .collect()
    })
}

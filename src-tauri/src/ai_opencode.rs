use crate::ai_provider::{InferenceProvider, InferenceRequest, ProviderError};
use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const OPENCODE_PROVIDER_ID: &str = "opencode";

#[derive(Debug, Clone, Serialize)]
pub struct OpenCodeModelInfo {
    pub id: String,
    pub variants: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct OpenCodeStatus {
    pub available: bool,
    pub installed: bool,
    pub authenticated: bool,
    pub models: Vec<OpenCodeModelInfo>,
    pub selected_model: String,
    pub selected_effort: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

fn binary_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("DATABASE_CONTATTI_OPENCODE_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    for candidate in [
        "/opt/homebrew/bin/opencode",
        "/usr/local/bin/opencode",
        "/usr/bin/opencode",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Some(path);
        }
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let user_bin = home.map(|home| PathBuf::from(home).join(".opencode/bin"));
    // A GUI app on macOS has a minimal PATH, hence the fixed locations above; this also
    // covers Windows (`.exe`, `.cmd`) where `which` does not exist.
    let search =
        std::env::var_os("PATH").map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>());
    user_bin
        .into_iter()
        .chain(search.into_iter().flatten())
        .flat_map(|dir| {
            ["opencode", "opencode.exe", "opencode.cmd"]
                .into_iter()
                .map(move |name| dir.join(name))
        })
        .find(|path| path.is_file())
}

pub(crate) fn parse_version_output(stdout: &str) -> Option<String> {
    stdout.split_whitespace().find_map(|part| {
        let trimmed = part.trim_start_matches('v');
        trimmed
            .starts_with(|ch: char| ch.is_ascii_digit())
            .then(|| trimmed.to_owned())
    })
}

fn read_version(binary: &PathBuf) -> Option<String> {
    let output = Command::new(binary).arg("--version").output().ok()?;
    output.status.success().then_some(())?;
    parse_version_output(&format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn parse_models_plain(stdout: &str) -> Vec<OpenCodeModelInfo> {
    let mut models = Vec::new();
    for line in stdout.lines() {
        let id = line.trim();
        if id.contains('/') && !id.starts_with('{') {
            let id = id.split('#').next().unwrap_or(id).to_owned();
            if !id.is_empty() {
                models.push(OpenCodeModelInfo {
                    id,
                    variants: Vec::new(),
                });
            }
        }
    }
    models
}

pub(crate) fn auth_output_authenticated(stdout: &str) -> bool {
    let text = stdout.trim();
    if text.is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    !(lower.contains("no providers")
        || lower.contains("not authenticated")
        || lower.contains("no authenticated integrations"))
}

fn list_models(binary: &PathBuf) -> Result<Vec<OpenCodeModelInfo>, String> {
    // OpenCode 2.0.x prints nothing for `models --standalone` (exit 0), so retry without it.
    let mut last_error = None;
    for args in [&["models", "--standalone"][..], &["models"][..]] {
        let output = Command::new(binary)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("impossibile avviare `opencode models`: {error}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr.trim().lines().last().unwrap_or("nessun dettaglio");
            last_error = Some(format!(
                "`opencode models` è fallito ({}): {detail}",
                output.status
            ));
            continue;
        }
        let models = parse_models_plain(&String::from_utf8_lossy(&output.stdout));
        if !models.is_empty() {
            return Ok(models);
        }
    }
    last_error.map_or_else(|| Ok(Vec::new()), Err)
}

fn check_authenticated(binary: &PathBuf) -> bool {
    Command::new(binary)
        .args(["auth", "list", "--standalone"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| auth_output_authenticated(&String::from_utf8_lossy(&output.stdout)))
}

fn inspect_opencode() -> OpenCodeStatus {
    let Some(binary) = binary_path() else {
        return OpenCodeStatus {
            available: false,
            installed: false,
            authenticated: false,
            models: Vec::new(),
            selected_model: String::new(),
            selected_effort: None,
            version: None,
            error: Some("OpenCode non è installato nel profilo personale".to_owned()),
        };
    };
    let version = read_version(&binary);
    let (models, models_error) = match list_models(&binary) {
        Ok(models) => (models, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let authenticated = check_authenticated(&binary);
    let selected_model = models
        .iter()
        .map(|model| model.id.clone())
        .find(|id| id.contains("muse-spark"))
        .or_else(|| models.first().map(|model| model.id.clone()))
        .unwrap_or_default();
    let selected_effort = models
        .iter()
        .find(|model| model.id == selected_model)
        .and_then(|model| {
            ["medium", "low", "high"]
                .into_iter()
                .find(|variant| model.variants.iter().any(|item| item == variant))
                .map(str::to_owned)
                .or_else(|| model.variants.first().cloned())
        });
    let error = if let Some(error) = models_error {
        Some(error)
    } else if models.is_empty() {
        Some("Nessun modello OpenCode disponibile".to_owned())
    } else if !authenticated {
        Some("OpenCode non risulta autenticato (`opencode auth login`)".to_owned())
    } else {
        None
    };
    OpenCodeStatus {
        available: !models.is_empty() && authenticated,
        installed: true,
        authenticated,
        models,
        selected_model,
        selected_effort,
        version,
        error,
    }
}

#[tauri::command]
pub async fn detect_opencode() -> Result<OpenCodeStatus, String> {
    tauri::async_runtime::spawn_blocking(inspect_opencode)
        .await
        .map_err(|error| error.to_string())
}

pub(crate) struct OpenCodeProvider {
    binary: PathBuf,
    model: String,
    effort: Option<String>,
    timeout: Duration,
    last_usage: Arc<Mutex<Option<Value>>>,
}

/// OpenCode agent for every request. Every permission is denied and every tool
/// disabled, so text scraped from a site or imported from a file cannot make the
/// model read files, run commands or fetch URLs.
const SANDBOX_AGENT: &str = "lead-extract";
const SANDBOX_CONFIG: &str = r#"{"agent":{"lead-extract":{"mode":"primary","description":"Extract contacts from untrusted page text","permission":{"*":"deny"},"tools":{"*":false}}}}"#;

pub(crate) fn resolve_model_cli(model_input: &str) -> String {
    let trimmed = model_input.trim();
    let without_variant = trimmed.split('#').next().unwrap_or(trimmed).trim();
    if without_variant.contains('/') {
        without_variant.to_owned()
    } else {
        let bare =
            crate::ai_selection::strip_provider_prefix(OPENCODE_PROVIDER_ID, without_variant);
        format!("{OPENCODE_PROVIDER_ID}/{bare}")
    }
}

/// Build the `-m` value for `opencode run`: effort is folded into `provider/model#effort`.
pub(crate) fn model_run_arg(model: &str, effort: Option<&str>) -> String {
    let base = resolve_model_cli(model);
    match effort.map(str::trim).filter(|value| !value.is_empty()) {
        Some(effort) => format!("{base}#{effort}"),
        None => base,
    }
}

pub(crate) fn is_effort_allowed(variants: &[String], effort: &str) -> bool {
    variants.is_empty() || variants.iter().any(|variant| variant == effort)
}

impl OpenCodeProvider {
    pub(crate) fn new(
        model: &str,
        effort: Option<&str>,
        timeout: Duration,
    ) -> Result<Self, String> {
        let status = inspect_opencode();
        if !status.available {
            return Err(status
                .error
                .unwrap_or_else(|| "OpenCode non disponibile".to_owned()));
        }
        let model_input = model.trim();
        let model_bare = {
            let stripped =
                crate::ai_selection::strip_provider_prefix(OPENCODE_PROVIDER_ID, model_input);
            stripped
                .split_once('#')
                .map(|(base, _)| base.to_owned())
                .unwrap_or(stripped)
        };
        let model_cli = resolve_model_cli(model_input);
        let model_matches = |entry_id: &str| {
            entry_id == model_bare
                || entry_id == model_cli
                || crate::ai_selection::strip_provider_prefix(OPENCODE_PROVIDER_ID, entry_id)
                    == model_bare
        };
        let selected = status.models.iter().find(|entry| model_matches(&entry.id));
        if selected.is_none() {
            return Err(format!("Modello OpenCode non disponibile: {model_cli}"));
        }
        let effort = effort.map(str::trim).filter(|value| !value.is_empty());
        if let Some(effort) = effort {
            let allowed = selected
                .map(|entry| entry.variants.as_slice())
                .unwrap_or(&[]);
            if !is_effort_allowed(allowed, effort) {
                return Err(format!(
                    "Effort OpenCode non supportato per {model_cli}: {effort}"
                ));
            }
        }
        let binary = binary_path().ok_or_else(|| "Eseguibile OpenCode non trovato".to_owned())?;
        Ok(Self {
            binary,
            model: model_cli,
            effort: effort.map(str::to_owned),
            timeout,
            last_usage: Arc::new(Mutex::new(None)),
        })
    }

    fn complete_blocking(&self, request: InferenceRequest) -> Result<String, ProviderError> {
        let message = format!("{}\n\n{}", request.system_prompt, request.user_prompt);
        let model_arg = model_run_arg(&self.model, self.effort.as_deref());
        let mut command = Command::new(&self.binary);
        command.args([
            "run",
            "--standalone",
            "-m",
            &model_arg,
            "--format",
            "json",
            "--title",
            "database-contatti-classifier",
        ]);
        command
            .args(["--pure", "--agent", SANDBOX_AGENT])
            .env("OPENCODE_CONFIG_CONTENT", SANDBOX_CONFIG)
            .current_dir(std::env::temp_dir());
        command
            .arg(&message)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            ProviderError::unavailable(format!("Impossibile avviare OpenCode: {error}"))
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProviderError::unavailable("OpenCode stdout non disponibile"))?;
        // Both pipes are drained on their own threads: a silent hang must still hit the
        // deadline, and a chatty stderr must not fill its pipe and block the child.
        let (lines_tx, lines_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if lines_tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_reader = child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut kept = Vec::new();
                let _ = pipe.by_ref().take(STDERR_LIMIT).read_to_end(&mut kept);
                // Keep draining so the child never blocks on a full pipe.
                let _ = std::io::copy(&mut pipe, &mut std::io::sink());
                String::from_utf8_lossy(&kept).into_owned()
            })
        });
        let deadline = std::time::Instant::now() + self.timeout;
        let mut text_parts = Vec::new();
        let mut usage: Option<Value> = None;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let line = match lines_rx.recv_timeout(remaining) {
                Ok(line) => line,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProviderError::timeout(
                        "OpenCode ha superato il timeout della richiesta",
                    ));
                }
            };
            let line = line
                .map_err(|error| ProviderError::unavailable(format!("OpenCode stdout: {error}")))?;
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let event_type = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if event_type == "text"
                && let Some(text) = value.pointer("/part/text").and_then(Value::as_str)
            {
                text_parts.push(text.to_owned());
            }
            if event_type == "step_finish"
                && let Some(tokens) = value.pointer("/part/tokens")
            {
                usage = Some(tokens.clone());
            }
            if matches!(event_type, "step_finish" | "error") && step_hit_limit(&value) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProviderError::rate_limited(
                    "Limite OpenCode raggiunto; attesa e nuovo tentativo automatico",
                ));
            }
        }
        let status = child
            .wait()
            .map_err(|error| ProviderError::unavailable(format!("OpenCode wait: {error}")))?;
        if let Ok(mut slot) = self.last_usage.lock() {
            *slot = usage;
        }
        if !status.success() {
            let stderr_text = stderr_reader
                .and_then(|handle| handle.join().ok())
                .filter(|buf| !buf.trim().is_empty());
            return Err(ProviderError::unavailable(format!(
                "OpenCode terminato con codice {:?}{}",
                status.code(),
                stderr_text
                    .map(|text| format!(": {}", text.trim()))
                    .unwrap_or_default()
            )));
        }
        let text = text_parts.join("");
        if text.trim().is_empty() {
            return Err(ProviderError::terminal("opencode.empty_response"));
        }
        Ok(extract_json_payload(&text))
    }
}

const STDERR_LIMIT: u64 = 64 * 1024;

/// Only the step's own reason and error count: the token counts in the same event
/// can contain "429" or "limit" as plain digits or key names.
fn step_hit_limit(step: &Value) -> bool {
    let text = [
        step.pointer("/part/reason"),
        step.pointer("/part/error"),
        step.get("error"),
    ]
    .into_iter()
    .flatten()
    .map(Value::to_string)
    .collect::<String>()
    .to_lowercase();
    ["quota", "rate limit", "usage limit", "429", "limit reached"]
        .iter()
        .any(|needle| text.contains(needle))
}

fn extract_json_payload(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('{')
        && let Some(end) = trimmed.rfind('}')
        && end > start
    {
        return trimmed[start..=end].to_owned();
    }
    trimmed.to_owned()
}

impl InferenceProvider for OpenCodeProvider {
    fn provider_id(&self) -> &'static str {
        OPENCODE_PROVIDER_ID
    }

    fn complete(
        &self,
        request: InferenceRequest,
    ) -> BoxFuture<'static, Result<String, ProviderError>> {
        let binary = self.binary.clone();
        let model = self.model.clone();
        let effort = self.effort.clone();
        let timeout = self.timeout;
        let last_usage = self.last_usage.clone();
        Box::pin(async move {
            tauri::async_runtime::spawn_blocking(move || {
                OpenCodeProvider {
                    binary,
                    model,
                    effort,
                    timeout,
                    last_usage,
                }
                .complete_blocking(request)
            })
            .await
            .map_err(|error| {
                ProviderError::unavailable(format!("OpenCode worker interrotto: {error}"))
            })?
        })
    }

    fn take_last_usage(&self) -> Option<Value> {
        self.last_usage.lock().ok()?.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_output_reads_version() {
        assert_eq!(
            parse_version_output("opencode 2.0.18"),
            Some("2.0.18".to_owned())
        );
        assert_eq!(parse_version_output("v2.1.0"), Some("2.1.0".to_owned()));
        assert_eq!(parse_version_output("not-a-version"), None);
    }

    #[test]
    fn parse_models_plain_strips_variant_suffix() {
        let stdout = "opencode-go/muse-spark-1.3-contributor\nanthropic/claude-sonnet-4-5#high\n";
        let models = parse_models_plain(stdout);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "opencode-go/muse-spark-1.3-contributor");
        assert!(models[0].variants.is_empty());
        assert_eq!(models[1].id, "anthropic/claude-sonnet-4-5");
    }

    #[test]
    fn effort_gate_passes_with_empty_variants() {
        assert!(is_effort_allowed(&[], "high"));
        let variants = vec!["low".to_owned(), "high".to_owned()];
        assert!(is_effort_allowed(&variants, "high"));
        assert!(!is_effort_allowed(&variants, "minimal"));
    }

    #[test]
    fn resolve_model_cli_preserves_namespaced_id() {
        assert_eq!(
            resolve_model_cli("opencode-go/muse-spark-1.3-contributor"),
            "opencode-go/muse-spark-1.3-contributor"
        );
        assert_eq!(
            resolve_model_cli("muse-spark-1.3-contributor"),
            "opencode/muse-spark-1.3-contributor"
        );
        assert_eq!(
            resolve_model_cli("opencode-go/muse-spark-1.3-contributor#high"),
            "opencode-go/muse-spark-1.3-contributor"
        );
    }

    #[test]
    fn model_run_arg_folds_effort() {
        assert_eq!(
            model_run_arg("opencode-go/muse-spark-1.3-contributor", Some("high")),
            "opencode-go/muse-spark-1.3-contributor#high"
        );
        assert_eq!(
            model_run_arg("opencode-go/muse-spark-1.3-contributor", None),
            "opencode-go/muse-spark-1.3-contributor"
        );
    }

    #[test]
    fn auth_output_rejects_empty_and_v2_message() {
        assert!(!auth_output_authenticated(""));
        assert!(!auth_output_authenticated(
            "No authenticated integrations\n"
        ));
        assert!(!auth_output_authenticated("No providers configured"));
        assert!(auth_output_authenticated("opencode-go  stored\n"));
    }

    #[test]
    fn token_counts_containing_429_are_not_a_rate_limit() {
        let busy: Value = serde_json::from_str(
            r#"{"type":"step_finish","part":{"reason":"stop","tokens":{"input":14290,"output":429}}}"#,
        )
        .unwrap();
        assert!(!step_hit_limit(&busy));
        let limited: Value = serde_json::from_str(
            r#"{"type":"error","error":{"data":{"message":"Rate limit exceeded"}}}"#,
        )
        .unwrap();
        assert!(step_hit_limit(&limited));
    }

    #[cfg(unix)]
    #[test]
    fn silent_process_hits_the_deadline() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fake_opencode_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("opencode");
        std::fs::write(&binary, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let provider = OpenCodeProvider {
            binary,
            model: "opencode-go/x".to_owned(),
            effort: None,
            timeout: Duration::from_millis(300),
            last_usage: Arc::new(Mutex::new(None)),
        };
        let started = std::time::Instant::now();
        let error = provider
            .complete_blocking(InferenceRequest {
                system_prompt: String::new(),
                user_prompt: String::new(),
            })
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(error.message.contains("timeout"), "{}", error.message);
    }

    #[test]
    fn extract_json_payload_strips_prose() {
        let text = "Here you go:\n{\"results\":[]}\nthanks";
        assert_eq!(extract_json_payload(text), "{\"results\":[]}");
    }
}

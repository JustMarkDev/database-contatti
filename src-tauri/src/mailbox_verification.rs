use crate::{new_contact_id, open_db};
use chrono::{DateTime, Duration, Utc};
use futures::{StreamExt, stream};
use native_tls::TlsConnector;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration as StdDuration, Instant};

const MAX_PILOT: usize = 1000;
const MAX_RECOMMENDED: usize = 5000;
const DEFAULT_RATE: u64 = 5;
static MAILBOX_STARTUP_NORMALIZED: AtomicBool = AtomicBool::new(false);
static MAILBOX_WORKER_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct MailboxVerificationSettings {
    pub ehlo_hostname: Option<String>,
    pub envelope_sender: Option<String>,
    pub starttls_required: bool,
    pub global_rate_per_minute: u32,
    pub max_concurrency: u32,
}

#[derive(Debug, Serialize)]
pub(crate) struct MailboxVerificationOverview {
    pub settings: MailboxVerificationSettings,
    pub active_job: Option<MailboxVerificationStatus>,
    pub eligible_contacts: usize,
    pub eligible_domains: usize,
    pub eligible_mx_hosts: usize,
    pub routable_contacts: usize,
    pub non_routable_contacts: usize,
    pub unknown_contacts: usize,
    pub suppressed_contacts: usize,
    pub blacklisted_contacts: usize,
    pub non_contact_mailboxes: usize,
    pub temporary_dns_failures: usize,
    pub preflight_ok: bool,
    pub preflight_message: Option<String>,
    pub last_completed_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MailboxVerificationStatus {
    pub id: String,
    pub mode: String,
    pub status: String,
    pub stage: String,
    pub total_queued: usize,
    pub completed: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub temporary: usize,
    pub unknown: usize,
    pub catch_all: usize,
    pub policy_blocked: usize,
    pub last_error: Option<String>,
    pub safety_gate: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct MailboxDnsRevalidationReport {
    pub domains_examined: usize,
    pub domains_revalidated: usize,
    pub routing_ok: usize,
    pub no_routing: usize,
    pub temporary_failures: usize,
    pub promoted_contacts: usize,
    pub separated_contacts: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MailboxVerificationSettingsInput {
    pub ehlo_hostname: Option<String>,
    pub envelope_sender: Option<String>,
    #[serde(alias = "require_starttls")]
    pub starttls_required: Option<bool>,
    pub global_rate_per_minute: Option<u32>,
    pub max_concurrency: Option<u32>,
}

#[derive(Debug, Clone)]
struct Task {
    id: String,
    job_id: String,
    contact_id: Option<String>,
    candidate_id: Option<String>,
    comparison_key: String,
    email: String,
    domain: String,
    mx_hosts: Vec<String>,
    attempts: i64,
}

#[derive(Debug)]
struct ProbeResult {
    outcome: &'static str,
    smtp_code: Option<i64>,
    enhanced_code: Option<String>,
    error_class: Option<String>,
    response: Option<String>,
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn load_settings(db: &Connection) -> Result<MailboxVerificationSettings, String> {
    db.query_row("SELECT ehlo_hostname,envelope_sender,starttls_required,global_rate_per_minute,max_concurrency FROM mailbox_verification_settings WHERE id=1", [], |row| {
        Ok(MailboxVerificationSettings { ehlo_hostname: row.get(0)?, envelope_sender: row.get(1)?, starttls_required: row.get::<_,i64>(2)? != 0, global_rate_per_minute: row.get::<_,i64>(3)?.clamp(1,60) as u32, max_concurrency: row.get::<_,i64>(4)?.clamp(1,2) as u32 })
    }).optional().map_err(|e| e.to_string())?.ok_or_else(|| "Impostazioni SMTP non disponibili".to_owned())
}

fn validate_settings(
    input: MailboxVerificationSettingsInput,
) -> Result<MailboxVerificationSettings, String> {
    let ehlo = input
        .ehlo_hostname
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    let sender = input
        .envelope_sender
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    if let Some(host) = &ehlo {
        let valid = host.len() <= 253
            && host.contains('.')
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            });
        if !valid {
            return Err("Il nome EHLO deve essere un hostname FQDN".to_owned());
        }
    }
    if let Some(email) = &sender
        && !crate::parsing::is_valid_email_syntax(email)
    {
        return Err("Il mittente envelope non è un indirizzo valido".to_owned());
    }
    if input.starttls_required == Some(false) {
        return Err("STARTTLS è obbligatorio per la verifica SMTP".to_owned());
    }
    if input.max_concurrency.is_some_and(|value| value != 1) {
        return Err("La verifica SMTP conservativa usa una sola connessione alla volta".to_owned());
    }
    Ok(MailboxVerificationSettings {
        ehlo_hostname: ehlo,
        envelope_sender: sender,
        starttls_required: true,
        global_rate_per_minute: input.global_rate_per_minute.unwrap_or(5).clamp(1, 60),

        max_concurrency: 1,
    })
}

#[tauri::command]
pub(crate) async fn save_mailbox_verification_settings(
    app: tauri::AppHandle,
    settings: MailboxVerificationSettingsInput,
) -> Result<MailboxVerificationSettings, String> {
    let settings = validate_settings(settings)?;
    tauri::async_runtime::spawn_blocking(move || {
        let db = open_db(&app)?;
        db.execute("UPDATE mailbox_verification_settings SET ehlo_hostname=?1,envelope_sender=?2,starttls_required=?3,global_rate_per_minute=?4,max_concurrency=?5,updated_at=?6 WHERE id=1", params![settings.ehlo_hostname, settings.envelope_sender, i64::from(settings.starttls_required), settings.global_rate_per_minute, settings.max_concurrency, now()]).map_err(|e| e.to_string())?;
        Ok(settings)
    })
    .await
    .map_err(|e| format!("Salvataggio impostazioni interrotto: {e}"))?
}

fn status_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MailboxVerificationStatus> {
    Ok(MailboxVerificationStatus {
        id: row.get(0)?,
        mode: row.get(1)?,
        status: row.get(2)?,
        stage: row.get(3)?,
        total_queued: row.get::<_, i64>(4)? as usize,
        completed: row.get::<_, i64>(5)? as usize,
        accepted: row.get::<_, i64>(6)? as usize,
        rejected: row.get::<_, i64>(7)? as usize,
        temporary: row.get::<_, i64>(8)? as usize,
        unknown: row.get::<_, i64>(9)? as usize,
        catch_all: row.get::<_, i64>(10)? as usize,
        policy_blocked: row.get::<_, i64>(11)? as usize,
        last_error: row.get(12)?,
        safety_gate: row.get(13)?,
        created_at: row.get(14)?,
        started_at: row.get(15)?,
        finished_at: row.get(16)?,
        updated_at: row.get(17)?,
    })
}

fn load_status(db: &Connection, id: &str) -> Result<Option<MailboxVerificationStatus>, String> {
    db.query_row("SELECT id,mode,status,stage,total_queued,completed,accepted,rejected,temporary,unknown,catch_all,policy_blocked,last_error,safety_gate,created_at,started_at,finished_at,updated_at FROM mailbox_verification_jobs WHERE id=?1", params![id], status_from_row).optional().map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn get_mailbox_verification_status(
    app: tauri::AppHandle,
    job_id: Option<String>,
) -> Result<Option<MailboxVerificationStatus>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let db = open_db(&app)?;
        if let Some(id) = job_id {
            return load_status(&db, &id);
        }
        db.query_row("SELECT id,mode,status,stage,total_queued,completed,accepted,rejected,temporary,unknown,catch_all,policy_blocked,last_error,safety_gate,created_at,started_at,finished_at,updated_at FROM mailbox_verification_jobs ORDER BY created_at DESC LIMIT 1", [], status_from_row).optional().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("Lettura stato interrotta: {e}"))?
}

fn candidates_overview(
    db: &Connection,
) -> Result<(usize, usize, usize, usize, usize, usize), String> {
    let (eligible_contacts, eligible_domains): (usize, usize) = db
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT lower(substr(email,instr(email,'@')+1)))
             FROM contacts
             WHERE status='valid' AND coalesce(suppression_status,'active')='active'",
            [],
            |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as usize)),
        )
        .map_err(|e| e.to_string())?;
    let eligible_mx_hosts: usize = db
        .query_row(
            "SELECT COUNT(DISTINCT mx.value)
             FROM domain_verification_cache d
             JOIN json_each(d.mx_hosts_json) mx
             WHERE d.routing_status = 'routing_ok'
               AND d.domain IN (
                   SELECT DISTINCT lower(substr(email,instr(email,'@')+1))
                   FROM contacts
                   WHERE status='valid' AND coalesce(suppression_status,'active')='active'
               )",
            [],
            |r| Ok(r.get::<_, i64>(0)? as usize),
        )
        .map_err(|e| e.to_string())?;
    let (non_routable, unknown, suppressed): (usize, usize, usize) = db
        .query_row(
            "SELECT
                COUNT(CASE WHEN current_disposition='no_routing' THEN 1 END),
                COUNT(CASE WHEN current_disposition='temporary_dns_failure' OR routing_status IN ('unknown','temporary_failure','temporary_dns_failure') THEN 1 END),
                COUNT(CASE WHEN current_disposition='suppressed' THEN 1 END)
             FROM email_candidates
             WHERE current_disposition <> 'linked'",
            [],
            |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as usize, r.get::<_, i64>(2)? as usize)),
        )
        .map_err(|e| e.to_string())?;
    Ok((
        eligible_contacts,
        eligible_domains,
        eligible_mx_hosts,
        non_routable,
        unknown,
        suppressed,
    ))
}

#[tauri::command]
pub(crate) async fn get_mailbox_verification_overview(
    app: tauri::AppHandle,
) -> Result<MailboxVerificationOverview, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let db = open_db(&app)?;
        let settings = load_settings(&db)?;
        let (
            eligible_contacts,
            eligible_domains,
            eligible_mx_hosts,
            non_routable_contacts,
            unknown_contacts,
            suppressed_contacts,
        ) = candidates_overview(&db)?;
        let status = db
            .query_row(
                "SELECT id FROM mailbox_verification_jobs WHERE status IN ('preparing','running','paused') ORDER BY created_at DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .and_then(|id| load_status(&db, &id).ok().flatten());
        let (non_contact_mailboxes, blacklisted_contacts, temporary_dns_failures): (usize, usize, usize) = db
            .query_row(
                "SELECT
                    COUNT(CASE WHEN current_disposition='suppressed' AND disposition_reason='non_contact_mailbox' THEN 1 END),
                    COUNT(CASE WHEN current_disposition='blacklisted' OR (current_disposition='suppressed' AND disposition_reason='blacklist') THEN 1 END),
                    COUNT(CASE WHEN current_disposition='temporary_dns_failure' OR routing_status IN ('temporary_failure','temporary_dns_failure') THEN 1 END)
                 FROM email_candidates
                 WHERE current_disposition <> 'linked'",
                [],
                |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as usize, r.get::<_, i64>(2)? as usize)),
            )
            .map_err(|e| e.to_string())?;
        let last_completed_at: Option<String> = db.query_row("SELECT finished_at FROM mailbox_verification_jobs WHERE status='completed' AND finished_at IS NOT NULL ORDER BY finished_at DESC LIMIT 1",[],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        let identity_ready = settings.ehlo_hostname.is_some() && settings.envelope_sender.is_some();
        let preflight_ok = identity_ready && eligible_contacts > 0;
        let preflight_message = if !identity_ready {
            Some("Configura hostname EHLO e mittente envelope".to_owned())
        } else if eligible_contacts == 0 {
            Some("Nessun indirizzo attualmente eleggibile dopo DNS e soppressioni".to_owned())
        } else {
            None
        };
        Ok(MailboxVerificationOverview {
            settings,
            active_job: status,
            eligible_contacts,
            eligible_domains,
            eligible_mx_hosts,
            routable_contacts: eligible_contacts,
            non_routable_contacts,
            unknown_contacts,
            suppressed_contacts,
            blacklisted_contacts,
            non_contact_mailboxes,
            temporary_dns_failures,
            preflight_ok,
            preflight_message,
            last_completed_at,
        })
    })
    .await
    .map_err(|e| format!("Caricamento panoramica interrotto: {e}"))?
}

fn cache_expired(value: Option<&String>) -> bool {
    value
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v <= Utc::now())
        .unwrap_or(true)
}

/// `(domain, cached expiry, routing status)`; the latter two are None when never checked.
type DomainState = (String, Option<String>, Option<String>);

/// Domains of eligible contacts and of DNS-failed candidates, each with its
/// cached expiry and routing status (None when never checked).
pub(crate) fn dns_revalidation_domain_state(db: &Connection) -> Result<Vec<DomainState>, String> {
    let mut contact_stmt = db.prepare(
        "SELECT DISTINCT lower(substr(email,instr(email,'@')+1)) FROM contacts WHERE status='valid' AND coalesce(suppression_status,'active')='active'
         UNION
         SELECT DISTINCT lower(substr(COALESCE(canonical_email,raw_email),instr(COALESCE(canonical_email,raw_email),'@')+1))
         FROM email_candidates
         WHERE current_disposition='temporary_dns_failure' OR routing_status IN ('temporary_failure','temporary_dns_failure')"
    ).map_err(|e| e.to_string())?;
    let contact_domains = contact_stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut cache_stmt = db
        .prepare("SELECT lower(domain), expires_at, routing_status FROM domain_verification_cache")
        .map_err(|e| e.to_string())?;
    let cache: std::collections::HashMap<String, (Option<String>, Option<String>)> = cache_stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let mut rows = Vec::with_capacity(contact_domains.len());
    for domain in contact_domains {
        if let Some((expires_at, routing_status)) = cache.get(&domain) {
            rows.push((domain, expires_at.clone(), routing_status.clone()));
        } else {
            rows.push((domain, None, None));
        }
    }
    Ok(rows)
}

async fn prepare_dns(
    app: &tauri::AppHandle,
    temporary_only: bool,
) -> Result<MailboxDnsRevalidationReport, String> {
    let app_read = app.clone();
    let domain_state = crate::database::blocking(move || {
        let db = open_db(&app_read)?;
        dns_revalidation_domain_state(&db)
    })
    .await?;
    let domains_examined = domain_state.len();
    let domains: Vec<String> = domain_state
        .into_iter()
        .filter(|(_, expires, status)| {
            if temporary_only {
                return status.as_deref() == Some("temporary_failure");
            }
            status.is_none()
                || cache_expired(expires.as_ref())
                || status.as_deref() == Some("temporary_failure")
        })
        .map(|(domain, _, _)| domain)
        .collect();
    let resolver = hickory_resolver::TokioResolver::builder_tokio()
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    let results = stream::iter(domains.clone())
        .map(|domain| async {
            let r = crate::parsing::validate_domain(&resolver, &domain).await;
            (domain, r)
        })
        .buffer_unordered(20)
        .collect::<Vec<_>>()
        .await;
    let app_write = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        persist_dns_results(&app_write, results, domains_examined)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn persist_dns_results(
    app: &tauri::AppHandle,
    results: Vec<(String, crate::parsing::DomainValidation)>,
    examined: usize,
) -> Result<MailboxDnsRevalidationReport, String> {
    let mut db = open_db(app)?;
    apply_dns_results(&mut db, results, examined)
}

pub(crate) fn apply_dns_results(
    db: &mut Connection,
    results: Vec<(String, crate::parsing::DomainValidation)>,
    examined: usize,
) -> Result<MailboxDnsRevalidationReport, String> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let mut report = MailboxDnsRevalidationReport {
        domains_examined: examined,
        domains_revalidated: results.len(),
        routing_ok: 0,
        no_routing: 0,
        temporary_failures: 0,
        promoted_contacts: 0,
        separated_contacts: 0,
    };
    // One scan each instead of one per domain: the domain is an expression, so no index
    // serves it and a per-domain lookup rereads millions of rows for every domain.
    let wanted: std::collections::HashSet<&str> = results.iter().map(|(d, _)| d.as_str()).collect();
    type DomainCandidate = (String, String, Option<String>, String);
    let mut candidates_by_domain: std::collections::HashMap<String, Vec<DomainCandidate>> =
        std::collections::HashMap::new();
    {
        let mut stmt = tx.prepare(&format!("SELECT lower(substr(COALESCE(ec.canonical_email,ec.raw_email),instr(COALESCE(ec.canonical_email,ec.raw_email),'@')+1)),ec.id,ec.comparison_key,ec.contact_id,ec.current_disposition FROM email_candidates ec WHERE ec.current_disposition NOT IN ('suppressed','blacklisted','invalid_syntax') AND NOT ({})", held_quarantine_sql("ec")))
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let domain = row
                .get_ref(0)
                .map_err(|e| e.to_string())?
                .as_str()
                .map_err(|e| e.to_string())?;
            if !wanted.contains(domain) {
                continue;
            }
            let domain = domain.to_owned();
            let item = (
                row.get::<_, String>(1).map_err(|e| e.to_string())?,
                row.get::<_, String>(2).map_err(|e| e.to_string())?,
                row.get::<_, Option<String>>(3).map_err(|e| e.to_string())?,
                row.get::<_, String>(4).map_err(|e| e.to_string())?,
            );
            candidates_by_domain.entry(domain).or_default().push(item);
        }
    }
    let mut contacts_by_domain: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT lower(substr(email,instr(email,'@')+1)),id FROM contacts WHERE coalesce(suppression_status,'active')='active'")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let domain = row
                .get_ref(0)
                .map_err(|e| e.to_string())?
                .as_str()
                .map_err(|e| e.to_string())?;
            if !wanted.contains(domain) {
                continue;
            }
            let domain = domain.to_owned();
            let id: String = row.get(1).map_err(|e| e.to_string())?;
            contacts_by_domain.entry(domain).or_default().push(id);
        }
    }
    for (domain, result) in results {
        let status = match result.status {
            crate::EmailStatus::Valid => {
                report.routing_ok += 1;
                "routing_ok"
            }
            crate::EmailStatus::DomainMissingMx => {
                report.no_routing += 1;
                "no_routing"
            }
            _ => {
                report.temporary_failures += 1;
                "temporary_failure"
            }
        };
        let ttl =
            crate::imports::effective_cache_ttl_seconds(result.status, result.observed_ttl_seconds);
        let checked = now();
        let expires = (Utc::now() + Duration::seconds(ttl)).to_rfc3339();
        let mx_json = serde_json::to_string(&result.mx_hosts).unwrap_or_else(|_| "[]".into());
        let details_json = serde_json::json!({
            "source":"mailbox_verification_dns_prepare",
            "notes":result.notes
        })
        .to_string();
        tx.execute("INSERT INTO domain_verification_cache(domain,routing_status,dns_rcode,mx_hosts_json,ttl_seconds,observed_ttl_seconds,effective_cache_ttl_seconds,checked_at,expires_at,error_class,details_json) VALUES (?1,?2,?3,?4,?5,?6,?5,?7,?8,?9,?10) ON CONFLICT(domain) DO UPDATE SET routing_status=excluded.routing_status,dns_rcode=excluded.dns_rcode,mx_hosts_json=excluded.mx_hosts_json,ttl_seconds=excluded.ttl_seconds,observed_ttl_seconds=excluded.observed_ttl_seconds,effective_cache_ttl_seconds=excluded.effective_cache_ttl_seconds,checked_at=excluded.checked_at,expires_at=excluded.expires_at,error_class=excluded.error_class,details_json=excluded.details_json",params![domain,status,result.dns_rcode,mx_json,ttl,result.observed_ttl_seconds,checked,expires,if status=="temporary_failure" {Some("temporary_dns")} else {None::<&str>},details_json]).map_err(|e|e.to_string())?;
        let disp = if status == "routing_ok" {
            "linked"
        } else if status == "no_routing" {
            "no_routing"
        } else {
            "temporary_dns_failure"
        };
        let candidate_rows = candidates_by_domain.remove(&domain).unwrap_or_default();
        for (candidate_id, comparison_key, contact_id, old_disposition) in candidate_rows {
            let new_disposition = if status == "routing_ok" {
                if contact_id.is_some() {
                    "linked"
                } else {
                    "quarantined"
                }
            } else {
                disp
            };
            tx.execute("UPDATE email_candidates SET routing_status=?1,status=?2,candidate_status=?2,current_disposition=?2,disposition_reason=CASE WHEN ?1='routing_ok' THEN NULL ELSE ?2 END,outreach_eligible=CASE WHEN ?1='routing_ok' AND contact_id IS NOT NULL THEN 1 ELSE 0 END,last_seen_at=?3 WHERE id=?4",params![status,new_disposition,checked,candidate_id]).map_err(|e|e.to_string())?;
            if old_disposition != new_disposition {
                tx.execute("UPDATE candidate_dispositions SET is_current=0 WHERE candidate_id=?1 AND is_current=1", params![candidate_id]).map_err(|e|e.to_string())?;
                tx.execute("INSERT INTO candidate_dispositions(id,candidate_id,disposition,reason_code,details_json,is_current,created_at) VALUES (?1,?2,?3,?4,?5,1,?6)",params![new_contact_id(),candidate_id,new_disposition,format!("dns_revalidation_{status}"),serde_json::json!({"domain":domain,"previous":old_disposition}).to_string(),checked]).map_err(|e|e.to_string())?;
            }
            tx.execute("INSERT INTO verification_events(id,comparison_key,candidate_id,contact_id,method,provider,routing_status,dns_rcode,mx_hosts_json,ttl_seconds,observed_ttl_seconds,effective_cache_ttl_seconds,dns_checked_at,dns_expires_at,error_class,details_json,checked_at) VALUES (?1,?2,?3,?4,'dns_revalidation','system_resolver',?5,?6,?7,?8,?9,?8,?10,?11,?12,?13,?10)",params![new_contact_id(),comparison_key,candidate_id,contact_id,status,result.dns_rcode,mx_json,ttl,result.observed_ttl_seconds,checked,expires,if status=="temporary_failure" {Some("temporary_dns")} else {None::<&str>},details_json]).map_err(|e|e.to_string())?;
        }
        let contact_update_sql = "UPDATE contacts SET status=CASE WHEN ?1='routing_ok' THEN 'valid' ELSE CASE WHEN ?1='no_routing' THEN 'domain_missing_mx' ELSE 'domain_unverified' END END,routing_status=?1,last_confirmed_at=CASE WHEN ?1='routing_ok' THEN ?2 ELSE last_confirmed_at END WHERE id=?3 AND coalesce(suppression_status,'active')='active'";
        for contact_id in contacts_by_domain.remove(&domain).unwrap_or_default() {
            tx.execute(contact_update_sql, params![status, checked, contact_id])
                .map_err(|e| e.to_string())?;
        }
    }
    report.promoted_contacts += promote_routable_candidates(&tx)?;
    report.separated_contacts += separate_no_routing_contacts(&tx)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(report)
}

/// Quarantine reasons that a DNS result must never undo: the user removed the
/// contact, or a lifted blacklist entry, which needs fresh evidence (a new import).
const HELD_QUARANTINE_REASONS: &str =
    "'manual_contact_removal','legacy_invalid_cleanup','blacklist_lifted'";

fn held_quarantine_sql(candidate: &str) -> String {
    format!(
        "{candidate}.current_disposition='quarantined' AND (
             coalesce({candidate}.disposition_reason,'') IN ({HELD_QUARANTINE_REASONS})
             OR EXISTS (SELECT 1 FROM candidate_dispositions held
                        WHERE held.candidate_id={candidate}.id AND held.is_current=1
                          AND held.reason_code IN ({HELD_QUARANTINE_REASONS})))"
    )
}

fn promote_routable_candidates(tx: &rusqlite::Transaction<'_>) -> Result<usize, String> {
    let mut stmt = tx.prepare(&format!("SELECT ec.id,ec.canonical_email,ec.comparison_key,ec.category,ec.subcategory,ec.source_url,ec.source_row_id,sr.import_id,i.relative_path,sr.source_sheet,sr.row_number,sr.raw_context FROM email_candidates ec LEFT JOIN source_rows sr ON sr.id=ec.source_row_id LEFT JOIN imports i ON i.id=sr.import_id WHERE ec.contact_id IS NULL AND ec.routing_status='routing_ok' AND ec.current_disposition='quarantined' AND ec.canonical_email IS NOT NULL AND ec.syntax_status<>'invalid_syntax' AND NOT ({}) AND NOT EXISTS (SELECT 1 FROM suppressions s WHERE s.comparison_key=ec.comparison_key AND s.lifted_at IS NULL)", held_quarantine_sql("ec"))).map_err(|e|e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<i64>>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);
    let mut promoted = 0;
    for (
        candidate,
        email,
        key,
        category,
        subcategory,
        source_url,
        source_row_id,
        import_id,
        source_file,
        sheet,
        row,
        raw,
    ) in rows
    {
        let existing_contact: Option<String> = tx.query_row("SELECT id FROM contacts WHERE email_comparison_key=?1 OR email=?2 COLLATE NOCASE LIMIT 1",params![key,email],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        let contact_id = existing_contact.clone().unwrap_or_else(new_contact_id);
        let (local, domain, tld) = crate::parsing::email_parts(&email);
        let is_pec = i64::from(crate::parsing::is_pec_email(&email));
        tx.execute("INSERT OR IGNORE INTO contacts(id,email,email_comparison_key,email_local_part,email_domain,email_tld,status,validation_notes,source_file,category,subcategory,contact_kind,paese,is_pec,source_url,classification_method,classification_confidence,classification_evidence,needs_review,suppression_status,routing_status,mailbox_status,first_seen_at,last_seen_at,classification_state) VALUES (?1,?2,?3,?4,?5,?6,'valid','[\"DNS revalidation routing_ok\"]',coalesce(?7,'revalidated'),coalesce(?8,'Varie'),?9,'non_determinato','Italia',?10,?11,'revalidated',0,'[]',0,'active','routing_ok','unknown',?12,?12,'unprocessed')",params![contact_id,email,key,local,domain,tld,source_file,category,subcategory,is_pec,source_url,now()]).map_err(|e|e.to_string())?;
        let actual: String = tx.query_row("SELECT id FROM contacts WHERE email_comparison_key=?1 OR email=?2 COLLATE NOCASE LIMIT 1",params![key,email],|r|r.get(0)).map_err(|e|e.to_string())?;
        tx.execute("UPDATE email_candidates SET contact_id=?1,status='linked',candidate_status='linked',current_disposition='linked',disposition_reason='routing_ok',outreach_eligible=1 WHERE id=?2",params![actual,candidate]).map_err(|e|e.to_string())?;
        tx.execute(
            "UPDATE candidate_dispositions SET is_current=0 WHERE candidate_id=?1 AND is_current=1",
            params![candidate],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO candidate_dispositions(id,candidate_id,disposition,reason_code,is_current,created_at) VALUES (?1,?2,'linked','dns_revalidation_routing_ok',1,?3)",params![new_contact_id(),candidate,now()]).map_err(|e|e.to_string())?;
        if let (Some(sr), Some(sf), Some(row)) = (source_row_id, source_file, row) {
            tx.execute("INSERT OR IGNORE INTO contact_sources(id,contact_id,import_id,source_file,source_sheet,source_row,raw_context,source_url,source_row_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![new_contact_id(),actual,import_id,sf,sheet,row,raw.unwrap_or_else(||"{}".into()),source_url,sr]).map_err(|e|e.to_string())?;
        }
        tx.execute("UPDATE contacts SET source_occurrence_count=(SELECT COUNT(*) FROM contact_sources WHERE contact_id=?1),independent_source_count=(SELECT COUNT(DISTINCT COALESCE(i.relative_path || char(31) || COALESCE(i.content_sha256,i.file_hash,''),cs.source_file)) FROM contact_sources cs LEFT JOIN imports i ON i.id=cs.import_id WHERE cs.contact_id=?1),last_seen_at=?2 WHERE id=?1",params![actual,now()]).map_err(|e|e.to_string())?;
        if existing_contact.is_none() {
            let independent_sources: usize = tx
                .query_row(
                    "SELECT independent_source_count FROM contacts WHERE id=?1",
                    params![actual],
                    |row| Ok(row.get::<_, i64>(0)? as usize),
                )
                .map_err(|e| e.to_string())?;
            let source_confidence = (0.30 + 0.15 * independent_sources.min(4) as f64).min(0.90);
            let snapshot_id = new_contact_id();
            let created_at = now();
            tx.execute("INSERT INTO quality_score_snapshots(id,contact_id,score_version,routing_confidence,mailbox_confidence,source_confidence,freshness_score,entity_match_confidence,usefulness_score,refresh_priority,factors_json,created_at) VALUES (?1,?2,'initial-v1',0.9,NULL,?3,0.25,NULL,0.70,0.75,?4,?5)",params![snapshot_id,actual,source_confidence,serde_json::json!({"routing":"DNS revalidation routing_ok","independent_sources":independent_sources,"freshness":"legacy source date unavailable","entity":"unknown","mailbox":"unknown"}).to_string(),created_at]).map_err(|e|e.to_string())?;
            for (component, value, reason) in [
                ("routing", 0.9, "DNS routing confirmed"),
                ("source", source_confidence, "independent source count"),
                ("freshness", 0.25, "legacy source date unavailable"),
                ("entity_match", 0.0, "organization/entity unknown"),
                ("usefulness", 0.70, "initial mailbox usefulness estimate"),
                (
                    "refresh_priority",
                    0.75,
                    "legacy source should be refreshed",
                ),
            ] {
                tx.execute("INSERT INTO quality_score_components(snapshot_id,component,value,evidence_json) VALUES (?1,?2,?3,?4)",params![snapshot_id,component,value,serde_json::json!({"reason":reason}).to_string()]).map_err(|e|e.to_string())?;
            }
        }
        if existing_contact.is_none() {
            promoted += 1;
        }
    }
    Ok(promoted)
}

fn separate_no_routing_contacts(tx: &rusqlite::Transaction<'_>) -> Result<usize, String> {
    let mut stmt=tx.prepare("SELECT id FROM contacts WHERE status='domain_missing_mx' AND routing_status='no_routing'").map_err(|e|e.to_string())?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);
    let mut count = 0;
    for id in ids {
        let retained_candidates = tx.execute("UPDATE email_candidates SET contact_id=NULL,status='no_routing',candidate_status='no_routing',current_disposition='no_routing',disposition_reason='no_routing_after_revalidation',outreach_eligible=0 WHERE contact_id=?1",params![id]).map_err(|e|e.to_string())?;

        if retained_candidates > 0 {
            tx.execute("DELETE FROM contacts WHERE id=?1", params![id])
                .map_err(|e| e.to_string())?;
            count += 1;
        }
    }
    Ok(count)
}

#[tauri::command]
pub(crate) async fn revalidate_mailbox_dns(
    app: tauri::AppHandle,
) -> Result<MailboxDnsRevalidationReport, String> {
    prepare_dns(&app, true).await
}

fn eligible_tasks(db: &Connection, job_id: &str, limit: i64) -> Result<usize, String> {
    let mut stmt = db.prepare("WITH ranked AS (
        SELECT ec.id,ec.contact_id,ec.canonical_email,ec.comparison_key,
               lower(substr(ec.canonical_email,instr(ec.canonical_email,'@')+1)) AS domain,
               coalesce(ec.quality_score,0) AS priority,coalesce(d.mx_hosts_json,'[]') AS mx_hosts_json,
               row_number() OVER (
                 PARTITION BY ec.comparison_key
                 ORDER BY (ec.contact_id IS NOT NULL) DESC,coalesce(ec.quality_score,0) DESC,ec.last_seen_at DESC,ec.id
               ) AS ordinal
        FROM email_candidates ec
        LEFT JOIN domain_verification_cache d
          ON lower(d.domain)=lower(substr(ec.canonical_email,instr(ec.canonical_email,'@')+1))
        WHERE ec.canonical_email IS NOT NULL
          AND ec.routing_status='routing_ok'
          AND ec.syntax_status<>'invalid_syntax'
          AND ec.current_disposition NOT IN ('suppressed','blacklisted','invalid_syntax')
          AND NOT EXISTS (
            SELECT 1 FROM suppressions s
            WHERE s.comparison_key=ec.comparison_key AND s.lifted_at IS NULL
          )
    )
    SELECT id,contact_id,canonical_email,comparison_key,domain,priority,mx_hosts_json
    FROM ranked WHERE ordinal=1
    ORDER BY priority DESC,id LIMIT ?1").map_err(|e|e.to_string())?;
    let rows = stmt
        .query_map(params![limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, f64>(5)?,
                r.get::<_, String>(6)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);
    let mut inserted = 0;
    for (candidate, contact, email, key, domain, priority, mx_json) in rows {
        if crate::parsing::non_contact_mailbox_reason(&email).is_some() {
            continue;
        }
        let mut mx: Vec<String> = serde_json::from_str(&mx_json).unwrap_or_default();
        if mx.is_empty() {
            mx.push(domain.clone());
        }
        let changed = db.execute("INSERT OR IGNORE INTO mailbox_verification_tasks(id,job_id,contact_id,candidate_id,comparison_key,email,domain,mx_hosts_json,priority,status,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'queued',?10,?10)",params![new_contact_id(),job_id,contact,candidate,key,email,domain,serde_json::to_string(&mx).unwrap_or_else(|_|"[]".into()),priority,now()]).map_err(|e|e.to_string())?;
        inserted += changed;
    }
    Ok(inserted)
}

fn target_key(kind: &str, value: &str) -> String {
    format!(
        "{kind}:{}",
        value.trim_end_matches('.').to_ascii_lowercase()
    )
}

fn ready_mx_hosts(
    db: &Connection,
    domain: &str,
    mx_hosts: &[String],
) -> Result<Vec<String>, String> {
    let checked_at = now();
    let domain_key = target_key("domain", domain);
    let domain_blocked: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM mailbox_verification_target_state WHERE target_key=?1 AND cooldown_until>?2)",params![domain_key,checked_at],|r|r.get(0)).map_err(|e|e.to_string())?;
    if domain_blocked {
        return Ok(Vec::new());
    }
    let mut ready = Vec::new();
    for host in mx_hosts {
        let mx_key = target_key("mx", host);
        let blocked: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM mailbox_verification_target_state WHERE target_key=?1 AND cooldown_until>?2)",params![mx_key,checked_at],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !blocked {
            ready.push(host.trim_end_matches('.').to_ascii_lowercase());
        }
    }
    Ok(ready)
}

fn claim_task(db: &mut Connection, job_id: &str) -> Result<Option<Task>, String> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let checked_at = now();
    tx.execute("UPDATE mailbox_verification_tasks SET status='queued',leased_until=NULL,updated_at=?1 WHERE job_id=?2 AND status='leased' AND leased_until<=?1",params![checked_at,job_id]).map_err(|e|e.to_string())?;
    let skipped = tx.execute("UPDATE mailbox_verification_tasks
        SET status='not_attempted',outcome='suppressed_after_queue',leased_until=NULL,updated_at=?1
        WHERE job_id=?2 AND status='queued' AND (
          EXISTS (SELECT 1 FROM suppressions s WHERE s.comparison_key=mailbox_verification_tasks.comparison_key AND s.lifted_at IS NULL)
          OR NOT EXISTS (
            SELECT 1 FROM email_candidates ec
            WHERE ec.id=mailbox_verification_tasks.candidate_id
              AND ec.syntax_status<>'invalid_syntax'
              AND ec.current_disposition NOT IN ('suppressed','blacklisted','invalid_syntax')
          )
        )",params![checked_at,job_id]).map_err(|e|e.to_string())?;
    if skipped > 0 {
        tx.execute("UPDATE mailbox_verification_jobs SET total_queued=max(completed,total_queued-?1),updated_at=?2 WHERE id=?3",params![skipped as i64,checked_at,job_id]).map_err(|e|e.to_string())?;
    }
    type QueuedTaskRow = Option<(
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        String,
        i64,
    )>;
    let row: QueuedTaskRow = tx.query_row("SELECT t.id,t.job_id,t.contact_id,t.candidate_id,t.comparison_key,t.email,t.domain,t.mx_hosts_json,t.attempts
        FROM mailbox_verification_tasks t
        WHERE t.job_id=?1 AND t.status='queued'
          AND (t.next_attempt_at IS NULL OR t.next_attempt_at<=?2)
          AND NOT EXISTS (
            SELECT 1 FROM mailbox_verification_target_state s
            WHERE s.target_key='domain:' || lower(rtrim(t.domain,'.')) AND s.cooldown_until>?2
          )
          AND EXISTS (
            SELECT 1 FROM json_each(t.mx_hosts_json) mx
            WHERE NOT EXISTS (
              SELECT 1 FROM mailbox_verification_target_state s
              WHERE s.target_key='mx:' || lower(rtrim(mx.value,'.')) AND s.cooldown_until>?2
            )
          )
        ORDER BY t.priority DESC,t.created_at LIMIT 1",params![job_id,checked_at],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).optional().map_err(|e|e.to_string())?;
    let Some((id, jid, contact, candidate, key, email, domain, mx_json, attempts)) = row else {
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(None);
    };
    let mx: Vec<String> = serde_json::from_str(&mx_json).unwrap_or_default();
    let ready_mx = ready_mx_hosts(&tx, &domain, &mx)?;
    if ready_mx.is_empty() {
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(None);
    }
    tx.execute("UPDATE mailbox_verification_tasks SET status='leased',leased_until=?1,attempts=attempts+1,updated_at=?1 WHERE id=?2",params![(Utc::now()+Duration::minutes(10)).to_rfc3339(),id]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(Some(Task {
        id,
        job_id: jid,
        contact_id: contact,
        candidate_id: candidate,
        comparison_key: key,
        email,
        domain,
        mx_hosts: ready_mx,
        attempts,
    }))
}

fn smtp_response_code(response: &str) -> Option<i64> {
    response.get(0..3).and_then(|v| v.parse().ok())
}
fn smtp_write<S: Write>(stream: &mut S, line: &str) -> Result<(), String> {
    if line
        .chars()
        .any(|character| matches!(character, '\r' | '\n'))
    {
        return Err("smtp_command_contains_newline".to_owned());
    }
    stream
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(b"\r\n").map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())
}
fn smtp_read<S: Read>(stream: &mut S) -> Result<String, String> {
    let mut all = String::new();
    loop {
        let mut line = Vec::new();
        let mut skipped = 0_usize;
        loop {
            let mut byte = [0_u8; 1];
            let count = stream.read(&mut byte).map_err(|e| e.to_string())?;
            if count == 0 {
                return Err("smtp_eof".into());
            }
            if byte[0] != b'\n' && line.len() >= 4096 {
                // Over-long line: drop the rest of it so it is not read as the next reply line.
                skipped += 1;
                if skipped > 65_536 {
                    return Err("smtp_line_too_long".into());
                }
                continue;
            }
            line.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        all.push_str(&String::from_utf8_lossy(&line));
        if line.len() < 4 || line.get(3) != Some(&b'-') || all.len() >= 8192 {
            break;
        }
    }
    Ok(all)
}

fn classify_smtp(response: String) -> ProbeResult {
    let code = smtp_response_code(&response);
    let enhanced = response
        .split_whitespace()
        .find(|v| {
            v.chars().filter(|c| *c == '.').count() == 2
                && v.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
        .map(str::to_owned);
    let lower = response.to_ascii_lowercase();
    let enhanced_class = enhanced.as_deref().unwrap_or_default();
    let (outcome, error) = match code {
        Some(250) | Some(251) => ("accepted", None),
        Some(252) => ("unknown", Some("cannot_verify")),
        Some(550) | Some(551) | Some(553) => {
            if enhanced_class.starts_with("5.7.")
                || lower.contains("policy")
                || lower.contains("blocked")
                || lower.contains("spam")
            {
                ("policy_blocked", Some("policy"))
            } else if enhanced_class.starts_with("5.1.")
                || lower.contains("user unknown")
                || lower.contains("mailbox unavailable")
                || lower.contains("recipient address rejected")
            {
                ("rejected_permanent", Some("recipient_rejected"))
            } else {
                ("unknown", Some("ambiguous_5xx"))
            }
        }
        Some(c) if (400..500).contains(&c) => ("temporary", Some("smtp_4xx")),
        Some(c) if c >= 500 => ("policy_blocked", Some("smtp_5xx")),
        _ => ("unknown", Some("unclassified")),
    };
    ProbeResult {
        outcome,
        smtp_code: code,
        enhanced_code: enhanced,
        error_class: error.map(str::to_owned),
        response: Some(response.chars().take(512).collect()),
    }
}

fn protocol_failure(response: String, stage: &str) -> ProbeResult {
    let code = smtp_response_code(&response);
    let outcome = if code.is_some_and(|value| (400..500).contains(&value)) {
        "temporary"
    } else {
        "policy_blocked"
    };
    ProbeResult {
        outcome,
        smtp_code: code,
        enhanced_code: None,
        error_class: Some(format!("{stage}_rejected")),
        response: Some(response.chars().take(512).collect()),
    }
}

fn smtp_probe(host: &str, task: &Task, settings: &MailboxVerificationSettings) -> ProbeResult {
    let Some(ehlo) = settings.ehlo_hostname.as_deref() else {
        return ProbeResult {
            outcome: "unknown",
            smtp_code: None,
            enhanced_code: None,
            error_class: Some("identity_missing".into()),
            response: None,
        };
    };
    let Some(sender) = settings.envelope_sender.as_deref() else {
        return ProbeResult {
            outcome: "unknown",
            smtp_code: None,
            enhanced_code: None,
            error_class: Some("identity_missing".into()),
            response: None,
        };
    };
    if !crate::parsing::is_valid_email_syntax(sender)
        || !crate::parsing::is_valid_email_syntax(&task.email)
        || [ehlo, sender, task.email.as_str(), host]
            .iter()
            .any(|value| value.chars().any(|c| matches!(c, '\r' | '\n')))
    {
        return ProbeResult {
            outcome: "unknown",
            smtp_code: None,
            enhanced_code: None,
            error_class: Some("invalid_smtp_argument".into()),
            response: None,
        };
    }
    let timeout = StdDuration::from_secs(20);
    let address = format!("{host}:25");
    // The system resolver has no timeout of its own: bound lookup and connect together.
    let (found_tx, found_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let stream = address.to_socket_addrs().ok().and_then(|addrs| {
            // An MX record can name 127.0.0.1 or a LAN address: never probe those.
            addrs
                .filter(|addr| !crate::discovery::is_internal_ip(addr.ip()))
                .find_map(|addr| TcpStream::connect_timeout(&addr, timeout).ok())
        });
        let _ = found_tx.send(stream);
    });
    let socket = match found_rx
        .recv_timeout(StdDuration::from_secs(40))
        .ok()
        .flatten()
    {
        Some(s) => s,
        None => {
            return ProbeResult {
                outcome: "connection_failed",
                smtp_code: None,
                enhanced_code: None,
                error_class: Some("connect_failed".into()),
                response: None,
            };
        }
    };
    let stream = socket;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let mut stream = stream;
    let greeting = match smtp_read(&mut stream) {
        Ok(v) => v,
        Err(e) => {
            return ProbeResult {
                outcome: "connection_failed",
                smtp_code: None,
                enhanced_code: None,
                error_class: Some(format!("greeting:{e}")),
                response: None,
            };
        }
    };
    if smtp_response_code(&greeting) != Some(220) {
        return protocol_failure(greeting, "greeting");
    }
    if smtp_write(&mut stream, &format!("EHLO {ehlo}")).is_err() {
        return ProbeResult {
            outcome: "connection_failed",
            smtp_code: None,
            enhanced_code: None,
            error_class: Some("ehlo_write".into()),
            response: None,
        };
    }
    let ehlo_response = match smtp_read(&mut stream) {
        Ok(v) => v,
        Err(e) => {
            return ProbeResult {
                outcome: "connection_failed",
                smtp_code: None,
                enhanced_code: None,
                error_class: Some(format!("ehlo:{e}")),
                response: None,
            };
        }
    };
    if !smtp_response_code(&ehlo_response).is_some_and(|code| (200..300).contains(&code)) {
        return protocol_failure(ehlo_response, "ehlo");
    }
    if !ehlo_response.to_ascii_lowercase().contains("starttls") {
        return ProbeResult {
            outcome: "unknown",
            smtp_code: smtp_response_code(&ehlo_response),
            enhanced_code: None,
            error_class: Some("starttls_unavailable".into()),
            response: Some(ehlo_response.chars().take(512).collect()),
        };
    }
    if smtp_write(&mut stream, "STARTTLS").is_err() {
        return ProbeResult {
            outcome: "connection_failed",
            smtp_code: None,
            enhanced_code: None,
            error_class: Some("starttls_write".into()),
            response: None,
        };
    }
    let starttls = match smtp_read(&mut stream) {
        Ok(v) => v,
        Err(e) => {
            return ProbeResult {
                outcome: "connection_failed",
                smtp_code: None,
                enhanced_code: None,
                error_class: Some(format!("starttls:{e}")),
                response: None,
            };
        }
    };
    if smtp_response_code(&starttls) != Some(220) {
        return ProbeResult {
            outcome: "unknown",
            smtp_code: smtp_response_code(&starttls),
            enhanced_code: None,
            error_class: Some("starttls_rejected".into()),
            response: Some(starttls.chars().take(512).collect()),
        };
    }
    let connector = match TlsConnector::new() {
        Ok(v) => v,
        Err(e) => {
            return ProbeResult {
                outcome: "unknown",
                smtp_code: None,
                enhanced_code: None,
                error_class: Some(format!("tls_connector:{e}")),
                response: None,
            };
        }
    };
    let mut tls = match connector.connect(host, stream) {
        Ok(v) => v,
        Err(e) => {
            return ProbeResult {
                outcome: "unknown",
                smtp_code: None,
                enhanced_code: None,
                error_class: Some(format!("tls:{e}")),
                response: None,
            };
        }
    };
    if smtp_write(&mut tls, &format!("EHLO {ehlo}")).is_err() {
        return ProbeResult {
            outcome: "connection_failed",
            smtp_code: None,
            enhanced_code: None,
            error_class: Some("tls_ehlo_write".into()),
            response: None,
        };
    }
    let tls_ehlo = match smtp_read(&mut tls) {
        Ok(value) => value,
        Err(error) => {
            return ProbeResult {
                outcome: "connection_failed",
                smtp_code: None,
                enhanced_code: None,
                error_class: Some(format!("tls_ehlo:{error}")),
                response: None,
            };
        }
    };
    if !smtp_response_code(&tls_ehlo).is_some_and(|code| (200..300).contains(&code)) {
        return protocol_failure(tls_ehlo, "tls_ehlo");
    }
    if smtp_write(&mut tls, &format!("MAIL FROM:<{sender}>")).is_err() {
        return ProbeResult {
            outcome: "connection_failed",
            smtp_code: None,
            enhanced_code: None,
            error_class: Some("mail_from_write".into()),
            response: None,
        };
    }
    let mail = match smtp_read(&mut tls) {
        Ok(v) => v,
        Err(e) => {
            return ProbeResult {
                outcome: "connection_failed",
                smtp_code: None,
                enhanced_code: None,
                error_class: Some(format!("mail_from:{e}")),
                response: None,
            };
        }
    };
    if !smtp_response_code(&mail).is_some_and(|code| (200..300).contains(&code)) {
        return protocol_failure(mail, "mail_from");
    }
    let control = format!("probe-{}@{}", uuid::Uuid::new_v4().simple(), task.domain);
    if smtp_write(&mut tls, &format!("RCPT TO:<{control}>")).is_err() {
        return ProbeResult {
            outcome: "connection_failed",
            smtp_code: None,
            enhanced_code: None,
            error_class: Some("control_rcpt_write".into()),
            response: None,
        };
    }
    let control_result = match smtp_read(&mut tls) {
        Ok(v) => v,
        Err(e) => {
            return ProbeResult {
                outcome: "connection_failed",
                smtp_code: None,
                enhanced_code: None,
                error_class: Some(format!("control_rcpt:{e}")),
                response: None,
            };
        }
    };
    if matches!(smtp_response_code(&control_result), Some(250) | Some(251)) {
        let _ = smtp_write(&mut tls, "RSET");
        let _ = smtp_write(&mut tls, "QUIT");
        return ProbeResult {
            outcome: "catch_all",
            smtp_code: smtp_response_code(&control_result),
            enhanced_code: None,
            error_class: Some("catch_all_control_accepted".into()),
            response: Some(control_result.chars().take(512).collect()),
        };
    }
    let control_code = smtp_response_code(&control_result);
    if control_code == Some(252) {
        let _ = smtp_write(&mut tls, "RSET");
        let _ = smtp_write(&mut tls, "QUIT");
        return ProbeResult {
            outcome: "unknown",
            smtp_code: control_code,
            enhanced_code: None,
            error_class: Some("catch_all_control_ambiguous".into()),
            response: Some(control_result.chars().take(512).collect()),
        };
    }
    if control_code.is_some_and(|code| (400..500).contains(&code)) {
        let _ = smtp_write(&mut tls, "RSET");
        let _ = smtp_write(&mut tls, "QUIT");
        return protocol_failure(control_result, "catch_all_control");
    }
    if !control_code.is_some_and(|code| (500..600).contains(&code)) {
        let _ = smtp_write(&mut tls, "RSET");
        let _ = smtp_write(&mut tls, "QUIT");
        return ProbeResult {
            outcome: "unknown",
            smtp_code: control_code,
            enhanced_code: None,
            error_class: Some("catch_all_control_unclassified".into()),
            response: Some(control_result.chars().take(512).collect()),
        };
    }
    let control_classification = classify_smtp(control_result);
    if control_classification.outcome != "rejected_permanent" {
        let _ = smtp_write(&mut tls, "RSET");
        let _ = smtp_write(&mut tls, "QUIT");
        return control_classification;
    }
    if smtp_write(&mut tls, &format!("RCPT TO:<{}>", task.email)).is_err() {
        return ProbeResult {
            outcome: "connection_failed",
            smtp_code: None,
            enhanced_code: None,
            error_class: Some("rcpt_write".into()),
            response: None,
        };
    }
    let result = smtp_read(&mut tls)
        .map(classify_smtp)
        .unwrap_or(ProbeResult {
            outcome: "connection_failed",
            smtp_code: None,
            enhanced_code: None,
            error_class: Some("rcpt_read".into()),
            response: None,
        });
    let _ = smtp_write(&mut tls, "RSET");
    let _ = smtp_write(&mut tls, "QUIT");
    result
}

fn job_control(db: &Connection) -> Result<String, String> {
    db.query_row(
        "SELECT action FROM mailbox_verification_control WHERE id=1",
        [],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| e.to_string())
    .map(|v| v.unwrap_or_else(|| "paused".into()))
}

fn persist_probe(
    db: &mut Connection,
    task: &Task,
    result: &ProbeResult,
    duration_ms: i64,
) -> Result<(), String> {
    let attempted = now();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    tx.execute("INSERT INTO mailbox_verification_attempts(id,task_id,job_id,mx_host,port,transport,smtp_code,enhanced_code,outcome,error_class,response_excerpt,duration_ms,attempted_at) VALUES (?1,?2,?3,?4,25,'starttls',?5,?6,?7,?8,?9,?10,?11)",params![new_contact_id(),task.id,task.job_id,task.mx_hosts.first(),result.smtp_code,result.enhanced_code,result.outcome,result.error_class,result.response,duration_ms,attempted]).map_err(|e|e.to_string())?;
    let (mailbox_status, error_class) = match result.outcome {
        "accepted" => ("accepted", None),
        "rejected_permanent" => ("rejected", Some("recipient_rejected")),
        "temporary" => ("temporary", Some("temporary")),
        "catch_all" => ("catch_all", Some("catch_all")),
        "policy_blocked" => ("unknown", Some("policy")),
        "connection_failed" => ("unknown", Some("connection")),
        _ => ("unknown", result.error_class.as_deref()),
    };
    if let Some(candidate) = &task.candidate_id {
        tx.execute(
            "UPDATE email_candidates SET mailbox_status=?1,last_seen_at=?2 WHERE id=?3",
            params![mailbox_status, attempted, candidate],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(contact) = &task.contact_id {
        tx.execute("UPDATE contacts SET mailbox_status=?1,last_confirmed_at=CASE WHEN ?1='accepted' THEN ?2 ELSE last_confirmed_at END WHERE id=?3",params![mailbox_status,attempted,contact]).map_err(|e|e.to_string())?;
    }
    tx.execute("INSERT INTO verification_events(id,comparison_key,candidate_id,contact_id,method,provider,routing_status,mailbox_status,error_class,details_json,checked_at) VALUES (?1,?2,?3,?4,'smtp_starttls','direct_mx','routing_ok',?5,?6,?7,?8)",params![new_contact_id(),task.comparison_key,task.candidate_id,task.contact_id,mailbox_status,error_class,serde_json::json!({"outcome":result.outcome,"smtp_code":result.smtp_code,"enhanced_code":result.enhanced_code,"response":result.response,"transport":"STARTTLS","message_body_submitted":false,"mx_host":task.mx_hosts.first()}).to_string(),attempted]).map_err(|e|e.to_string())?;
    let final_outcome = result.outcome;
    let retry = matches!(final_outcome, "temporary" | "connection_failed") && task.attempts < 2;
    let status = if retry { "queued" } else { final_outcome };
    let next = if retry {
        Some(
            (Utc::now()
                + Duration::minutes(match task.attempts {
                    0 => 15,
                    1 => 60,
                    _ => 360,
                }))
            .to_rfc3339(),
        )
    } else {
        None
    };
    tx.execute("UPDATE mailbox_verification_tasks SET status=?1,next_attempt_at=?2,leased_until=NULL,outcome=?3,updated_at=?4 WHERE id=?5",params![status,next,final_outcome,attempted,task.id]).map_err(|e|e.to_string())?;
    let cooldown = if matches!(
        final_outcome,
        "temporary" | "connection_failed" | "policy_blocked"
    ) {
        900
    } else {
        60
    };
    if let Some(host) = task.mx_hosts.first() {
        tx.execute("INSERT INTO mailbox_verification_target_state(target_key,target_kind,cooldown_until,consecutive_failures,last_outcome,last_attempt_at,updated_at) VALUES (?1,'mx',?2,?3,?4,?5,?5) ON CONFLICT(target_key) DO UPDATE SET cooldown_until=excluded.cooldown_until,consecutive_failures=CASE WHEN excluded.consecutive_failures>0 THEN mailbox_verification_target_state.consecutive_failures+1 ELSE 0 END,last_outcome=excluded.last_outcome,last_attempt_at=excluded.last_attempt_at,updated_at=excluded.updated_at",params![target_key("mx",host),(Utc::now()+Duration::seconds(cooldown)).to_rfc3339(),i64::from(matches!(final_outcome,"temporary"|"connection_failed"|"policy_blocked")),final_outcome,attempted]).map_err(|e|e.to_string())?;
    }
    tx.execute("INSERT INTO mailbox_verification_target_state(target_key,target_kind,cooldown_until,consecutive_failures,last_outcome,last_attempt_at,updated_at) VALUES (?1,'domain',?2,?3,?4,?5,?5) ON CONFLICT(target_key) DO UPDATE SET cooldown_until=excluded.cooldown_until,consecutive_failures=CASE WHEN excluded.consecutive_failures>0 THEN mailbox_verification_target_state.consecutive_failures+1 ELSE 0 END,last_outcome=excluded.last_outcome,last_attempt_at=excluded.last_attempt_at,updated_at=excluded.updated_at",params![target_key("domain",&task.domain),(Utc::now()+Duration::seconds(cooldown)).to_rfc3339(),i64::from(matches!(final_outcome,"temporary"|"connection_failed"|"policy_blocked")),final_outcome,attempted]).map_err(|e|e.to_string())?;

    let mut propagated_catch_all = 0_usize;
    if final_outcome == "catch_all" && !retry {
        let mut queued = tx.prepare("SELECT id,candidate_id,contact_id,comparison_key FROM mailbox_verification_tasks WHERE job_id=?1 AND domain=?2 AND status='queued'").map_err(|e|e.to_string())?;
        let rows = queued
            .query_map(params![task.job_id, task.domain], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        drop(queued);
        for (task_id, candidate_id, contact_id, comparison_key) in rows {
            tx.execute("UPDATE mailbox_verification_tasks SET status='catch_all',outcome='catch_all',next_attempt_at=NULL,leased_until=NULL,updated_at=?1 WHERE id=?2",params![attempted,task_id]).map_err(|e|e.to_string())?;
            if let Some(candidate_id) = &candidate_id {
                tx.execute("UPDATE email_candidates SET mailbox_status='catch_all',last_seen_at=?1 WHERE id=?2",params![attempted,candidate_id]).map_err(|e|e.to_string())?;
            }
            if let Some(contact_id) = &contact_id {
                tx.execute(
                    "UPDATE contacts SET mailbox_status='catch_all' WHERE id=?1",
                    params![contact_id],
                )
                .map_err(|e| e.to_string())?;
            }
            tx.execute("INSERT INTO verification_events(id,comparison_key,candidate_id,contact_id,method,provider,routing_status,mailbox_status,error_class,details_json,checked_at) VALUES (?1,?2,?3,?4,'smtp_domain_control','direct_mx','routing_ok','catch_all','catch_all_inherited',?5,?6)",params![new_contact_id(),comparison_key,candidate_id,contact_id,serde_json::json!({"domain":task.domain,"control_task_id":task.id,"message_body_submitted":false}).to_string(),attempted]).map_err(|e|e.to_string())?;
            propagated_catch_all += 1;
        }
    }
    if !retry {
        let counter = match final_outcome {
            "accepted" => "accepted",
            "rejected_permanent" => "rejected",
            "temporary" => "temporary",
            "catch_all" => "catch_all",
            "policy_blocked" => "policy_blocked",
            _ => "unknown",
        };
        let increment = 1 + propagated_catch_all;
        let counter_increment = if final_outcome == "catch_all" {
            increment
        } else {
            1
        };
        let sql = format!(
            "UPDATE mailbox_verification_jobs SET completed=completed+?1,{counter}={counter}+?2,updated_at=?3 WHERE id=?4"
        );
        tx.execute(
            &sql,
            params![
                increment as i64,
                counter_increment as i64,
                attempted,
                task.job_id
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

fn finish_or_advance(db: &Connection, job_id: &str) -> Result<bool, String> {
    let (mode,stage,completed,_total,policy,unknown): (String,String,i64,i64,i64,i64)=db.query_row("SELECT mode,stage,completed,total_queued,policy_blocked,unknown FROM mailbox_verification_jobs WHERE id=?1",params![job_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).map_err(|e|e.to_string())?;
    let pending:i64=db.query_row("SELECT COUNT(*) FROM mailbox_verification_tasks WHERE job_id=?1 AND status IN ('queued','leased')",params![job_id],|r|r.get(0)).map_err(|e|e.to_string())?;
    if pending > 0 {
        return Ok(false);
    }
    if mode == "recommended" && stage == "pilot" && completed >= MAX_PILOT as i64 {
        let failures = policy + unknown;
        if failures * 5 > completed {
            db.execute("UPDATE mailbox_verification_jobs SET status='completed',stage='pilot',safety_gate='Pilot stopped: too many policy/unknown results',finished_at=?1,updated_at=?1 WHERE id=?2",params![now(),job_id]).map_err(|e|e.to_string())?;
            return Ok(true);
        }
        let db2 = db;
        let added = eligible_tasks(
            db2,
            job_id,
            MAX_RECOMMENDED.saturating_sub(completed as usize) as i64,
        )?;
        db.execute("UPDATE mailbox_verification_jobs SET stage='rollout',total_queued=total_queued+?1,updated_at=?2 WHERE id=?3",params![added as i64,now(),job_id]).map_err(|e|e.to_string())?;
        return Ok(false);
    }
    db.execute("UPDATE mailbox_verification_jobs SET status='completed',stage='complete',finished_at=?1,updated_at=?1 WHERE id=?2",params![now(),job_id]).map_err(|e|e.to_string())?;
    Ok(true)
}

async fn run_job(app: tauri::AppHandle, job_id: String) {
    let mut last_probe = Instant::now() - StdDuration::from_secs(60 / DEFAULT_RATE);
    loop {
        let app_read = app.clone();
        let state = tauri::async_runtime::spawn_blocking(move || {
            let db = open_db(&app_read)?;
            Ok::<_, String>((job_control(&db)?, load_settings(&db)?))
        })
        .await;
        let Ok(Ok((control, settings))) = state else {
            if let Ok(db) = open_db(&app) {
                let _ = db.execute("UPDATE mailbox_verification_jobs SET status='failed',last_error='Unable to load durable worker state',finished_at=?1,updated_at=?1 WHERE id=?2 AND status='running'",params![now(),job_id]);
                let _ = db.execute("UPDATE mailbox_verification_tasks SET status='queued',leased_until=NULL,updated_at=?1 WHERE job_id=?2 AND status='leased'",params![now(),job_id]);
            }
            break;
        };
        if control == "cancelled" {
            if let Ok(db) = open_db(&app) {
                let _=db.execute("UPDATE mailbox_verification_jobs SET status='cancelled',finished_at=?1,updated_at=?1 WHERE id=?2",params![now(),job_id]);
            }
            break;
        }
        if control == "paused" {
            if let Ok(db) = open_db(&app) {
                let _=db.execute("UPDATE mailbox_verification_jobs SET status='paused',updated_at=?1 WHERE id=?2 AND status IN ('running','preparing')",params![now(),job_id]);
            }
            tokio::time::sleep(StdDuration::from_secs(2)).await;
            continue;
        }
        let app_claim = app.clone();
        let jid = job_id.clone();
        let task = tauri::async_runtime::spawn_blocking(move || {
            let mut db = open_db(&app_claim)?;
            claim_task(&mut db, &jid)
        })
        .await;
        let Ok(Ok(Some(task))) = task else {
            let app_wait = app.clone();
            let jid = job_id.clone();
            let done = tauri::async_runtime::spawn_blocking(move || {
                let db = open_db(&app_wait)?;
                finish_or_advance(&db, &jid)
            })
            .await
            .ok()
            .and_then(|v| v.ok())
            .unwrap_or(false);
            if done {
                break;
            }
            tokio::time::sleep(StdDuration::from_secs(5)).await;
            continue;
        };
        let interval =
            StdDuration::from_secs_f64(60.0 / (settings.global_rate_per_minute.max(1) as f64));
        if let Some(wait) = interval.checked_sub(last_probe.elapsed()) {
            tokio::time::sleep(wait).await;
        }
        last_probe = Instant::now();
        let start = Instant::now();
        let mut probe_task = task.clone();
        probe_task.mx_hosts.truncate(1);
        let probe_settings = settings.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            if probe_task.mx_hosts.is_empty() {
                return (
                    probe_task,
                    ProbeResult {
                        outcome: "unknown",
                        smtp_code: None,
                        enhanced_code: None,
                        error_class: Some("no_mx_host".into()),
                        response: None,
                    },
                );
            }
            let result = smtp_probe(&probe_task.mx_hosts[0], &probe_task, &probe_settings);
            (probe_task, result)
        })
        .await;
        if let Ok((persisted_task, result)) = result {
            let app_write = app.clone();
            let persisted = tauri::async_runtime::spawn_blocking(move || {
                let mut db = open_db(&app_write)?;
                persist_probe(
                    &mut db,
                    &persisted_task,
                    &result,
                    start.elapsed().as_millis() as i64,
                )
            })
            .await;
            if !matches!(persisted, Ok(Ok(()))) {
                if let Ok(db) = open_db(&app) {
                    let _ = db.execute("UPDATE mailbox_verification_jobs SET status='failed',last_error='Unable to persist SMTP evidence',finished_at=?1,updated_at=?1 WHERE id=?2",params![now(),job_id]);
                    let _ = db.execute("UPDATE mailbox_verification_tasks SET status='queued',leased_until=NULL,updated_at=?1 WHERE job_id=?2 AND status='leased'",params![now(),job_id]);
                }
                break;
            }
        } else {
            if let Ok(db) = open_db(&app) {
                let _ = db.execute("UPDATE mailbox_verification_jobs SET status='failed',last_error='SMTP worker panicked',finished_at=?1,updated_at=?1 WHERE id=?2",params![now(),job_id]);
                let _ = db.execute("UPDATE mailbox_verification_tasks SET status='queued',leased_until=NULL,updated_at=?1 WHERE job_id=?2 AND status='leased'",params![now(),job_id]);
            }
            break;
        }
    }
    MAILBOX_WORKER_STARTED.store(false, Ordering::SeqCst);
}

#[tauri::command]
pub(crate) async fn start_mailbox_verification(
    app: tauri::AppHandle,
    mode: String,
) -> Result<MailboxVerificationStatus, String> {
    if mode != "recommended" && mode != "everything" {
        return Err("Modalità verifica non riconosciuta".into());
    }
    let settings_db = open_db(&app)?;
    let settings = load_settings(&settings_db)?;
    if settings.ehlo_hostname.is_none() || settings.envelope_sender.is_none() {
        return Err("Configura prima hostname EHLO e mittente envelope".into());
    }
    {
        let db = open_db(&app)?;
        let active: i64 = db.query_row("SELECT COUNT(*) FROM mailbox_verification_jobs WHERE status IN ('preparing','running','paused')",[],|r|r.get(0)).map_err(|e|e.to_string())?;
        if active > 0 {
            return Err("È già presente una verifica SMTP".into());
        }
    }
    let _ = prepare_dns(&app, false).await?;
    let mut db = open_db(&app)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    if tx.query_row("SELECT COUNT(*) FROM mailbox_verification_jobs WHERE status IN ('preparing','running','paused')",[],|r|r.get::<_,i64>(0)).map_err(|e|e.to_string())? > 0 {return Err("È già presente una verifica SMTP".into());}
    let id = new_contact_id();
    let created = now();
    tx.execute("INSERT INTO mailbox_verification_jobs(id,mode,status,stage,created_at,started_at,updated_at,settings_json) VALUES (?1,?2,'running',CASE WHEN ?2='recommended' THEN 'pilot' ELSE 'everything' END,?3,?3,?3,?4)",params![id,mode,created,serde_json::to_string(&settings).unwrap_or_else(|_|"{}".into())]).map_err(|e|e.to_string())?;
    let added = if mode == "everything" {
        eligible_tasks(&tx, &id, i64::MAX)?
    } else {
        eligible_tasks(&tx, &id, MAX_PILOT as i64)?
    };
    tx.execute(
        "UPDATE mailbox_verification_jobs SET total_queued=?1 WHERE id=?2",
        params![added as i64, id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE mailbox_verification_control SET action='running',updated_at=?1 WHERE id=1",
        params![created],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    let app_worker = app.clone();
    let worker_id = id.clone();
    if MAILBOX_WORKER_STARTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        let db = open_db(&app)?;
        db.execute("UPDATE mailbox_verification_jobs SET status='paused',last_error='A verification worker is already active',updated_at=?1 WHERE id=?2",params![now(),id]).map_err(|e|e.to_string())?;
        return load_status(&db, &id)?
            .ok_or_else(|| "Verifica creata ma stato non disponibile".into());
    }
    tauri::async_runtime::spawn(run_job(app_worker, worker_id));
    load_status(&db, &id)?.ok_or_else(|| "Verifica creata ma stato non disponibile".into())
}

#[tauri::command]
pub(crate) async fn control_mailbox_verification(
    app: tauri::AppHandle,
    action: String,
) -> Result<(), String> {
    crate::database::blocking(move || {
        if !matches!(action.as_str(), "pause" | "resume" | "cancel") {
            return Err("Azione verifica non riconosciuta".into());
        }
        let db = open_db(&app)?;
        let state = if action == "resume" {
            "running"
        } else if action == "cancel" {
            "cancelled"
        } else {
            "paused"
        };
        db.execute(
            "UPDATE mailbox_verification_control SET action=?1,updated_at=?2 WHERE id=1",
            params![state, now()],
        )
        .map_err(|e| e.to_string())?;
        db.execute("UPDATE mailbox_verification_jobs SET status=CASE WHEN ?1='paused' THEN 'paused' WHEN ?1='cancelled' THEN 'cancelled' WHEN ?1='running' THEN 'running' ELSE status END,finished_at=CASE WHEN ?1='running' THEN NULL ELSE finished_at END,last_error=CASE WHEN ?1='running' THEN NULL ELSE last_error END,updated_at=?2 WHERE status IN ('preparing','running','paused')",params![state,now()]).map_err(|e|e.to_string())?;
        if state == "running" && !MAILBOX_WORKER_STARTED.swap(true, Ordering::SeqCst) {
            if let Some(job_id) = db.query_row("SELECT id FROM mailbox_verification_jobs WHERE status='running' ORDER BY created_at DESC LIMIT 1",[],|r|r.get::<_,String>(0)).optional().map_err(|e|e.to_string())? {
                tauri::async_runtime::spawn(run_job(app.clone(), job_id));
            } else {
                MAILBOX_WORKER_STARTED.store(false, Ordering::SeqCst);
            }
        }
        Ok(())
    })
    .await
}

pub(crate) fn pause_running_jobs(db: &Connection) -> Result<usize, String> {
    let n=db.execute("UPDATE mailbox_verification_jobs SET status='paused',last_error='Application closed; resume explicitly',updated_at=?1 WHERE status IN ('preparing','running')",params![now()]).map_err(|e|e.to_string())?;
    db.execute(
        "UPDATE mailbox_verification_control SET action='paused',updated_at=?1 WHERE id=1",
        params![now()],
    )
    .map_err(|e| e.to_string())?;
    db.execute("UPDATE mailbox_verification_tasks SET status='queued',leased_until=NULL,updated_at=?1 WHERE status='leased'",params![now()]).map_err(|e|e.to_string())?;
    Ok(n)
}

pub(crate) fn cancel_jobs_for_database_clear(db: &Connection) -> Result<(), String> {
    let timestamp = now();
    db.execute(
        "UPDATE mailbox_verification_control SET action='cancelled',updated_at=?1 WHERE id=1",
        params![timestamp],
    )
    .map_err(|e| e.to_string())?;
    db.execute("UPDATE mailbox_verification_jobs SET status='cancelled',last_error='Database cleared by user',finished_at=?1,updated_at=?1 WHERE status IN ('preparing','running','paused')",params![timestamp]).map_err(|e|e.to_string())?;
    Ok(())
}

pub(crate) fn normalize_jobs_on_startup(db: &Connection) -> Result<usize, String> {
    if MAILBOX_STARTUP_NORMALIZED.swap(true, Ordering::SeqCst) {
        return Ok(0);
    }
    pause_running_jobs(db)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smtp_read_drops_the_tail_of_an_overlong_line() {
        let mut data = b"250-".to_vec();
        data.extend(std::iter::repeat_n(b'x', 5000));
        data.extend_from_slice(b"\r\n250 OK\r\n");
        let reply = smtp_read(&mut std::io::Cursor::new(data)).unwrap();
        assert!(reply.ends_with("250 OK\r\n"), "{}", reply.len());
        assert_eq!(smtp_response_code(&reply), Some(250));
        let endless = std::io::repeat(b'x');
        assert!(smtp_read(&mut { endless }).is_err());
    }

    #[test]
    fn smtp_outcomes_are_conservative() {
        assert_eq!(classify_smtp("250 2.1.5 Ok\r\n".into()).outcome, "accepted");
        assert_eq!(
            classify_smtp("252 2.1.5 Cannot VRFY user\r\n".into()).outcome,
            "unknown"
        );
        assert_eq!(
            classify_smtp("550 5.1.1 User unknown\r\n".into()).outcome,
            "rejected_permanent"
        );
        assert_eq!(
            classify_smtp("450 4.7.1 Try again later\r\n".into()).outcome,
            "temporary"
        );
        assert_eq!(
            classify_smtp("550 5.7.1 Policy blocked\r\n".into()).outcome,
            "policy_blocked"
        );
    }

    #[test]
    fn smtp_settings_require_tls_and_valid_identity() {
        let rejected = validate_settings(MailboxVerificationSettingsInput {
            ehlo_hostname: Some("mailer.example.test".into()),
            envelope_sender: Some("probe@example.test".into()),
            starttls_required: Some(false),
            global_rate_per_minute: None,
            max_concurrency: None,
        });
        assert!(rejected.is_err());
        let accepted = validate_settings(MailboxVerificationSettingsInput {
            ehlo_hostname: Some("mailer.example.test".into()),
            envelope_sender: Some("probe@example.test".into()),
            starttls_required: Some(true),
            global_rate_per_minute: Some(5),
            max_concurrency: Some(1),
        })
        .expect("valid settings");
        assert!(accepted.starttls_required);
    }

    #[test]
    fn smtp_identity_and_command_lines_reject_injection() {
        let invalid = validate_settings(MailboxVerificationSettingsInput {
            ehlo_hostname: Some("mailer.example.test;QUIT".into()),
            envelope_sender: Some("probe@example.test".into()),
            starttls_required: Some(true),
            global_rate_per_minute: None,
            max_concurrency: Some(1),
        });
        assert!(invalid.is_err());

        let mut buffer = Vec::new();
        assert!(smtp_write(&mut buffer, "EHLO safe.example.test\r\nQUIT").is_err());
        assert!(buffer.is_empty());
    }

    #[test]
    fn ambiguous_recipient_rejections_are_not_declared_missing() {
        assert_eq!(
            classify_smtp("550 5.7.1 Access denied\r\n".into()).outcome,
            "policy_blocked"
        );
        assert_eq!(
            classify_smtp("550 5.1.1 User unknown\r\n".into()).outcome,
            "rejected_permanent"
        );
        assert_eq!(
            classify_smtp("550 Requested action not taken\r\n".into()).outcome,
            "unknown"
        );
        assert_eq!(
            protocol_failure("450 4.7.1 Try later\r\n".into(), "mail_from").outcome,
            "temporary"
        );
    }
}

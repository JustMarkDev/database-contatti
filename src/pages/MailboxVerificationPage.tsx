import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "../components/ui/toast";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "../components/ui/alert-dialog";
import { Badge } from "../components/ui/badge";
import { ScrollIndicators, useScrollActivity } from "../components/ScrollIndicators";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardHeader } from "../components/ui/card";
import { Input } from "../components/ui/input";
import { Icon } from "../components/Icon";
import { Progress } from "../components/ui/progress";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "../components/ui/table";
import {
  controlMailboxVerification,
  getMailboxVerificationOverview,
  getMailboxVerificationStatus,
  revalidateMailboxDns,
  saveMailboxVerificationSettings,
  startMailboxVerification,
} from "../api/mailboxVerification";
import { listCandidates } from "../api/candidates";
import type {
  Candidate,
  MailboxDnsRevalidationReport,
  MailboxVerificationOverview,
  MailboxVerificationSettings,
  MailboxVerificationStatus,
} from "../types";

type Props = {
  active: boolean;
  /** DNS revalidation promotes and separates contacts: the contacts view must reload. */
  onContactsChanged: () => void;
};

const CANDIDATE_PAGE_SIZE = 100;

const emptySettings: MailboxVerificationSettings = {
  ehlo_hostname: "",
  envelope_sender: "",
  require_starttls: true,
};

const emptyOverview: MailboxVerificationOverview = {
  eligible_addresses: 0,
  eligible_domains: 0,
  eligible_mx_groups: 0,
  excluded_blacklisted: 0,
  excluded_suppressed: 0,
  excluded_non_contact: 0,
  excluded_no_routing: 0,
  temporary_dns_failures: 0,
  preflight_ok: false,
  preflight_message: null,
  settings: null,
  status: null,
  last_completed_at: null,
};

const emptyStatus: MailboxVerificationStatus = {
  id: null,
  mode: null,
  state: "idle",
  phase: "",
  processed: 0,
  total: 0,
  accepted: 0,
  rejected: 0,
  temporary: 0,
  catch_all: 0,
  policy_blocked: 0,
  unknown: 0,
  dns_revalidated: 0,
  started_at: null,
  updated_at: null,
  eta_seconds: null,
  message: null,
  error: null,
};

function numberValue(value: unknown, fallback = 0) {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function normalizeStatus(value: unknown): MailboxVerificationStatus | null {
  if (!value || typeof value !== "object") return null;
  const raw = value as Partial<MailboxVerificationStatus>;
  const backend = value as {
    status?: unknown;
    stage?: unknown;
    total_queued?: unknown;
    completed?: unknown;
    last_error?: unknown;
  };
  const candidateState = raw.state ?? backend.status ?? "idle";
  const rawState = typeof candidateState === "string" ? candidateState : "idle";
  const state = [
    "idle",
    "preparing",
    "running",
    "pausing",
    "paused",
    "completed",
    "cancelled",
    "failed",
  ].includes(rawState)
    ? (rawState as MailboxVerificationStatus["state"])
    : rawState === "pending"
      ? "preparing"
      : "idle";
  return {
    ...emptyStatus,
    ...raw,
    state,
    mode: raw.mode === "recommended" || raw.mode === "everything" ? raw.mode : null,
    phase:
      typeof raw.phase === "string"
        ? raw.phase
        : typeof backend.stage === "string"
          ? backend.stage
          : "",
    processed: numberValue(raw.processed, numberValue(backend.completed)),
    total: numberValue(raw.total, numberValue(backend.total_queued)),
    accepted: numberValue(raw.accepted),
    rejected: numberValue(raw.rejected),
    temporary: numberValue(raw.temporary),
    catch_all: numberValue(raw.catch_all),
    policy_blocked: numberValue(raw.policy_blocked),
    unknown: numberValue(raw.unknown),
    dns_revalidated: numberValue(raw.dns_revalidated),
    error:
      typeof raw.error === "string"
        ? raw.error
        : typeof backend.last_error === "string"
          ? backend.last_error
          : null,
  };
}

function normalizeOverview(value: unknown): MailboxVerificationOverview {
  if (!value || typeof value !== "object") return emptyOverview;
  const raw = value as Partial<MailboxVerificationOverview>;
  const backend = value as {
    eligible_contacts?: unknown;
    eligible_domains?: unknown;
    eligible_mx_hosts?: unknown;
    non_routable_contacts?: unknown;
    suppressed_contacts?: unknown;
    blacklisted_contacts?: unknown;
    non_contact_mailboxes?: unknown;
    temporary_dns_failures?: unknown;
    preflight_message?: unknown;
    last_completed_at?: unknown;
    active_job?: unknown;
  };
  const rawSettings = raw.settings;
  const settings = rawSettings
    ? {
        ...emptySettings,
        ehlo_hostname: rawSettings.ehlo_hostname ?? "",
        envelope_sender: rawSettings.envelope_sender ?? "",
        require_starttls: rawSettings.require_starttls ?? rawSettings.starttls_required ?? true,
        starttls_required: rawSettings.starttls_required,
        global_rate_per_minute: rawSettings.global_rate_per_minute,
        max_concurrency: rawSettings.max_concurrency,
      }
    : null;
  const eligibleAddresses = numberValue(
    raw.eligible_addresses,
    numberValue(backend.eligible_contacts),
  );
  const backendPreflight = (value as { preflight_ok?: unknown }).preflight_ok;
  return {
    ...emptyOverview,
    ...raw,
    eligible_addresses: eligibleAddresses,
    eligible_domains: numberValue(raw.eligible_domains, numberValue(backend.eligible_domains)),
    eligible_mx_groups: numberValue(raw.eligible_mx_groups, numberValue(backend.eligible_mx_hosts)),
    excluded_blacklisted: numberValue(
      raw.excluded_blacklisted,
      numberValue(backend.blacklisted_contacts),
    ),
    excluded_suppressed: numberValue(
      raw.excluded_suppressed,
      numberValue(backend.suppressed_contacts),
    ),
    excluded_non_contact: numberValue(
      raw.excluded_non_contact,
      numberValue(backend.non_contact_mailboxes),
    ),
    excluded_no_routing: numberValue(
      raw.excluded_no_routing,
      numberValue(backend.non_routable_contacts),
    ),
    temporary_dns_failures: numberValue(
      raw.temporary_dns_failures,
      numberValue(backend.temporary_dns_failures),
    ),
    preflight_ok:
      typeof backendPreflight === "boolean"
        ? backendPreflight
        : Boolean(settings?.ehlo_hostname && settings.envelope_sender && eligibleAddresses > 0),
    preflight_message:
      raw.preflight_message ??
      (typeof backend.preflight_message === "string" ? backend.preflight_message : null),
    settings,
    status: normalizeStatus(raw.status ?? backend.active_job),
    last_completed_at:
      raw.last_completed_at ??
      (typeof backend.last_completed_at === "string" ? backend.last_completed_at : null),
  };
}

function isValidHostname(value: string) {
  return /^(?=.{1,253}$)(?!-)(?:[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,63}$/.test(
    value.trim(),
  );
}

function isValidEmail(value: string) {
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(value.trim());
}

function formatNumber(value: number) {
  return value.toLocaleString("it-IT");
}

function formatEta(seconds: number | null) {
  if (seconds === null || !Number.isFinite(seconds) || seconds < 0) return "—";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  if (hours < 48) return `${hours} h ${remainingMinutes} min`;
  return `${Math.floor(hours / 24)} giorni`;
}

function stateLabel(state: MailboxVerificationStatus["state"]) {
  return {
    idle: "Nessuna verifica",
    preparing: "Preparazione",
    running: "In esecuzione",
    pausing: "Pausa in corso",
    paused: "In pausa",
    completed: "Completato",
    cancelled: "Annullato",
    failed: "Errore",
  }[state];
}

function displayStatus(status: MailboxVerificationStatus | null) {
  return status ?? emptyStatus;
}

export function MailboxVerificationPage({ active, onContactsChanged }: Props) {
  const [overview, setOverview] = useState<MailboxVerificationOverview>(emptyOverview);
  const [settings, setSettings] = useState<MailboxVerificationSettings>(emptySettings);
  const [savedSettings, setSavedSettings] = useState<MailboxVerificationSettings | null>(null);
  const [status, setStatus] = useState<MailboxVerificationStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [action, setAction] = useState<string | null>(null);
  const [dnsReport, setDnsReport] = useState<MailboxDnsRevalidationReport | null>(null);
  const [probeEverythingOpen, setProbeEverythingOpen] = useState(false);

  const [candidates, setCandidates] = useState<Candidate[]>([]);
  const [candidateTotal, setCandidateTotal] = useState(0);
  const [candidateOffset, setCandidateOffset] = useState(0);
  const [candidateQuery, setCandidateQuery] = useState("");
  const [candidateLoading, setCandidateLoading] = useState(false);
  const [candidateLoaded, setCandidateLoaded] = useState(false);

  const generation = useRef(0);
  const candidateGeneration = useRef(0);
  const candidateScroll = useScrollActivity();
  const mounted = useRef(true);
  const settingsInitialized = useRef(false);
  const inFlight = useRef(false);

  const refresh = useCallback(async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    const current = ++generation.current;
    setLoading(true);
    try {
      const [overviewResult, statusResult] = await Promise.all([
        getMailboxVerificationOverview(),
        getMailboxVerificationStatus(),
      ]);
      if (!mounted.current || current !== generation.current) return;
      const nextOverview = normalizeOverview(overviewResult);
      const nextStatus = normalizeStatus(statusResult) ?? nextOverview.status;
      setOverview(nextOverview);
      setStatus(nextStatus);
      if (nextOverview.settings && !settingsInitialized.current) {
        setSettings({ ...emptySettings, ...nextOverview.settings, require_starttls: true });
        setSavedSettings({ ...emptySettings, ...nextOverview.settings, require_starttls: true });
        settingsInitialized.current = true;
      }
    } catch (error) {
      if (mounted.current && current === generation.current) toast.error(String(error));
    } finally {
      inFlight.current = false;
      if (mounted.current && current === generation.current) setLoading(false);
    }
  }, []);

  const loadRecoverable = useCallback(async () => {
    const current = ++candidateGeneration.current;
    setCandidateLoading(true);
    try {
      const result = await listCandidates(
        { query: candidateQuery, status: "", disposition: "" },
        candidateOffset,
      );
      if (mounted.current && current === candidateGeneration.current) {
        setCandidates(Array.isArray(result?.candidates) ? result.candidates : []);
        setCandidateTotal(typeof result?.total === "number" ? result.total : 0);
        setCandidateLoaded(true);
      }
    } catch (error) {
      if (mounted.current && current === candidateGeneration.current) toast.error(String(error));
    } finally {
      if (mounted.current && current === candidateGeneration.current) setCandidateLoading(false);
    }
  }, [candidateQuery, candidateOffset]);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      generation.current += 1;
    };
  }, []);

  useEffect(() => {
    if (!active) return;
    void refresh();
    void loadRecoverable();
    return () => {
      candidateGeneration.current += 1;
    };
  }, [active, refresh, loadRecoverable]);

  useEffect(() => {
    if (!active) return;
    const state = status?.state;
    const shouldPoll =
      state === "preparing" || state === "running" || state === "pausing" || state === "paused";
    if (!shouldPoll) return;
    const interval = window.setInterval(() => void refresh(), 5_000);
    return () => {
      window.clearInterval(interval);
    };
  }, [active, status?.state, refresh]);

  const settingsValid =
    isValidHostname(settings.ehlo_hostname) && isValidEmail(settings.envelope_sender);
  const settingsUnchanged =
    Boolean(savedSettings) &&
    isValidHostname(savedSettings?.ehlo_hostname ?? "") &&
    isValidEmail(savedSettings?.envelope_sender ?? "") &&
    savedSettings?.ehlo_hostname === settings.ehlo_hostname.trim() &&
    savedSettings?.envelope_sender === settings.envelope_sender.trim() &&
    savedSettings?.require_starttls === true;
  const canStart = settingsValid && settingsUnchanged && overview.preflight_ok && !action;
  const currentStatus = displayStatus(status);
  const jobActive = ["preparing", "running", "pausing"].includes(currentStatus.state);
  const jobPaused = currentStatus.state === "paused";
  const progress =
    currentStatus.total > 0
      ? Math.min(100, (currentStatus.processed / currentStatus.total) * 100)
      : 0;

  const updateSettings = (key: keyof MailboxVerificationSettings, value: string | boolean) => {
    setSettings((current) => ({ ...current, [key]: value }));
  };

  const saveSettings = async () => {
    if (!settingsValid) return;
    setSaving(true);
    try {
      const next = {
        ...settings,
        ehlo_hostname: settings.ehlo_hostname.trim(),
        envelope_sender: settings.envelope_sender.trim(),
        require_starttls: true,
      };
      await saveMailboxVerificationSettings(next);
      setSettings(next);
      setSavedSettings(next);
      settingsInitialized.current = true;
      toast.success("Identità SMTP salvata");
      await refresh();
    } catch (error) {
      toast.error(String(error));
    } finally {
      setSaving(false);
    }
  };

  const start = async (mode: "recommended" | "everything") => {
    setAction(mode);
    try {
      const next = await startMailboxVerification(mode);
      setStatus(normalizeStatus(next));
      toast.success(
        mode === "recommended" ? "Verifica graduale avviata" : "Verifica completa avviata",
      );
    } catch (error) {
      toast.error(String(error));
    } finally {
      setAction(null);
      onContactsChanged();
      void refresh();
    }
  };

  const runControl = async (nextAction: "pause" | "resume" | "cancel") => {
    setAction(nextAction);
    try {
      await controlMailboxVerification(nextAction);
      await refresh();
    } catch (error) {
      toast.error(String(error));
    } finally {
      setAction(null);
    }
  };

  const revalidateDns = async () => {
    setAction("dns");
    try {
      const next = await revalidateMailboxDns();
      setDnsReport(next);
      toast.success(
        `DNS completato: ${formatNumber(next.domains_revalidated)} domini ricontrollati, ${formatNumber(next.promoted_contacts)} contatti recuperati`,
      );
    } catch (error) {
      toast.error(String(error));
    } finally {
      setAction(null);
      onContactsChanged();
      void refresh();
      void loadRecoverable();
    }
  };

  const summary = useMemo(
    () =>
      [
        ["Indirizzi eleggibili", overview.eligible_addresses],
        ["Domini distinti", overview.eligible_domains],
        ["DNS temporaneo (Recuperabili)", overview.temporary_dns_failures],
        ["Gruppi MX", overview.eligible_mx_groups],
      ] as const,
    [overview],
  );

  if (!active) return null;

  return (
    <section className="content-island" aria-label="Verifica caselle">
      <div className="island-section island-section-header">
        <header className="topbar">
          <div>
            <h1>Verifica caselle</h1>
            <p className="topbar-description">
              Rivalida gli indirizzi con DNS temporaneo per recuperarli come contatti ed esegui la
              verifica delle caselle via SMTP.
            </p>
          </div>
          <div className="header-metrics">
            <div>
              <span>Stato</span>
              <strong>{stateLabel(currentStatus.state)}</strong>
            </div>
          </div>
        </header>
      </div>

      <div className="island-section">
        <section className="stat-strip" aria-label="Riepilogo verifica">
          {summary.map(([label, value]) => (
            <div className="stat" key={label}>
              <span>{label}</span>
              <strong>{formatNumber(value)}</strong>
            </div>
          ))}
        </section>
      </div>

      <Card className="island-section verification-panel">
        <CardHeader className="section-heading section-heading-row">
          <div>
            <h2>Recupero DNS (Indirizzi con errore temporaneo)</h2>
            <p>
              Indirizzi esclusi durante l’importazione per timeout o indisponibilità temporanea del
              DNS. Con la rivalidazione, i domini che rispondono con record MX validi vengono
              automaticamente promossi a contatti attivi.
            </p>
          </div>
          <Button
            onClick={() => void revalidateDns()}
            disabled={!!action || jobActive || jobPaused}
          >
            {action === "dns" ? "Rivalidazione in corso…" : "Rivalida DNS"}
          </Button>
        </CardHeader>
        <CardContent>
          {dnsReport && (
            <div className="callout callout-success mb-3" role="status">
              Esito della rivalidazione DNS:{" "}
              <strong>{formatNumber(dnsReport.domains_revalidated)}</strong> domini ricontrollati ·{" "}
              <strong>{formatNumber(dnsReport.promoted_contacts)}</strong> contatti promossi ·{" "}
              <strong>{formatNumber(dnsReport.routing_ok)}</strong> con instradamento valido ·{" "}
              <strong>{formatNumber(dnsReport.temporary_failures)}</strong> ancora temporanei
            </div>
          )}
          <div
            className="search-field mb-3"
            role="search"
            aria-label="Filtra candidati recuperabili"
          >
            <Icon name="search" size={16} className="search-field-icon" />
            <Input
              className="search-wide pl-9"
              value={candidateQuery}
              onChange={(event) => {
                setCandidateQuery(event.currentTarget.value);
                setCandidateOffset(0);
              }}
              placeholder="Cerca email, dominio o file…"
              aria-label="Cerca candidati recuperabili"
            />
          </div>
          <div className="contacts-table-shell">
            <div
              ref={candidateScroll.ref}
              className="contacts-table-wrap"
              aria-busy={candidateLoading}
              onScroll={candidateScroll.onScroll}
            >
              <Table className="contacts-table candidates-table">
                <TableHeader>
                  <TableRow>
                    <TableHead>Email</TableHead>
                    <TableHead>Stato</TableHead>
                    <TableHead>File origine</TableHead>
                    <TableHead>Riga</TableHead>
                    <TableHead>Categoria</TableHead>
                    <TableHead>Data rilevamento</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {candidates.map((candidate) => (
                    <TableRow key={candidate.id}>
                      <TableCell className="cell-primary">
                        {candidate.canonical_email || candidate.raw_email || "—"}
                      </TableCell>
                      <TableCell>
                        <Badge variant="warning">DNS temporaneo</Badge>
                      </TableCell>
                      <TableCell>{candidate.source_file}</TableCell>
                      <TableCell>{candidate.source_row ?? "—"}</TableCell>
                      <TableCell>{candidate.category ?? "—"}</TableCell>
                      <TableCell>{candidate.created_at || candidate.last_seen_at || "—"}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
            <ScrollIndicators activity={candidateScroll} />
          </div>
          {candidateLoading && (
            <p className="table-status" role="status">
              Caricamento candidati recuperabili…
            </p>
          )}
          {candidateLoaded && !candidateLoading && !candidates.length && (
            <p className="table-empty-state">
              Nessun indirizzo recuperabile corrisponde alla ricerca.
            </p>
          )}
          <div className="table-toolbar">
            <span>
              {candidateTotal
                ? `${candidateOffset + 1}–${Math.min(candidateOffset + candidates.length, candidateTotal)} di ${candidateTotal.toLocaleString("it-IT")} recuperabili`
                : "Nessun indirizzo con errore DNS temporaneo presente nel database."}
            </span>
            {candidateTotal > CANDIDATE_PAGE_SIZE && (
              <div className="actions-row">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    setCandidateOffset((value) => Math.max(0, value - CANDIDATE_PAGE_SIZE))
                  }
                  disabled={candidateLoading || candidateOffset === 0}
                >
                  Precedente
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setCandidateOffset((value) => value + CANDIDATE_PAGE_SIZE)}
                  disabled={
                    candidateLoading || candidateOffset + candidates.length >= candidateTotal
                  }
                >
                  Successiva
                </Button>
              </div>
            )}
          </div>
        </CardContent>
      </Card>

      <Card className="island-section verification-panel">
        <CardHeader className="section-heading">
          <div>
            <h2>Identità SMTP</h2>
            <p>
              Il server remoto deve poter identificare chi sta eseguendo la verifica. STARTTLS è
              obbligatorio: senza una connessione TLS funzionante il risultato resta sconosciuto.
            </p>
          </div>
          <Badge variant={settingsUnchanged ? "default" : "warning"}>
            {settingsUnchanged ? "Salvata" : "Da configurare"}
          </Badge>
        </CardHeader>
        <CardContent>
          <div className="verification-settings-grid">
            <label className="filter-group">
              Hostname EHLO
              <Input
                value={settings.ehlo_hostname}
                onChange={(event) => updateSettings("ehlo_hostname", event.currentTarget.value)}
                placeholder="verifier.example.it"
                aria-label="Hostname EHLO"
                aria-describedby="ehlo-help"
                autoComplete="off"
              />
              <small id="ehlo-help">Un hostname completo, senza schema o porta.</small>
            </label>
            <label className="filter-group">
              Mittente envelope
              <Input
                type="email"
                value={settings.envelope_sender}
                onChange={(event) => updateSettings("envelope_sender", event.currentTarget.value)}
                placeholder="verifier@example.it"
                aria-label="Mittente envelope"
                aria-describedby="sender-help"
                autoComplete="off"
              />
              <small id="sender-help">
                Usato solo nel comando SMTP MAIL FROM; non viene inviato alcun messaggio.
              </small>
            </label>
          </div>
          <div className="verification-tls-note" role="note">
            <strong>STARTTLS obbligatorio</strong>
            <span>
              I server che non offrono TLS o non completano la negoziazione TLS non vengono
              considerati verificati. La verifica si ferma prima del comando DATA e non invia alcun
              messaggio.
            </span>
          </div>
          <div className="actions-row">
            <Button
              onClick={() => void saveSettings()}
              disabled={!settingsValid || saving || loading}
            >
              {saving ? "Salvataggio…" : "Salva identità"}
            </Button>
            {!settingsValid && (
              <span className="verification-inline-note">
                Inserisci un hostname e un mittente validi.
              </span>
            )}
            {settingsValid && !settingsUnchanged && (
              <span className="verification-inline-note">Salva le modifiche prima di avviare.</span>
            )}
          </div>
        </CardContent>
      </Card>

      <Card className="island-section verification-panel">
        <CardHeader className="section-heading">
          <div>
            <h2>Avvia verifica</h2>
            <p>
              La verifica graduale, consigliata, rivalida il DNS, prova un campione prioritario di
              1.000 indirizzi e, dopo il controllo di sicurezza, può proseguire automaticamente fino
              a 5.000.
            </p>
          </div>
        </CardHeader>
        <CardContent>
          {!overview.preflight_ok && (
            <div className="verification-warning" role="alert">
              <strong>Controlli preliminari non ancora completati</strong>
              <span>
                {overview.preflight_message ??
                  "Salva l'identità SMTP e attendi l'anteprima degli indirizzi eleggibili."}
              </span>
            </div>
          )}
          <div className="actions-row verification-start-actions">
            <Button
              onClick={() => void start("recommended")}
              disabled={!canStart || jobActive || jobPaused}
            >
              Avvia verifica graduale
            </Button>
            <Button
              variant="outline"
              onClick={() => setProbeEverythingOpen(true)}
              disabled={!canStart || jobActive || jobPaused}
            >
              Verifica completa
            </Button>
          </div>
          <p className="verification-inline-note">
            Ogni verifica raggruppa gli indirizzi per dominio e MX, rispetta limiti di frequenza
            globali, per dominio e per MX e, dopo il riavvio dell’app, si riprende solo su
            richiesta.
          </p>
        </CardContent>
      </Card>

      {currentStatus.state !== "idle" && (
        <Card className="island-section verification-panel" aria-live="polite">
          <CardHeader className="section-heading section-heading-row">
            <div>
              <h2>Avanzamento</h2>
              <p>{currentStatus.message ?? (currentStatus.phase || "Attività di verifica")}</p>
            </div>
            <Badge variant={currentStatus.state === "failed" ? "destructive" : "default"}>
              {stateLabel(currentStatus.state)}
            </Badge>
          </CardHeader>
          <CardContent>
            <div className="verification-progress-heading">
              <strong>
                {formatNumber(currentStatus.processed)} / {formatNumber(currentStatus.total)}
              </strong>
              <span>
                {Math.round(progress)}% · ETA {formatEta(currentStatus.eta_seconds)}
              </span>
            </div>
            <Progress value={progress} aria-label="Avanzamento verifica" />
            <div className="verification-outcome-grid">
              <span>
                Accettati <strong>{formatNumber(currentStatus.accepted)}</strong>
              </span>
              <span>
                Rifiutati <strong>{formatNumber(currentStatus.rejected)}</strong>
              </span>
              <span>
                Temporanei <strong>{formatNumber(currentStatus.temporary)}</strong>
              </span>
              <span>
                Catch-all <strong>{formatNumber(currentStatus.catch_all)}</strong>
              </span>
              <span>
                Bloccati dal server <strong>{formatNumber(currentStatus.policy_blocked)}</strong>
              </span>
              <span>
                Sconosciuti <strong>{formatNumber(currentStatus.unknown)}</strong>
              </span>
            </div>
            <div className="actions-row">
              {jobActive && currentStatus.state !== "pausing" && (
                <Button
                  variant="outline"
                  onClick={() => void runControl("pause")}
                  disabled={!!action}
                >
                  Pausa
                </Button>
              )}
              {jobPaused && (
                <Button onClick={() => void runControl("resume")} disabled={!!action}>
                  Riprendi
                </Button>
              )}
              {(jobActive || jobPaused) && (
                <Button
                  variant="destructive"
                  onClick={() => void runControl("cancel")}
                  disabled={!!action}
                >
                  Annulla
                </Button>
              )}
            </div>
            {currentStatus.error && <p className="verification-error">{currentStatus.error}</p>}
          </CardContent>
        </Card>
      )}

      <Card className="island-section verification-panel">
        <CardHeader className="section-heading">
          <div>
            <h2>Attività precedente</h2>
            <p>
              Le risposte SMTP sono evidenza datata e non cancellano mai le occorrenze originali.
            </p>
          </div>
        </CardHeader>
        <CardContent>
          <div className="verification-history-grid">
            <span>
              Ultimo completamento <strong>{overview.last_completed_at ?? "Mai"}</strong>
            </span>
            <span>
              DNS rivalidati in questa verifica{" "}
              <strong>{formatNumber(currentStatus.dns_revalidated)}</strong>
            </span>
            <span>
              STARTTLS <strong>Obbligatorio</strong>
            </span>
            {dnsReport && (
              <span role="status">
                Ultimo DNS: <strong>{formatNumber(dnsReport.domains_revalidated)}</strong> domini,{" "}
                <strong>{formatNumber(dnsReport.routing_ok)}</strong> instradabili,{" "}
                <strong>{formatNumber(dnsReport.temporary_failures)}</strong> temporanei
              </span>
            )}
          </div>
        </CardContent>
      </Card>

      <AlertDialog open={probeEverythingOpen} onOpenChange={setProbeEverythingOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Avviare la verifica completa?</AlertDialogTitle>
            <AlertDialogDescription>
              Dopo la rivalidazione DNS verranno messi in coda tutti gli indirizzi eleggibili:{" "}
              <strong>{formatNumber(overview.eligible_addresses)}</strong> indirizzi su{" "}
              <strong>{formatNumber(overview.eligible_domains)}</strong> domini. La verifica può
              durare giorni o mesi, procede con limiti di frequenza e non può dimostrare con
              certezza l’esistenza di una casella catch-all.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Annulla</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                setProbeEverythingOpen(false);
                void start("everything");
              }}
            >
              Conferma verifica completa
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </section>
  );
}

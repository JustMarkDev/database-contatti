import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "../components/ui/toast";
import {
  approveReview,
  cancelAiClassification,
  detectOpencode,
  detectTypesafe,
  getTypesafeApiKey,
  setTypesafeApiKey,
  getResumableAiRun,
  listAiFieldChanges,
  listAiRuns,
  listPendingReviews,
  pauseAiClassification,
  rejectReview,
  resumeAiClassification,
  startAiClassification,
} from "../api/ai";
import { platform } from "../platform";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardHeader } from "../components/ui/card";
import { Input } from "../components/ui/input";
import { Icon } from "../components/Icon";
import { Progress } from "../components/ui/progress";
import type {
  AiFieldChange,
  AiProgress,
  AiRun,
  AiStartRequest,
  OpenCodeStatus,
  Review,
  TypeSafeKeyView,
  TypeSafeStatus,
} from "../types";

const ACTIVITY_LIMIT = 200;

const CASCADE_REQUEST: AiStartRequest = {
  thinking: true,
  contextSize: 4096,
  timeoutSeconds: 600,
  force: false,
  mode: "full",
  provider: "cascade",
  model: "jev-latest",
  jevModel: "jev-latest",
  escalationProvider: "opencode",
  escalationModel: "opencode-go/muse-spark-1.3-contributor",
  escalationEffort: "xhigh",
  confidenceThreshold: 0.8,
  gapNoulThreshold: 0.55,
  subcategoryFloor: 0.7,
  cheapSkipSubAbove: 0.88,
};

const fieldLabels: Record<string, string> = {
  category: "categoria",
  subcategory: "sottocategoria",
  comune: "comune",
  provincia: "provincia",
  regione: "regione",
  classificazione: "classificazione",
};

const liveFieldNames = new Set(Object.keys(fieldLabels));

type ActivityEntry = {
  id: string;
  at: number;
  text: string;
  tone?: "ok" | "warn" | "error";
};

/**
 * Per-contact status lines. Field-change rows carry the real detail; these only
 * flood the feed and freeze the UI when Jev runs fast.
 */
function isCoarseProgress(text: string) {
  return / · (aggiornato|nessuna modifica|gap tassonomia proposto)\b/u.test(text);
}

/** Notable operational lines worth keeping in the live feed. */
function isNotableProgress(text: string) {
  if (!text.trim() || isCoarseProgress(text)) return false;
  if (/^(ai|deterministic) · \d+\/\d+$/u.test(text)) return false;
  return true;
}

type Props = {
  active: boolean;
  /** Enrichment and review decisions rewrite contacts: the contacts view must reload. */
  onContactsChanged: () => void;
  onReviewCount: (value: number) => void;
  sharedDatabaseTotal: number;
};

function formatClock(at: number) {
  return new Intl.DateTimeFormat("it-IT", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(at));
}

function formatRunStatus(status: string) {
  switch (status) {
    case "running":
      return "in corso";
    case "pausing":
      return "arresto…";
    case "paused":
      return "in pausa";
    case "completed":
      return "completata";
    case "cancelled":
      return "annullata";
    case "failed":
      return "fallita";
    default:
      return status;
  }
}

function sourceLabel(evidence: string[]): string | null {
  const marker = evidence.find((entry) => entry.startsWith("via:"));
  if (!marker) return null;
  switch (marker.slice(4)) {
    case "jev":
      return "Jev";
    case "muse-minimal":
      return "Muse Spark";
    case "muse-high":
      return "Muse Spark (retry)";
    default:
      return marker.slice(4);
  }
}

function changeToActivity(change: AiFieldChange): ActivityEntry | null {
  if (!liveFieldNames.has(change.field_name)) return null;
  const label = fieldLabels[change.field_name] ?? change.field_name;
  const from = (change.old_value ?? "").trim();
  const to = (change.new_value ?? "").trim();
  let detail: string;
  switch (change.change_kind) {
    case "aggiunto":
      detail = `${label} → ${to || "—"}`;
      break;
    case "modificato":
      detail = `${label}: ${from || "—"} → ${to || "—"}`;
      break;
    case "in_revisione":
      detail = `${label} proposta: ${from ? `${from} → ` : ""}${to || "—"}`;
      break;
    case "saltato":
      detail = "saltato (già classificato)";
      break;
  }
  const via = sourceLabel(change.evidence ?? []);
  return {
    id: `change:${change.id}`,
    at: change.created_at ? Date.parse(change.created_at) || Date.now() : Date.now(),
    text: via ? `${change.email} · ${detail} · ${via}` : `${change.email} · ${detail}`,
    tone:
      change.change_kind === "saltato"
        ? "warn"
        : change.status === "pending_review"
          ? "warn"
          : "ok",
  };
}

export function AiPage({ active, onContactsChanged, onReviewCount, sharedDatabaseTotal }: Props) {
  const [opencode, setOpencode] = useState<OpenCodeStatus | null>(null);
  const [typesafe, setTypesafe] = useState<TypeSafeStatus | null>(null);
  const [typesafeKey, setTypesafeKey] = useState<TypeSafeKeyView | null>(null);
  const [availabilityLoading, setAvailabilityLoading] = useState(true);
  const [typesafeKeyDraft, setTypesafeKeyDraft] = useState("");
  const [progress, setProgress] = useState<AiProgress | null>(null);
  const [run, setRun] = useState<AiRun | null>(null);
  const [activity, setActivity] = useState<ActivityEntry[]>([]);
  const [reviews, setReviews] = useState<Review[]>([]);
  const [reviewTotal, setReviewTotal] = useState(0);
  const [pendingAction, setPendingAction] = useState<"start" | "pause" | "resume" | "stop" | null>(
    null,
  );
  const [actionMessage, setActionMessage] = useState<string | null>(null);
  const mounted = useRef(true);
  const autoResumeStarted = useRef(false);
  const activityRef = useRef<HTMLDivElement | null>(null);
  const stickToLive = useRef(true);
  const progressSeq = useRef(0);
  const seenChangeIdsRef = useRef<Set<string>>(new Set());
  const pendingProgressRef = useRef<AiProgress | null>(null);
  const progressFrameRef = useRef(0);
  const pullTimerRef = useRef(0);
  const lifecycleTimerRef = useRef(0);
  const latestRunIdRef = useRef<string | null>(null);

  const pushActivity = useCallback((entries: ActivityEntry[]) => {
    if (!entries.length) return;
    setActivity((previous) => {
      const known = new Set(previous.map((entry) => entry.id));
      const next = entries.filter((entry) => !known.has(entry.id));
      if (!next.length) return previous;
      return [...next, ...previous].slice(0, ACTIVITY_LIMIT);
    });
  }, []);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (progressFrameRef.current) cancelAnimationFrame(progressFrameRef.current);
      if (pullTimerRef.current) window.clearTimeout(pullTimerRef.current);
      if (lifecycleTimerRef.current) window.clearTimeout(lifecycleTimerRef.current);
    };
  }, []);

  const refresh = useCallback(async () => {
    const pending = await listPendingReviews();
    if (!mounted.current) return;
    setReviews(pending.reviews);
    setReviewTotal(pending.total);
    onReviewCount(pending.total);
  }, [onReviewCount]);

  const refreshLifecycle = useCallback(async () => {
    const all = (await listAiRuns()) ?? [];
    const latest = all[0] ?? null;
    if (!mounted.current) return;
    setRun((previous) => {
      if (previous?.id && latest?.id && previous.id !== latest.id) {
        setActivity([]);
        seenChangeIdsRef.current = new Set();
      }
      return latest;
    });
    setPendingAction((action) => {
      if (!latest) return action;
      if (["start", "resume"].includes(action ?? "") && latest.status === "running") {
        setActionMessage(null);
        return null;
      }
      if (action === "pause" && ["pausing", "paused"].includes(latest.status)) return null;
      if (action === "stop" && latest.status === "cancelled") return null;
      return action;
    });
    return latest;
  }, [listAiRuns]);

  const pullLiveChanges = useCallback(
    async (runId: string) => {
      try {
        const value = await listAiFieldChanges(runId, 0, 120);
        const changes = value?.changes ?? [];
        const fresh = changes.filter((change) => !seenChangeIdsRef.current.has(change.id));
        if (!fresh.length) return;
        for (const change of fresh) seenChangeIdsRef.current.add(change.id);
        pushActivity(
          fresh.map(changeToActivity).filter((entry): entry is ActivityEntry => entry !== null),
        );
      } catch {
        // Live feed stays best-effort; progress messages still arrive.
      }
    },
    [listAiFieldChanges, pushActivity],
  );

  const schedulePullLiveChanges = useCallback(
    (runId: string) => {
      latestRunIdRef.current = runId;
      if (pullTimerRef.current) return;
      pullTimerRef.current = window.setTimeout(() => {
        pullTimerRef.current = 0;
        const id = latestRunIdRef.current;
        if (id) void pullLiveChanges(id);
      }, 750);
    },
    [pullLiveChanges],
  );

  const scheduleLifecycleRefresh = useCallback(() => {
    if (lifecycleTimerRef.current) return;
    lifecycleTimerRef.current = window.setTimeout(() => {
      lifecycleTimerRef.current = 0;
      void refreshLifecycle().then((latest) => {
        if (latest?.id) schedulePullLiveChanges(latest.id);
      });
    }, 1_500);
  }, [refreshLifecycle, schedulePullLiveChanges]);

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    setAvailabilityLoading(true);
    const check = () => {
      const pending = [
        detectTypesafe()
          .then((value) => {
            if (!cancelled) setTypesafe(value);
          })
          .catch(() => {}),
        detectOpencode()
          .then((value) => {
            if (!cancelled) setOpencode(value);
          })
          .catch(() => {}),
        getTypesafeApiKey()
          .then((value) => {
            if (!cancelled) setTypesafeKey(value);
          })
          .catch(() => {}),
      ];
      void Promise.all(pending).then(() => {
        if (!cancelled) setAvailabilityLoading(false);
      });
    };
    check();
    const interval = window.setInterval(check, 5_000);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [active, detectOpencode, detectTypesafe, getTypesafeApiKey]);

  const providerAvailable = (typesafe?.available ?? false) && (opencode?.available ?? false);

  const lastRunStatus = useRef<string | undefined>(undefined);
  useEffect(() => {
    const previous = lastRunStatus.current;
    lastRunStatus.current = run?.status;
    const wasActive = previous === "running" || previous === "pausing";
    const isActive = run?.status === "running" || run?.status === "pausing";
    if (wasActive && !isActive) onContactsChanged();
  }, [onContactsChanged, run?.status]);

  useEffect(() => {
    let cleanup: (() => void) | undefined;
    void platform
      .listen<AiProgress>("ai-progress", (event) => {
        const payload = event.payload;
        pendingProgressRef.current = payload;
        if (!progressFrameRef.current) {
          progressFrameRef.current = requestAnimationFrame(() => {
            progressFrameRef.current = 0;
            const latest = pendingProgressRef.current;
            if (latest) setProgress(latest);
          });
        }
        const text = payload.message || payload.error;
        if (text && isNotableProgress(text)) {
          progressSeq.current += 1;
          pushActivity([
            {
              id: `progress:${progressSeq.current}:${payload.processed}:${text}`,
              at: Date.now(),
              text,
              tone: payload.error ? "error" : undefined,
            },
          ]);
        }
        scheduleLifecycleRefresh();
      })
      .then((fn) => {
        cleanup = fn;
      });
    return () => cleanup?.();
  }, [pushActivity, scheduleLifecycleRefresh]);

  useEffect(() => {
    if (!active) return;
    void getResumableAiRun()
      .then((value) => {
        if (value) setRun(value);
        return Promise.all([refreshLifecycle(), refresh()]);
      })
      .catch((error) => toast.error(String(error)));
  }, [active, getResumableAiRun, refresh, refreshLifecycle]);

  useEffect(() => {
    if (!active || !providerAvailable || autoResumeStarted.current) return;
    autoResumeStarted.current = true;
    void getResumableAiRun()
      .then((value) => {
        if (!value || value.interruption_reason !== "Pausa automatica alla chiusura dell'app") {
          return;
        }
        setRun(value);
        setPendingAction("resume");
        setActionMessage("Ripresa automatica…");
        pushActivity([
          {
            id: `resume-auto:${value.id}`,
            at: Date.now(),
            text: "Ripresa automatica dell'esecuzione interrotta alla chiusura",
          },
        ]);
        return resumeAiClassification(value.id)
          .then(() => refresh())
          .catch((error) => toast.error(String(error)))
          .finally(() => {
            if (mounted.current) {
              setPendingAction(null);
              setActionMessage(null);
            }
          });
      })
      .catch(() => {});
  }, [active, getResumableAiRun, providerAvailable, pushActivity, refresh, resumeAiClassification]);

  useEffect(() => {
    if (!run?.id) return;
    void pullLiveChanges(run.id);
  }, [pullLiveChanges, run?.id]);

  useEffect(() => {
    if (!run?.id || !["running", "pausing"].includes(run.status)) return;
    const interval = window.setInterval(() => {
      void pullLiveChanges(run.id);
      void refreshLifecycle();
    }, 1_000);
    return () => window.clearInterval(interval);
  }, [pullLiveChanges, refreshLifecycle, run?.id, run?.status]);

  useEffect(() => {
    if (!stickToLive.current || !activityRef.current) return;
    activityRef.current.scrollTop = 0;
  }, [activity]);

  const runActive = Boolean(run && ["running", "pausing"].includes(run.status));
  const runPaused = run?.status === "paused";
  const total = progress?.total || run?.total || sharedDatabaseTotal || 0;
  const processed = progress?.processed ?? run?.processed ?? 0;
  const percent =
    total > 0 ? Math.min(100, Math.max(0, Math.floor((processed * 1000) / total) / 10)) : 0;
  const percentLabel =
    total > 0 && processed > 0 && percent < 0.1
      ? "<0.1"
      : percent.toFixed(percent % 1 === 0 ? 0 : 1);

  async function start() {
    setPendingAction("start");
    setActionMessage("Avvio arricchimento…");
    setProgress(null);
    setActivity([]);
    seenChangeIdsRef.current = new Set();
    pushActivity([
      {
        id: `start:${Date.now()}`,
        at: Date.now(),
        text: "Avvio arricchimento Cascade Jev → Muse Spark 1.3",
      },
    ]);
    try {
      window.setTimeout(() => void refreshLifecycle(), 50);
      await startAiClassification(CASCADE_REQUEST);
      await refresh();
      await refreshLifecycle();
    } catch (error) {
      toast.error(String(error));
      pushActivity([
        {
          id: `start-error:${Date.now()}`,
          at: Date.now(),
          text: String(error),
          tone: "error",
        },
      ]);
    } finally {
      setPendingAction(null);
      setActionMessage(null);
    }
  }

  async function pause() {
    if (!run) return;
    setPendingAction("pause");
    setActionMessage("Pausa…");
    try {
      await pauseAiClassification(run.id);
      await refreshLifecycle();
      pushActivity([
        { id: `pause:${Date.now()}`, at: Date.now(), text: "Pausa richiesta", tone: "warn" },
      ]);
    } catch (error) {
      toast.error(String(error));
    } finally {
      setPendingAction(null);
      setActionMessage(null);
    }
  }

  async function resume() {
    if (!run) return;
    setPendingAction("resume");
    setActionMessage("Ripresa…");
    try {
      await resumeAiClassification(run.id);
      await refresh();
      await refreshLifecycle();
      pushActivity([{ id: `resume:${Date.now()}`, at: Date.now(), text: "Esecuzione ripresa" }]);
    } catch (error) {
      toast.error(String(error));
    } finally {
      setPendingAction(null);
      setActionMessage(null);
    }
  }

  async function stop() {
    if (!run) return;
    setPendingAction("stop");
    setActionMessage("Arresto…");
    try {
      await cancelAiClassification(run.id);
      await refreshLifecycle();
      pushActivity([
        {
          id: `stop:${Date.now()}`,
          at: Date.now(),
          text: "Esecuzione annullata",
          tone: "warn",
        },
      ]);
    } catch (error) {
      toast.error(String(error));
    } finally {
      setPendingAction(null);
      setActionMessage(null);
    }
  }

  async function saveKey() {
    try {
      const saved = await setTypesafeApiKey(typesafeKeyDraft.trim());
      setTypesafeKey(saved);
      setTypesafeKeyDraft("");
      const status = await detectTypesafe();
      setTypesafe(status);
      toast.success("Chiave TypeSafe salvata.");
    } catch (error) {
      toast.error(String(error));
    }
  }

  const primaryDisabled =
    Boolean(pendingAction) ||
    (!runActive && !runPaused && !providerAvailable) ||
    run?.status === "pausing";

  const primaryLabel = (() => {
    if (pendingAction === "start") return "Avvio…";
    if (pendingAction === "resume") return "Ripresa…";
    if (pendingAction === "pause") return "Pausa…";
    if (run?.status === "pausing") return "Arresto in corso…";
    if (runActive) return "Ferma";
    if (runPaused) return "Riprendi";
    return "Arricchisci database";
  })();

  async function onPrimary() {
    if (runActive) {
      await pause();
      return;
    }
    if (runPaused) {
      await resume();
      return;
    }
    await start();
  }

  if (!active) return null;

  return (
    <section className="content-island" aria-label="Classificazione IA">
      <div className="island-section island-section-header">
        <header className="topbar">
          <div>
            <h1>Classificazione IA</h1>
            <p className="topbar-description">
              Completa categoria, sottocategoria e località con Jev, poi Muse Spark. Le proposte
              incerte aspettano la tua approvazione.
            </p>
          </div>
        </header>
      </div>

      <Card className="island-section ai-panel" aria-label="Arricchimento">
        <CardContent className="flex flex-col gap-4">
          {availabilityLoading ? (
            <p role="status">Verifica disponibilità…</p>
          ) : (
            (!typesafeKey?.configured || !typesafe?.available) && (
              <div className="inline-form">
                <Input
                  aria-label="Chiave API TypeSafe"
                  type="password"
                  value={typesafeKeyDraft}
                  onChange={(event) => setTypesafeKeyDraft(event.target.value)}
                  placeholder="Chiave API TypeSafe"
                />
                <Button onClick={() => void saveKey()} disabled={!typesafeKeyDraft.trim()}>
                  Salva chiave
                </Button>
              </div>
            )
          )}

          <div className="ai-control-row">
            <div className="actions-row">
              <Button onClick={() => void onPrimary()} disabled={primaryDisabled}>
                <Icon
                  name={runActive ? "pause" : runPaused ? "play" : "sparkle"}
                  size={16}
                  className="size-4"
                />
                {primaryLabel}
              </Button>
              {(runActive || runPaused) && (
                <Button
                  variant="outline"
                  onClick={() => void stop()}
                  disabled={Boolean(pendingAction)}
                >
                  {pendingAction === "stop" ? "Arresto…" : "Annulla"}
                </Button>
              )}
            </div>
            {!availabilityLoading && !providerAvailable && (
              <p className="ai-provider-status is-down" role="status">
                {!typesafe?.available
                  ? `Jev non pronto${typesafe?.error ? `: ${typesafe.error}` : ""}`
                  : `OpenCode non pronto${opencode?.error ? `: ${opencode.error}` : ""}`}
              </p>
            )}
            {!availabilityLoading && providerAvailable && opencode?.version && (
              <p className="ai-provider-status" role="status">
                Jev e OpenCode {opencode.version} pronti
              </p>
            )}
          </div>

          {(runActive || runPaused || processed > 0) && (
            <div className="progress-card" aria-label="Avanzamento">
              <div className="ai-progress-heading">
                <strong>
                  {percentLabel}% · {processed}/{total || "…"}
                </strong>
              </div>
              <Progress value={total > 0 ? Math.min(100, Math.round(percent)) : null} />
              {actionMessage && <p>{actionMessage}</p>}
              {run?.interruption_reason && <p>{run.interruption_reason}</p>}
            </div>
          )}
        </CardContent>
      </Card>

      <Card className="island-section ai-log-panel" aria-label="Attività in tempo reale">
        <CardHeader className="section-heading">
          <div>
            <h2>Attività in tempo reale</h2>
            {run ? (
              <dl className="ai-run-metrics" aria-label="Riepilogo esecuzione">
                <div className="ai-run-metric ai-run-metric-status">
                  <dt>Stato</dt>
                  <dd>{formatRunStatus(run.status)}</dd>
                </div>
                <div className="ai-run-metric">
                  <dt>Elaborati</dt>
                  <dd>
                    {run.processed.toLocaleString("it-IT")}/{run.total.toLocaleString("it-IT")}
                  </dd>
                </div>
                <div className="ai-run-metric">
                  <dt>Aggiornati</dt>
                  <dd>{run.updated.toLocaleString("it-IT")}</dd>
                </div>
                <div className="ai-run-metric">
                  <dt>Lacune</dt>
                  <dd>{run.reviews_created.toLocaleString("it-IT")}</dd>
                </div>
                <div className="ai-run-metric">
                  <dt>Saltati</dt>
                  <dd>{run.skipped.toLocaleString("it-IT")}</dd>
                </div>
                <div className="ai-run-metric">
                  <dt>Errori</dt>
                  <dd>{run.errors.toLocaleString("it-IT")}</dd>
                </div>
              </dl>
            ) : (
              <p>I messaggi di progresso e le modifiche appaiono qui in tempo reale.</p>
            )}
          </div>
        </CardHeader>
        <CardContent>
          <div
            className="ai-activity-log"
            ref={activityRef}
            role="log"
            aria-live="polite"
            aria-relevant="additions"
            onScroll={(event) => {
              stickToLive.current = event.currentTarget.scrollTop < 24;
            }}
          >
            {activity.length === 0 && <p className="ai-empty-state">In attesa di attività…</p>}
            {activity.map((entry) => (
              <div
                key={entry.id}
                className={`ai-activity-line${entry.tone ? ` ai-activity-${entry.tone}` : ""}`}
              >
                <time dateTime={new Date(entry.at).toISOString()}>{formatClock(entry.at)}</time>
                <span>{entry.text}</span>
              </div>
            ))}
          </div>
        </CardContent>
      </Card>

      {reviews.length > 0 && (
        <Card className="island-section ai-panel" aria-label="Lacune di tassonomia">
          <CardHeader className="section-heading">
            <div>
              <h2>Lacune di tassonomia</h2>
              <p>
                {reviewTotal.toLocaleString("it-IT")} proposte in attesa di approvazione
                {reviewTotal > reviews.length
                  ? `; mostrate le ${reviews.length} più vecchie.`
                  : "."}
              </p>
            </div>
          </CardHeader>
          <CardContent className="review-list">
            {reviews.map((review) => (
              <div className="review-card" key={review.id}>
                <strong>{review.email}</strong>
                <p>
                  {review.current_category}
                  {review.current_subcategory ? ` / ${review.current_subcategory}` : ""}{" "}
                  <span className="review-arrow">→</span> {review.proposed_category}
                  {review.proposed_subcategory ? ` / ${review.proposed_subcategory}` : ""}
                </p>
                <div className="actions-row">
                  <Button
                    size="sm"
                    onClick={() =>
                      void approveReview(review.id)
                        .then(() => {
                          onContactsChanged();
                          return refresh();
                        })
                        .catch((error) => toast.error(String(error)))
                    }
                  >
                    Approva
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() =>
                      void rejectReview(review.id)
                        .then(() => {
                          onContactsChanged();
                          return refresh();
                        })
                        .catch((error) => toast.error(String(error)))
                    }
                  >
                    Rifiuta
                  </Button>
                </div>
              </div>
            ))}
          </CardContent>
        </Card>
      )}
    </section>
  );
}

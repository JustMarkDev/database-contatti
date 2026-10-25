import { useCallback, useEffect, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  AlertDiamondIcon,
  DatabaseIcon,
  Download01Icon,
  Refresh01Icon,
  SparklesIcon,
  Upload01Icon,
} from "@hugeicons/core-free-icons";
import { detectOpencode, detectTypesafe, getTypesafeApiKey, setTypesafeApiKey } from "../api/ai";
import { exportDatabaseBackup, importDatabaseBackup } from "../api/backup";
import { clearBlacklist } from "../api/blacklist";
import { clearContacts, refreshDashboard } from "../api/contacts";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { confirmDialog } from "../components/ui/confirm-dialog";
import { Input } from "../components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../components/ui/select";
import {
  EFFORT_LEVELS,
  loadOpenCodePrefs,
  saveOpenCodePrefs,
  type OpenCodePrefs,
} from "../features/opencodePrefs";
import { toast } from "../components/ui/toast";
import { emptyFilters } from "../features/filters";
import { platform } from "../platform";
import type { OpenCodeStatus, TypeSafeKeyView, TypeSafeStatus } from "../types";

type Props = {
  active: boolean;
  busy: boolean;
  setBusy: (value: boolean) => void;
  databaseTotal: number;
  blacklistCount: number;
  onSummary: (contacts: number, blacklist: number, reviewCount: number) => void | Promise<void>;
  onDatabaseCleared?: () => void;
  onDatabaseRestored?: () => void | Promise<void>;
  onBlacklistCount: (value: number) => void | Promise<void>;
};

async function confirmDestructiveAction(kind: "database" | "blacklist") {
  const database = kind === "database";
  const firstConfirmed = await confirmDialog({
    title: database ? "Eliminare tutti i contatti?" : "Eliminare tutta la blacklist?",
    description: database
      ? "ATTENZIONE: questa azione cancella definitivamente tutti i contatti dal database. La blacklist rimarrà invariata e non è possibile annullare l'operazione."
      : "ATTENZIONE: questa azione cancella definitivamente tutti gli indirizzi dalla blacklist. I contatti già esclusi non verranno ripristinati e non è possibile annullare l'operazione.",
    actionLabel: "Continua",
    cancelLabel: "Annulla",
    variant: "destructive",
  });
  if (!firstConfirmed) return false;

  return confirmDialog({
    title: "Conferma definitiva",
    description: database
      ? "Sei davvero sicuro? Tutti i dati dei contatti saranno eliminati in modo permanente. Premi «Elimina definitivamente» solo se vuoi procedere."
      : "Sei davvero sicuro? Tutte le email della blacklist saranno eliminate in modo permanente. Premi «Elimina definitivamente» solo se vuoi procedere.",
    actionLabel: "Elimina definitivamente",
    cancelLabel: "Annulla",
    variant: "destructive",
  });
}

function ProviderRow({
  name,
  badges,
  error,
  children,
}: {
  name: string;
  badges: { label: string; ok: boolean }[];
  error?: string | null;
  children?: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-2 rounded-md border border-border-soft p-3">
      <div className="flex flex-wrap items-center gap-2">
        <strong className="text-sm">{name}</strong>
        {badges.map((badge) => (
          <Badge key={badge.label} variant={badge.ok ? "default" : "warning"}>
            {badge.label}
          </Badge>
        ))}
      </div>
      {error && <p className="m-0 text-sm text-muted-foreground">{error}</p>}
      {children}
    </div>
  );
}

function ProvidersCard({ active }: { active: boolean }) {
  const [opencode, setOpencode] = useState<OpenCodeStatus | null>(null);
  const [typesafe, setTypesafe] = useState<TypeSafeStatus | null>(null);
  const [key, setKey] = useState<TypeSafeKeyView | null>(null);
  const [draft, setDraft] = useState("");
  const [loading, setLoading] = useState(true);
  const [prefs, setPrefs] = useState<OpenCodePrefs>(loadOpenCodePrefs);

  function updatePrefs(next: OpenCodePrefs) {
    setPrefs(next);
    saveOpenCodePrefs(next);
  }

  const refresh = useCallback(async () => {
    setLoading(true);
    await Promise.all([
      detectOpencode().then(setOpencode, () => setOpencode(null)),
      detectTypesafe().then(setTypesafe, () => setTypesafe(null)),
      getTypesafeApiKey().then(setKey, () => setKey(null)),
    ]);
    setLoading(false);
  }, []);

  useEffect(() => {
    if (active) void refresh();
  }, [active, refresh]);

  async function saveKey(value: string) {
    try {
      setKey(await setTypesafeApiKey(value));
      setDraft("");
      setTypesafe(await detectTypesafe());
      toast.success(value ? "Chiave Jev salvata." : "Chiave Jev rimossa.");
    } catch (error) {
      toast.error(String(error));
    }
  }

  return (
    <Card className="island-section settings-panel">
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <HugeiconsIcon
            icon={SparklesIcon}
            size={20}
            strokeWidth={1.75}
            color="currentColor"
            className="size-5 text-brand"
          />
          <span>Provider IA</span>
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="ml-auto"
            onClick={() => void refresh()}
            disabled={loading}
          >
            <HugeiconsIcon
              icon={Refresh01Icon}
              size={16}
              strokeWidth={1.75}
              color="currentColor"
              className="size-4"
            />
            {loading ? "Verifica…" : "Ricontrolla"}
          </Button>
        </CardTitle>
        <CardDescription>
          Stato dei provider usati per la classificazione. Le chiavi restano sul tuo computer.
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <ProviderRow
          name="OpenCode"
          error={opencode?.error}
          badges={
            opencode
              ? [
                  {
                    label: opencode.installed ? "Installato" : "Non installato",
                    ok: opencode.installed,
                  },
                  ...(opencode.version ? [{ label: `v${opencode.version}`, ok: true }] : []),
                  {
                    label: opencode.authenticated ? "Autenticato" : "Non autenticato",
                    ok: opencode.authenticated,
                  },
                  { label: `${opencode.models.length} modelli`, ok: opencode.models.length > 0 },
                ]
              : []
          }
        >
          {opencode && opencode.models.length > 0 && (
            <div className="grid gap-3 sm:grid-cols-2">
              <label className="flex flex-col gap-1 text-sm">
                Modello
                <Select
                  value={prefs.model ?? opencode.selected_model}
                  onValueChange={(model) => updatePrefs({ ...prefs, model: model as string })}
                >
                  <SelectTrigger aria-label="Modello OpenCode">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {opencode.models.map((model) => (
                      <SelectItem key={model.id} value={model.id}>
                        {model.id}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </label>
              <label className="flex flex-col gap-1 text-sm">
                Effort
                <Select
                  value={prefs.effort ?? "default"}
                  onValueChange={(effort) =>
                    updatePrefs({
                      ...prefs,
                      effort: effort === "default" ? null : (effort as string),
                    })
                  }
                >
                  <SelectTrigger aria-label="Effort OpenCode">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {["default", ...EFFORT_LEVELS].map((level) => (
                      <SelectItem key={level} value={level}>
                        {level === "default" ? "Predefinito (xhigh)" : level}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </label>
            </div>
          )}
        </ProviderRow>
        <ProviderRow
          name="Jev (TypeSafe)"
          error={typesafe?.error}
          badges={
            typesafe
              ? [
                  {
                    label: key?.configured ? "Chiave configurata" : "Chiave mancante",
                    ok: Boolean(key?.configured),
                  },
                  ...(key?.from_env ? [{ label: "Da TYPESAFE_API_KEY", ok: true }] : []),
                  { label: typesafe.available ? "Pronto" : "Non pronto", ok: typesafe.available },
                ]
              : []
          }
        >
          <form
            className="inline-form"
            onSubmit={(event) => {
              event.preventDefault();
              void saveKey(draft.trim());
            }}
          >
            <Input
              aria-label="Chiave API TypeSafe"
              type="password"
              autoComplete="off"
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              placeholder={
                key?.configured ? "Sostituisci la chiave API TypeSafe" : "Chiave API TypeSafe"
              }
            />
            <Button type="submit" disabled={!draft.trim()}>
              Salva chiave
            </Button>
            {key?.configured && !key.from_env && (
              <Button type="button" variant="outline" onClick={() => void saveKey("")}>
                Rimuovi
              </Button>
            )}
          </form>
        </ProviderRow>
      </CardContent>
    </Card>
  );
}

export function SettingsPage({
  active,
  busy,
  setBusy,
  databaseTotal,
  blacklistCount,
  onSummary,
  onDatabaseCleared,
  onDatabaseRestored,
  onBlacklistCount,
}: Props) {
  async function handleExportBackup() {
    try {
      const today = new Date().toISOString().slice(0, 10);
      const path = await platform.saveFile({
        defaultPath: `database_contatti_backup_${today}.zip`,
        filters: [
          { name: "Archivio di backup ZIP (.zip)", extensions: ["zip"] },
          { name: "Database SQLite (.sqlite3, .db)", extensions: ["sqlite3", "db"] },
        ],
      });
      if (!path) return;

      setBusy(true);
      const toastId = toast.loading("Creazione e compressione del backup in corso…");
      try {
        const report = await exportDatabaseBackup(path);
        const sizeMb = (report.size_bytes / (1024 * 1024)).toFixed(1);
        const sizeDisplay =
          report.size_bytes >= 1024 * 1024 * 1024
            ? `${(report.size_bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`
            : `${sizeMb} MB`;
        toast.success(`Backup esportato con successo (${sizeDisplay})`, {
          id: toastId,
          description: `File salvato in ${report.path}. Contiene ${report.contact_count.toLocaleString("it-IT")} contatti e ${report.blacklist_count.toLocaleString("it-IT")} voci blacklist.`,
        });
      } catch (error) {
        toast.error("Errore durante l'esportazione del backup", {
          id: toastId,
          description: String(error),
        });
      } finally {
        setBusy(false);
      }
    } catch (error) {
      toast.error("Impossibile avviare il salvataggio del backup", {
        description: String(error),
      });
    }
  }

  async function handleImportBackup() {
    try {
      const selected = await platform.openFiles({
        multiple: false,
        filters: [{ name: "Backup del database", extensions: ["zip", "sqlite3", "db"] }],
      });
      if (!selected) return;
      const filePath = Array.isArray(selected) ? selected[0] : selected;
      if (!filePath) return;

      const confirmed = await confirmDialog({
        title: "Ripristinare il database dal backup?",
        description:
          "ATTENZIONE: Il ripristino sostituirà tutti i dati attuali (contatti, blacklist, candidati, storico importazioni) con quelli contenuti nel file selezionato. L'operazione non è annullabile.",
        actionLabel: "Ripristina database",
        cancelLabel: "Annulla",
        variant: "destructive",
      });
      if (!confirmed) return;

      setBusy(true);
      const toastId = toast.loading("Verifica e ripristino del database in corso…");
      try {
        const report = await importDatabaseBackup(filePath);
        if (onDatabaseRestored) {
          await onDatabaseRestored();
        } else {
          const dashboard = await refreshDashboard(emptyFilters());
          await onSummary(
            dashboard.database_total,
            dashboard.blacklist_count,
            dashboard.pending_review_count,
          );
          await onBlacklistCount(dashboard.blacklist_count);
        }
        toast.success("Database ripristinato con successo!", {
          id: toastId,
          description: `Ripristinati ${report.contact_count.toLocaleString("it-IT")} contatti e ${report.blacklist_count.toLocaleString("it-IT")} indirizzi in blacklist.`,
        });
      } catch (error) {
        toast.error("Errore durante il ripristino del database", {
          id: toastId,
          description: String(error),
        });
      } finally {
        setBusy(false);
      }
    } catch (error) {
      toast.error("Impossibile selezionare il file di backup", {
        description: String(error),
      });
    }
  }

  async function clearDatabase() {
    if (!(await confirmDestructiveAction("database"))) return;
    setBusy(true);
    const toastId = toast.loading("Eliminazione definitiva dei contatti in corso…");
    try {
      await clearContacts();
      await onSummary(0, blacklistCount, 0);
      onDatabaseCleared?.();
      toast.success("Tutti i contatti sono stati eliminati", { id: toastId });
    } catch (error) {
      toast.error(String(error), { id: toastId });
    } finally {
      setBusy(false);
    }
  }

  async function clearBlacklistEntries() {
    if (!(await confirmDestructiveAction("blacklist"))) return;
    setBusy(true);
    const toastId = toast.loading("Eliminazione definitiva della blacklist in corso…");
    try {
      await clearBlacklist();
      await onBlacklistCount(0);
      toast.success("Tutta la blacklist è stata eliminata", { id: toastId });
    } catch (error) {
      toast.error(String(error), { id: toastId });
    } finally {
      setBusy(false);
    }
  }

  if (!active) return null;

  return (
    <section className="content-island" aria-label="Impostazioni">
      <div className="island-section island-section-header">
        <header className="topbar">
          <div>
            <h1>Impostazioni</h1>
            <p className="topbar-description">
              Provider IA, backup, ripristino e manutenzione dell'archivio locale.
            </p>
          </div>
        </header>
      </div>

      <Card className="island-section settings-panel">
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <HugeiconsIcon
              icon={DatabaseIcon}
              size={20}
              strokeWidth={1.75}
              color="currentColor"
              className="size-5 text-brand"
            />
            <span>Backup e migrazione del database</span>
          </CardTitle>
          <CardDescription>
            Esporta l'intero archivio (contatti, blacklist, candidati, storico importazioni e
            classificazioni) in un singolo file compresso per trasferirlo su un altro computer o
            ripristina un backup esistente.
          </CardDescription>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          <div className="flex flex-wrap items-center gap-3">
            <Button
              type="button"
              variant="default"
              onClick={() => void handleExportBackup()}
              disabled={busy}
            >
              <HugeiconsIcon
                icon={Download01Icon}
                size={16}
                strokeWidth={1.75}
                color="currentColor"
                className="size-4"
              />
              Esporta backup database (.zip)
            </Button>
            <Button
              type="button"
              variant="outline"
              onClick={() => void handleImportBackup()}
              disabled={busy}
            >
              <HugeiconsIcon
                icon={Upload01Icon}
                size={16}
                strokeWidth={1.75}
                color="currentColor"
                className="size-4"
              />
              Importa / Ripristina database
            </Button>
          </div>
        </CardContent>
      </Card>

      <ProvidersCard active={active} />

      <Card className="island-section settings-panel">
        <details>
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-md px-4 text-sm font-semibold text-destructive focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/60">
            <HugeiconsIcon
              icon={AlertDiamondIcon}
              size={20}
              strokeWidth={1.75}
              color="currentColor"
              aria-hidden="true"
              className="size-5 text-destructive"
            />
            <span>Azioni distruttive</span>
          </summary>
          <CardContent className="mt-4 flex flex-col gap-3 border-t border-border-soft pt-4">
            <p className="m-0 text-sm text-muted-foreground">
              Queste operazioni cancellano dati in modo permanente e richiedono due conferme.
            </p>
            <div className="danger-warning" role="note">
              <HugeiconsIcon
                icon={AlertDiamondIcon}
                size={20}
                strokeWidth={1.75}
                color="currentColor"
                aria-hidden="true"
                className="size-5"
              />
              <strong>Non è possibile annullare queste operazioni o recuperare i dati.</strong>
            </div>
            <div className="flex flex-wrap gap-2">
              <Button
                type="button"
                variant="destructive"
                onClick={() => void clearDatabase()}
                disabled={busy || databaseTotal === 0}
              >
                <HugeiconsIcon
                  icon={AlertDiamondIcon}
                  size={16}
                  strokeWidth={1.75}
                  color="currentColor"
                  aria-hidden="true"
                  className="size-4"
                />
                Ripulisci database
              </Button>
              <Button
                type="button"
                variant="destructive"
                onClick={() => void clearBlacklistEntries()}
                disabled={busy || blacklistCount === 0}
              >
                <HugeiconsIcon
                  icon={AlertDiamondIcon}
                  size={16}
                  strokeWidth={1.75}
                  color="currentColor"
                  aria-hidden="true"
                  className="size-4"
                />
                Ripulisci blacklist
              </Button>
            </div>
          </CardContent>
        </details>
      </Card>
    </section>
  );
}

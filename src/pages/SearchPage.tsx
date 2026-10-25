import { memo, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  cancelLeadCrawl,
  discoverBySearch,
  exploreDirectory,
  exploreSeeds,
  exportSearchLeadsTxt,
  findSeedSites,
  getLeadFilterOptions,
  importLeadSource,
  listLeadSources,
  listSearchLeads,
  planLeadRequest,
  listSearchRuns,
  listSeedLists,
  promoteSearchLeads,
} from "../api/search";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";
import { Badge } from "../components/ui/badge";
import { Card } from "../components/ui/card";
import { Progress } from "../components/ui/progress";
import { Icon } from "../components/Icon";
import { MultiFilter } from "../components/MultiFilter";
import { DetailsSheet, type DetailSection } from "../components/ContactDetailsSheet";
import { confirmDialog } from "../components/ui/confirm-dialog";
import { toast } from "../components/ui/toast";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "../components/ui/table";
import { platform } from "../platform";
import type {
  CrawlProgress,
  FieldFilter,
  LeadFilterOptions,
  LeadFilters,
  LeadPlan,
  LeadSource,
  SearchLeadView,
  SearchRunView,
  SeedListView,
} from "../types";

/**
 * Cerca lead: a sandbox that mirrors the contacts database.
 *
 * Open-data imports, directories, web search and site crawls all land here
 * first; **Aggiungi al database** promotes the filtered leads into contacts.
 * Sites without a published address become "siti da esplorare" (seeds).
 */

const PAGE_SIZE = 100;
/** Pages read per site, the home included: each one is a context.dev credit. */
const PAGES_PER_SITE = 3;
/** PDFs per school site (role circolari); 0 keeps every run at ≤4 credits a site. */
const MAX_PDFS = 0;
const SITES_PER_RUN = 50;
/** Organizations known only by name searched per click (one credit each). */
const SITES_TO_FIND = 50;
/** Where web-search discoveries land (`lead_candidates::CHANNEL_SEARCH`). */
const SEARCH_LIST = "Scoperta: ricerca web";
/** Every crawl reads pages with the IA: it is what finds roles, offices and places. */
const USE_AI = true;

/** Working columns of the lead table; everything else is in the detail sheet. */
export const LEAD_COLUMNS = [
  "email",
  "ente",
  "categoria",
  "sottocategoria",
  "comune",
  "provincia",
  "regione",
  "pec",
  "fonti",
  "stato",
] as const;

const emptyField = (): FieldFilter => ({ include: [], exclude: [] });
export const emptyLeadFilters = (): LeadFilters => ({
  query: "",
  kind: emptyField(),
  run: emptyField(),
  category: emptyField(),
  subcategory: emptyField(),
  regione: emptyField(),
  provincia: emptyField(),
  method: emptyField(),
  pec: emptyField(),
  stato: emptyField(),
});
/** The two halves of the page: open data parsed directly, or web search and crawl. */
export type LeadMode = "open" | "web";
const MODE_KINDS: Record<LeadMode, string[]> = {
  open: ["source"],
  web: ["web", "directory", "crawl"],
};
/** Empty filters scoped to the runs of one tab. */
export const tabFilters = (mode: LeadMode): LeadFilters => ({
  ...emptyLeadFilters(),
  kind: { include: MODE_KINDS[mode], exclude: [] },
});
const emptyOptions = (): LeadFilterOptions => ({
  run: [],
  category: [],
  subcategory: [],
  regione: [],
  provincia: [],
  method: [],
  pec: [],
  stato: [],
});

/** `open_data`, `contextdev` (older runs: `sito`, `tinyfish`, `archivio`) (+`ia`) in words. */
export function formatMethod(method: string | null | undefined): string {
  if (!method) return "—";
  const [base, ...extra] = method.split("+");
  const label =
    {
      open_data: "Dati aperti",
      contextdev: "Sito web (context.dev)",
      sito: "Sito web",
      tinyfish: "Sito (TinyFish)",
      archivio: "Archivio web",
      commoncrawl: "Common Crawl",
      elenco: "Elenco",
    }[base] ?? base;
  return extra.includes("ia") ? `${label} + IA` : label;
}

const STATO_LABELS: Record<string, string> = {
  nuovo: "Nuovo",
  completa: "Completa il contatto",
  nel_database: "Già nel database",
};
const formatStato = (value: string) => STATO_LABELS[value] ?? value;
const yesNo = (value: boolean) => (value ? "Sì" : "No");

function leadSections(lead: SearchLeadView): DetailSection[] {
  return [
    [
      "Email",
      [
        ["Chiave confronto", lead.email_comparison_key],
        ["PEC", yesNo(lead.is_pec)],
        ["Stato", formatStato(lead.stato)],
      ],
    ],
    [
      "Ente e persona",
      [
        ["Organizzazione", lead.organization],
        ["Ufficio o gruppo", lead.department],
        ["Persona", lead.person_name],
        ["Ruolo", lead.role],
        ["Codice ufficiale", lead.official_code],
        ["Sito web", lead.website],
      ],
    ],
    [
      "Classificazione",
      [
        ["Categoria", lead.category],
        ["Sottocategoria", lead.subcategory],
      ],
    ],
    [
      "Località",
      [
        ["Indirizzo", lead.address],
        ["CAP", lead.postal_code],
        ["Comune", lead.comune],
        ["Provincia", lead.provincia],
        ["Regione", lead.regione],
        ["Macroarea", lead.macroarea],
        ["Paese", lead.paese],
      ],
    ],
    [
      "Fonte",
      [
        ["Ricerca", lead.run_label],
        ["Metodo", formatMethod(lead.method)],
        ["Pagina", lead.source_url],
        ["Trovato il", lead.retrieved_at],
        ["Ricerche che lo contengono", lead.source_count],
      ],
    ],
  ];
}

/** Datasets first, then directories. */
function sortLeadSources(catalog: LeadSource[]): LeadSource[] {
  const rank = { dataset: 0, directory: 1 } as const;
  return [...catalog].sort((left, right) => rank[left.kind] - rank[right.kind]);
}

function Panel({
  title,
  description,
  children,
  open = false,
}: {
  title: string;
  description: string;
  children: ReactNode;
  open?: boolean;
}) {
  return (
    <details className="search-panel" open={open}>
      <summary>
        <strong>{title}</strong>
        <span>{description}</span>
      </summary>
      <div className="search-panel-body">{children}</div>
    </details>
  );
}

function SourceCard({
  title,
  meta,
  note,
  children,
}: {
  title: string;
  meta?: string;
  note?: string;
  children: ReactNode;
}) {
  return (
    <li className="search-source">
      <div className="search-source-text">
        <strong>{title}</strong>
        {meta && <span className="search-source-meta">{meta}</span>}
        {note && <p className="search-source-note">{note}</p>}
      </div>
      <div className="search-source-actions">{children}</div>
    </li>
  );
}

type RunTotals = { lead_count: number; page_count: number; errors?: string[] };
const addTotals = (total: RunTotals, part: RunTotals | null): RunTotals => ({
  lead_count: total.lead_count + (part?.lead_count ?? 0),
  page_count: total.page_count + (part?.page_count ?? 0),
  errors: [...(total.errors ?? []), ...(part?.errors ?? [])],
});
/** Tell the user which tables of an import did not load; the rest was imported. */
const warnAboutErrors = (errors: string[] | undefined) => {
  if (errors?.length) toast.warning(`Tabelle non caricate: ${errors.join(" · ")}`);
};

export function SearchPage({
  active,
  onContactsChanged,
}: {
  active: boolean;
  onContactsChanged: () => void;
}) {
  const [mode, setMode] = useState<LeadMode>("open");
  const [filters, setFilters] = useState<LeadFilters>(() => tabFilters("open"));
  const [searchText, setSearchText] = useState("");
  const [options, setOptions] = useState<LeadFilterOptions>(emptyOptions);
  const [runs, setRuns] = useState<SearchRunView[]>([]);
  const [leads, setLeads] = useState<SearchLeadView[]>([]);
  const [total, setTotal] = useState(0);
  const [offset, setOffset] = useState(0);
  const [loading, setLoading] = useState(false);
  const [hasLoaded, setHasLoaded] = useState(false);
  const [selected, setSelected] = useState<SearchLeadView | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);
  const [busy, setBusy] = useState<string | null>(null);
  const [progress, setProgress] = useState<CrawlProgress | null>(null);
  const [promoting, setPromoting] = useState(false);

  const [sources, setSources] = useState<LeadSource[] | null>(null);
  const [seedLists, setSeedLists] = useState<SeedListView[]>([]);

  const [query, setQuery] = useState("");
  const [picked, setPicked] = useState<Set<string>>(() => new Set());

  const generation = useRef(0);
  const mounted = useRef(true);
  const searchTimer = useRef<number | null>(null);
  const lastProgressRefresh = useRef(0);
  const stopped = useRef(false);

  const runLabels = useMemo(() => new Map(runs.map((run) => [run.id, run.label])), [runs]);
  const refresh = useCallback(() => setRefreshKey((value) => value + 1), []);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      generation.current += 1;
      if (searchTimer.current) window.clearTimeout(searchTimer.current);
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void platform
      .listen<CrawlProgress>("lead-crawl-progress", (event) => {
        if (!mounted.current) return;
        setProgress(event.payload.finished ? null : event.payload);
        const now = Date.now();
        if (event.payload.finished || now - lastProgressRefresh.current >= 2_000) {
          lastProgressRefresh.current = now;
          refresh();
        }
      })
      .then((cleanup) => {
        unlisten = cleanup;
      });
    return () => unlisten?.();
  }, [refresh]);

  useEffect(() => {
    if (!active) return;
    const report = (error: unknown) => {
      if (mounted.current) toast.error(String(error));
    };
    if (!sources) {
      void listLeadSources()
        .then((catalog) => mounted.current && setSources(sortLeadSources(catalog)))
        .catch(report);
    }
  }, [active, sources]);

  useEffect(() => {
    if (!active) return;
    const report = (error: unknown) => {
      if (mounted.current) toast.error(String(error));
    };
    void getLeadFilterOptions(MODE_KINDS[mode])
      .then((value) => mounted.current && setOptions(value))
      .catch(report);
    void listSearchRuns()
      .then((value) => mounted.current && setRuns(value))
      .catch(report);
    void listSeedLists()
      .then((value) => mounted.current && setSeedLists(value))
      .catch(report);
  }, [active, mode, refreshKey]);

  useEffect(() => {
    setOffset(0);
  }, [filters]);

  useEffect(() => {
    if (!active) return;
    const current = ++generation.current;
    setLoading(true);
    void listSearchLeads(filters, offset)
      .then((page) => {
        if (!mounted.current || current !== generation.current) return;
        setLeads(page.leads);
        setTotal(page.total);
        setHasLoaded(true);
      })
      .catch((error) => {
        if (mounted.current && current === generation.current) toast.error(String(error));
      })
      .finally(() => {
        if (mounted.current && current === generation.current) setLoading(false);
      });
  }, [active, filters, offset, refreshKey]);

  /** Run one long action at a time; the sandbox reloads when it ends. */
  async function perform(label: string, action: () => Promise<string | null>) {
    setBusy(label);
    try {
      const message = await action();
      if (mounted.current && message) toast.success(message);
    } catch (error) {
      if (mounted.current) toast.error(String(error));
    } finally {
      if (mounted.current) {
        setBusy(null);
        setProgress(null);
        refresh();
      }
    }
  }

  const runMessage = (summary: { lead_count: number; page_count: number }) =>
    `${summary.lead_count.toLocaleString("it-IT")} lead` +
    (summary.page_count ? `, ${summary.page_count.toLocaleString("it-IT")} pagine lette` : "");

  function showOnlyRun(runId: string) {
    setSearchText("");
    setFilters((previous) => ({
      ...tabFilters("open"),
      kind: previous.kind,
      run: { include: [runId], exclude: [] },
    }));
  }

  /** Free-form request: the IA picks sources and filters, then everything runs on its own. */
  async function startRequest() {
    const trimmed = query.trim();
    if (!trimmed) {
      toast.error("Descrivi cosa cerchi.");
      return;
    }
    stopped.current = false;
    await perform("Ricerca guidata dall'IA", async () => {
      const plan = await planLeadRequest(trimmed);
      let totals: RunTotals = { lead_count: 0, page_count: 0 };
      const problems: string[] = [];
      const attempt = async (name: string, step: () => Promise<RunTotals | null>) => {
        if (stopped.current) return;
        try {
          totals = addTotals(totals, await step());
        } catch (error) {
          problems.push(`${name}: ${String(error)}`);
        }
      };
      for (const id of plan.source_ids) {
        const source = sources?.find((candidate) => candidate.id === id);
        if (!source) {
          problems.push(`Fonte sconosciuta: ${id}`);
          continue;
        }
        await attempt(source.name, () =>
          source.kind === "directory"
            ? exploreDirectory({ sourceId: id, useAi: USE_AI })
            : importLeadSource(id),
        );
      }
      await attempt("Siti già raccolti", () => readList(null, plan));
      let newSites = 0;
      await attempt("Ricerca sul web", async () => {
        const report = await discoverBySearch({
          query: plan.web_query,
          category: plan.category,
          subcategory: plan.subcategory,
          regione: plan.regione,
        });
        newSites = report.accepted;
        return report.accepted > 0 ? readList(SEARCH_LIST, plan) : null;
      });
      if (mounted.current) {
        setSearchText("");
        setFilters({
          ...tabFilters("web"),
          category: { include: plan.category ? [plan.category] : [], exclude: [] },
          subcategory: { include: plan.subcategory ? [plan.subcategory] : [], exclude: [] },
          regione: { include: plan.regione ? [plan.regione] : [], exclude: [] },
        });
      }
      if (problems.length > 0) toast.error(problems.join(" · "));
      warnAboutErrors(totals.errors);
      if (totals.lead_count === 0 && totals.page_count === 0 && problems.length === 0) {
        throw new Error("Nessun risultato per questa richiesta.");
      }
      const found = newSites ? ` · ${newSites.toLocaleString("it-IT")} siti nuovi` : "";
      return `Ricerca conclusa: ${runMessage(totals)}${found}`;
    });
  }

  /** Web search for the site of organizations known only by name, checked before reading. */
  function findSites(list: SeedListView) {
    void perform(`Ricerca dei siti di ${list.list_name}`, async () => {
      const found = await findSeedSites(list.list_name, SITES_TO_FIND);
      return `${list.list_name}: ${found.toLocaleString("it-IT")} siti trovati da leggere`;
    });
  }

  function importSource(source: LeadSource) {
    void perform(`Importazione ${source.name}`, async () => {
      const summary = await importLeadSource(source.id);
      warnAboutErrors(summary.errors);
      showOnlyRun(summary.run_id);
      return `${source.name}: ${runMessage(summary)}`;
    });
  }

  /** Reads pending sites of one list or matching a plan; nothing pending is not an error here. */
  async function readList(listName: string | null, plan?: LeadPlan) {
    try {
      return await exploreSeeds({
        listName,
        category: plan?.category,
        subcategory: plan?.subcategory,
        regione: plan?.regione,
        limit: SITES_PER_RUN,
        maxPages: PAGES_PER_SITE,
        maxPdfs: MAX_PDFS,
        useAi: USE_AI,
      });
    } catch (error) {
      if (String(error).includes("Nessun sito")) return null;
      throw error;
    }
  }

  /** One checkbox of "Usa i siti già pubblici": what it reads when started. */
  const places = useMemo(() => {
    const catalog = sources ?? [];
    const chained = new Set<string>();
    const items: {
      key: string;
      title: string;
      meta: string;
      run: () => Promise<RunTotals | null>;
    }[] = [];
    for (const source of catalog) {
      if (source.kind === "directory") {
        const list = `Elenco: ${source.name}`;
        chained.add(list);
        items.push({
          key: source.id,
          title: source.name,
          meta: source.note,
          run: async () => {
            const own = await exploreDirectory({ sourceId: source.id, useAi: USE_AI });
            return addTotals(own, await readList(list));
          },
        });
      } else if (source.kind === "dataset" && source.emails === "no") {
        const list = `Fonte: ${source.name}`;
        chained.add(list);
        items.push({
          key: source.id,
          title: source.name,
          meta: source.note,
          run: async () => {
            const own = await importLeadSource(source.id);
            return addTotals(own, await readList(list));
          },
        });
      }
    }
    for (const list of seedLists) {
      if (list.pending === 0 || chained.has(list.list_name)) continue;
      items.push({
        key: `list:${list.list_name}`,
        title: list.list_name,
        meta:
          `${list.pending.toLocaleString("it-IT")} siti ancora da leggere` +
          (list.to_verify ? ` · ${list.to_verify.toLocaleString("it-IT")} da verificare` : ""),
        run: () => readList(list.list_name),
      });
    }
    return items;
  }, [sources, seedLists]);

  /** Every open-data registry that publishes emails, one after the other. */
  function importAll() {
    const registries = (sources ?? []).filter(
      (source) => source.kind === "dataset" && source.emails !== "no",
    );
    stopped.current = false;
    void perform("Importazione di tutti i dati aperti", async () => {
      let totals: RunTotals = { lead_count: 0, page_count: 0 };
      const failed: string[] = [];
      for (const source of registries) {
        if (stopped.current) break;
        try {
          totals = addTotals(totals, await importLeadSource(source.id));
        } catch {
          failed.push(source.name);
        }
      }
      if (failed.length > 0) toast.error(`Non importati: ${failed.join(", ")}`);
      warnAboutErrors(totals.errors);
      return `Dati aperti importati: ${runMessage(totals)}`;
    });
  }

  function togglePlace(key: string) {
    setPicked((previous) => {
      const next = new Set(previous);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  }

  function readPlaces() {
    const chosen = places.filter((place) => picked.has(place.key));
    stopped.current = false;
    void perform("Lettura dei siti scelti", async () => {
      let totals: RunTotals = { lead_count: 0, page_count: 0 };
      const failed: string[] = [];
      for (const place of chosen) {
        if (stopped.current) break;
        try {
          totals = addTotals(totals, await place.run());
        } catch (error) {
          failed.push(`${place.title}: ${String(error)}`);
        }
      }
      setPicked(new Set());
      if (failed.length > 0) toast.error(failed.join(" · "));
      warnAboutErrors(totals.errors);
      return `Lettura conclusa: ${runMessage(totals)}`;
    });
  }

  async function addToDatabase() {
    const confirmed = await confirmDialog({
      title: "Aggiungere i lead al database?",
      description: `${total.toLocaleString("it-IT")} indirizzi filtrati entrano in Contatti (PEC comprese, segnate come PEC). Gli indirizzi in blacklist sono saltati. I lead trasferiti escono da questa lista.`,
      actionLabel: "Aggiungi al database",
    });
    if (!confirmed) return;
    setPromoting(true);
    try {
      const report = await promoteSearchLeads(filters);
      if (!mounted.current) return;
      const parts = [
        `${report.added.toLocaleString("it-IT")} nuovi`,
        `${report.updated.toLocaleString("it-IT")} aggiornati`,
      ];
      if (report.skipped_blacklist > 0) {
        parts.push(`${report.skipped_blacklist.toLocaleString("it-IT")} in blacklist`);
      }
      if (report.unverified > 0) {
        parts.push(
          `${report.unverified.toLocaleString("it-IT")} in attesa di verifica del dominio`,
        );
      }
      toast.success(`Lead aggiunti al database: ${parts.join(", ")}`);
      onContactsChanged();
      refresh();
    } catch (caught) {
      toast.error(String(caught));
    } finally {
      if (mounted.current) setPromoting(false);
    }
  }

  async function exportEmails() {
    try {
      const path = await platform.saveFile({
        defaultPath: `lead-${new Date().toISOString().slice(0, 10)}.txt`,
        filters: [{ name: "Testo", extensions: ["txt"] }],
      });
      if (!path) return;
      const report = await exportSearchLeadsTxt(filters, path);
      toast.success(`${report.rows_written.toLocaleString("it-IT")} indirizzi esportati`);
    } catch (caught) {
      toast.error(String(caught));
    }
  }

  function onSearchChange(value: string) {
    setSearchText(value);
    if (searchTimer.current) window.clearTimeout(searchTimer.current);
    searchTimer.current = window.setTimeout(() => {
      setFilters((previous) => ({ ...previous, query: value }));
    }, 300);
  }

  if (!active) return null;

  const working = busy !== null;

  return (
    <section className="content-island" aria-label="Cerca lead">
      <div className="island-section island-section-header">
        <header className="topbar">
          <div>
            <h1>Cerca lead</h1>
            <p className="topbar-description">
              Importa dati aperti o cerca sul web. I lead restano qui, con gli stessi campi dei
              contatti, finché non li aggiungi al database.
            </p>
          </div>
          <div className="header-metrics">
            <div aria-live="polite">
              <span>Lead filtrati</span>
              <strong>{total.toLocaleString("it-IT")}</strong>
            </div>
            <Button
              onClick={() => void addToDatabase()}
              disabled={working || promoting || total === 0}
            >
              <Icon name="plus" size={16} className="size-4" />
              {promoting ? "Aggiunta…" : "Aggiungi al database"}
            </Button>
          </div>
        </header>
      </div>

      {working && (
        <div className="island-section">
          <Card className="import-progress" role="status">
            <strong>{busy}</strong>
            <span>
              {progress
                ? `${progress.done} di ${progress.total} siti, ${progress.leads.toLocaleString("it-IT")} lead${progress.host ? ` · ${progress.host}` : ""}`
                : "In corso…"}
            </span>
            {progress && (
              <Progress
                value={(progress.done / Math.max(1, progress.total)) * 100}
                className="col-span-full w-full"
              />
            )}
            <div className="import-progress-actions">
              <Button
                variant="destructive"
                size="icon"
                aria-label="Ferma esplorazione"
                title="Ferma esplorazione"
                onClick={() => {
                  stopped.current = true;
                  void cancelLeadCrawl();
                }}
              >
                <Icon name="stop" />
              </Button>
            </div>
          </Card>
        </div>
      )}

      <div className="island-section search-panels">
        <div className="search-tabs" role="tablist" aria-label="Tipo di ricerca">
          {(
            [
              ["open", "Dati aperti", "Dataset scaricati e letti direttamente"],
              ["web", "Ricerca web", "Descrivi cosa cerchi o scegli i siti da leggere"],
            ] as const
          ).map(([value, label, hint]) => (
            <button
              key={value}
              type="button"
              role="tab"
              aria-selected={mode === value}
              className="search-tab"
              onClick={() => {
                if (mode === value) return;
                setMode(value);
                setSearchText("");
                setFilters(tabFilters(value));
              }}
            >
              <strong>{label}</strong>
              <span>{hint}</span>
            </button>
          ))}
        </div>

        {mode === "open" && (
          <Panel
            title="Dati aperti"
            description="Registri pubblici scaricati per intero: email, sito e indirizzo sono già scritti nel file."
            open
          >
            <Button onClick={importAll} disabled={working} className="justify-self-start">
              <Icon name="download" size={16} className="size-4" />
              Importa tutto
            </Button>
            <ul className="search-sources">
              {sources
                ?.filter((source) => source.kind === "dataset" && source.emails !== "no")
                .map((source) => (
                  <SourceCard
                    key={source.id}
                    title={source.name}
                    meta={`${source.publisher} · ${source.license}`}
                    note={source.note}
                  >
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => importSource(source)}
                      disabled={working}
                      aria-label={`Importa ${source.name}`}
                    >
                      Importa
                    </Button>
                  </SourceCard>
                ))}
            </ul>
          </Panel>
        )}
        {mode === "web" && (
          <>
            <Panel
              title="Cosa vuoi trovare?"
              description="Scrivilo con parole tue: l'IA sceglie le fonti, legge i siti e cerca anche sul web."
              open
            >
              <div className="search-query-block">
                <div className="search-query-row">
                  <div className="search-field">
                    <Icon name="search" size={16} className="search-field-icon" />
                    <Input
                      className="h-10 pl-9 text-sm"
                      value={query}
                      onChange={(event) => setQuery(event.currentTarget.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter" && !working) void startRequest();
                      }}
                      placeholder="Scrivi cosa cerchi, per esempio tutte le pro loco della Lombardia"
                      aria-label="Descrizione della ricerca lead"
                    />
                  </div>
                  <Button
                    className="h-10 px-4"
                    onClick={() => void startRequest()}
                    disabled={working || !query.trim()}
                  >
                    <Icon name="sparkle" size={16} className="size-4" />
                    Cerca
                  </Button>
                </div>
              </div>
            </Panel>

            <Panel
              title="Usa i siti già pubblici"
              description="Scegli dove cercare: l'app legge quei siti con l'IA e raccoglie le email."
              open
            >
              <ul className="search-sources">
                {places.map((place) => (
                  <SourceCard key={place.key} title={place.title} note={place.meta}>
                    <label>
                      <input
                        type="checkbox"
                        checked={picked.has(place.key)}
                        onChange={() => togglePlace(place.key)}
                        disabled={working}
                      />
                      Usa questo elenco
                    </label>
                  </SourceCard>
                ))}
              </ul>
              <Button
                onClick={readPlaces}
                disabled={working || picked.size === 0}
                className="justify-self-start"
              >
                <Icon name="sparkle" size={16} className="size-4" />
                Leggi i siti scelti
              </Button>
              {seedLists.some((list) => list.without_site > 0) && (
                <ul className="search-sources" aria-label="Enti senza sito">
                  {seedLists
                    .filter((list) => list.without_site > 0)
                    .map((list) => (
                      <SourceCard
                        key={`find:${list.list_name}`}
                        title={`${list.list_name}: enti senza sito`}
                        note={`${list.without_site.toLocaleString("it-IT")} enti noti solo per nome: la ricerca web trova il sito e lo controlla prima di leggerlo`}
                      >
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => findSites(list)}
                          disabled={working}
                          aria-label={`Trova i siti di ${list.list_name}`}
                        >
                          Trova i siti
                        </Button>
                      </SourceCard>
                    ))}
                </ul>
              )}
            </Panel>
          </>
        )}
      </div>

      <div className="island-section">
        <div className="filter-bar-row">
          <div className="search-field" role="search">
            <Icon name="search" size={16} className="search-field-icon" />
            <Input
              className="search-wide pl-9"
              value={searchText}
              onChange={(event) => onSearchChange(event.currentTarget.value)}
              placeholder="Cerca email, ente, persona, località…"
              aria-label="Cerca nei lead"
            />
          </div>
          <div className="actions-row">
            <Button variant="outline" disabled={!total} onClick={() => void exportEmails()}>
              <Icon name="download" size={16} className="size-4 text-faint" />
              Esporta email
            </Button>
            <Button
              variant="ghost"
              onClick={() => {
                setSearchText("");
                setFilters(tabFilters(mode));
              }}
            >
              Azzera filtri
            </Button>
          </div>
        </div>
        <div className="advanced-filters-grid">
          <MultiFilter
            label="Ricerca"
            options={options.run}
            value={filters.run}
            onChange={(run) => setFilters({ ...filters, run })}
            formatValue={(value) => runLabels.get(value) ?? value}
          />
          <MultiFilter
            label="Categoria"
            options={options.category}
            value={filters.category}
            onChange={(category) => setFilters({ ...filters, category })}
          />
          <MultiFilter
            label="Sottocategoria"
            options={options.subcategory}
            value={filters.subcategory}
            onChange={(subcategory) => setFilters({ ...filters, subcategory })}
          />
          <MultiFilter
            label="Regione"
            options={options.regione}
            value={filters.regione}
            onChange={(regione) => setFilters({ ...filters, regione })}
          />
          <MultiFilter
            label="Provincia"
            options={options.provincia}
            value={filters.provincia}
            onChange={(provincia) => setFilters({ ...filters, provincia })}
          />
          <MultiFilter
            label="Metodo"
            options={options.method}
            value={filters.method}
            onChange={(method) => setFilters({ ...filters, method })}
            formatValue={formatMethod}
          />
          <MultiFilter
            label="PEC"
            options={options.pec}
            value={filters.pec}
            onChange={(pec) => setFilters({ ...filters, pec })}
            formatValue={(value) => (value === "pec" ? "PEC" : "Mail ordinaria (standard)")}
          />
          <MultiFilter
            label="Stato"
            options={options.stato}
            value={filters.stato}
            onChange={(stato) => setFilters({ ...filters, stato })}
            formatValue={formatStato}
          />
        </div>
      </div>

      <div className="island-section island-section-flush">
        <div className="contacts-table-shell">
          <div className="contacts-table-wrap">
            <Table className="contacts-table" aria-label="Lead">
              <TableHeader>
                <TableRow>
                  {LEAD_COLUMNS.map((column) => (
                    <TableHead key={column}>{column}</TableHead>
                  ))}
                  <TableHead className="col-actions">azioni</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {leads.map((lead) => (
                  <LeadRow key={lead.id} lead={lead} onOpen={setSelected} />
                ))}
              </TableBody>
            </Table>
          </div>
        </div>
        {loading && (
          <p className="table-status" role="status">
            Caricamento lead…
          </p>
        )}
        {hasLoaded && !loading && leads.length === 0 && (
          <p className="table-empty-state">
            {runs.length === 0
              ? "Importa una fonte aperta, esplora un elenco o avvia una ricerca per vedere i lead."
              : "Nessun lead corrisponde ai filtri."}
          </p>
        )}
        <div className="table-toolbar">
          <div>
            <strong>Lead in area di prova</strong>
            <span>
              {total
                ? `${offset + 1}–${Math.min(offset + leads.length, total)} di ${total.toLocaleString("it-IT")}`
                : "Nessun risultato"}
            </span>
          </div>
          <div className="actions-row">
            <Button
              variant="outline"
              onClick={() => setOffset((value) => Math.max(0, value - PAGE_SIZE))}
              disabled={loading || offset === 0}
            >
              Precedente
            </Button>
            <Button
              variant="outline"
              onClick={() => setOffset((value) => value + PAGE_SIZE)}
              disabled={loading || offset + leads.length >= total}
            >
              Successiva
            </Button>
          </div>
        </div>
      </div>
      <DetailsSheet
        title={selected?.email ?? null}
        description={
          selected ? [selected.category, selected.subcategory].filter(Boolean).join(" / ") : ""
        }
        sections={selected ? leadSections(selected) : []}
        onClose={() => setSelected(null)}
      />
    </section>
  );
}

/** Memoized so typing in the filters does not re-render every cell of the page. */
const LeadRow = memo(function LeadRow({
  lead,
  onOpen,
}: {
  lead: SearchLeadView;
  onOpen: (lead: SearchLeadView) => void;
}) {
  return (
    <TableRow>
      <TableCell className="cell-primary">{lead.email}</TableCell>
      <TableCell>
        {[lead.organization, lead.department].filter(Boolean).join(" · ") ||
          lead.person_name ||
          "—"}
      </TableCell>
      <TableCell>{lead.category ?? "—"}</TableCell>
      <TableCell>{lead.subcategory ?? "—"}</TableCell>
      <TableCell>{lead.comune ?? "—"}</TableCell>
      <TableCell>{lead.provincia ?? "—"}</TableCell>
      <TableCell>{lead.regione ?? "—"}</TableCell>
      <TableCell>{lead.is_pec ? <Badge>PEC</Badge> : "—"}</TableCell>
      <TableCell className="cell-num">{lead.source_count}</TableCell>
      <TableCell>
        <span className={lead.stato === "completa" ? "cell-accent" : undefined}>
          {formatStato(lead.stato)}
        </span>
      </TableCell>
      <TableCell className="col-actions">
        <div className="row-actions">
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Dettagli"
            title="Dettagli"
            onClick={() => onOpen(lead)}
          >
            <Icon name="view" size={16} className="size-4" />
          </Button>
        </div>
      </TableCell>
    </TableRow>
  );
});

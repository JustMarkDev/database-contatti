import { memo, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  cancelLeadCrawl,
  discoverByMaps,
  discoverBySearch,
  estimateExplore,
  exploreDirectory,
  exploreSeeds,
  exportSearchLeadsTxt,
  findSeedSites,
  getContextdevSettings,
  getLeadFilterOptions,
  listLeadDirectories,
  listSearchLeads,
  planLeadRequest,
  listChannelYields,
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
import { loadExplorePrefs } from "../features/explorePrefs";
import { platform } from "../platform";
import type {
  ChannelYield,
  CrawlProgress,
  FieldFilter,
  LeadFilterOptions,
  LeadFilters,
  LeadDirectory,
  LeadPlan,
  SearchLeadView,
  SearchRunView,
  SeedListView,
} from "../types";

/**
 * Cerca lead: a sandbox that mirrors the contacts database.
 *
 * Directories, web search, Google Maps and site reads (all through
 * context.dev) land here first; **Aggiungi al database** promotes the
 * filtered leads into contacts. Sites to read are "seeds", grouped in lists.
 */

const PAGE_SIZE = 100;
const SITES_PER_RUN = 50;
/** Organizations known only by name searched per click (one credit each). */
const SITES_TO_FIND = 50;
/** Where web-search discoveries land (`lead_candidates::CHANNEL_SEARCH`). */
const SEARCH_LIST = "Scoperta: ricerca web";
/** Where Google Maps discoveries land (`lead_candidates::CHANNEL_MAPS`). */
const MAPS_LIST = "Scoperta: Google Maps";
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

/** A page section with its own heading, divided from the others by the island hairlines. */
function Step({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: ReactNode;
}) {
  return (
    <section className="island-section search-section" aria-label={title}>
      <header className="search-section-head">
        <h2>{title}</h2>
        <p>{description}</p>
      </header>
      {children}
    </section>
  );
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

type RunTotals = { lead_count: number; page_count: number };
const addTotals = (total: RunTotals, part: RunTotals | null): RunTotals => ({
  lead_count: total.lead_count + (part?.lead_count ?? 0),
  page_count: total.page_count + (part?.page_count ?? 0),
});

export function SearchPage({
  active,
  onContactsChanged,
}: {
  active: boolean;
  onContactsChanged: () => void;
}) {
  const [filters, setFilters] = useState<LeadFilters>(emptyLeadFilters);
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

  const [directories, setDirectories] = useState<LeadDirectory[] | null>(null);
  const [seedLists, setSeedLists] = useState<SeedListView[]>([]);
  const [mapsEnabled, setMapsEnabled] = useState(false);
  const [yields, setYields] = useState<ChannelYield[]>([]);

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
    if (!directories) {
      void listLeadDirectories()
        .then((catalog) => mounted.current && setDirectories(catalog))
        .catch(report);
    }
  }, [active, directories]);

  useEffect(() => {
    if (!active) return;
    const report = (error: unknown) => {
      if (mounted.current) toast.error(String(error));
    };
    void getLeadFilterOptions([])
      .then((value) => mounted.current && setOptions(value))
      .catch(report);
    void listSearchRuns()
      .then((value) => mounted.current && setRuns(value))
      .catch(report);
    void listSeedLists()
      .then((value) => mounted.current && setSeedLists(value))
      .catch(report);
    void getContextdevSettings()
      .then((value) => mounted.current && setMapsEnabled(value.maps_enabled))
      .catch(report);
    void listChannelYields()
      .then((value) => mounted.current && setYields(value))
      .catch(report);
  }, [active, refreshKey]);

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

  /** Free-form request: the IA picks directories and filters, then everything runs on its own. */
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
        const entry = directories?.find((candidate) => candidate.id === id);
        if (!entry) {
          problems.push(`Elenco sconosciuto: ${id}`);
          continue;
        }
        await attempt(entry.name, () => exploreDirectory({ sourceId: id, useAi: USE_AI }));
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
      if (mapsEnabled && plan.subcategory && plan.regione) {
        await attempt("Google Maps", async () => {
          const report = await discoverByMaps({
            category: plan.category,
            subcategory: plan.subcategory,
            regione: plan.regione,
          });
          newSites += report.accepted;
          return report.accepted > 0 ? readList(MAPS_LIST, plan) : null;
        });
      }
      if (mounted.current) {
        setSearchText("");
        setFilters({
          ...emptyLeadFilters(),
          category: { include: plan.category ? [plan.category] : [], exclude: [] },
          subcategory: { include: plan.subcategory ? [plan.subcategory] : [], exclude: [] },
          regione: { include: plan.regione ? [plan.regione] : [], exclude: [] },
        });
      }
      if (problems.length > 0) toast.error(problems.join(" · "));
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

  /** Reads pending sites of one list or matching a plan; nothing pending is not an error here. */
  async function readList(listName: string | null, plan?: LeadPlan) {
    const { pagesPerSite, schoolPdfs } = loadExplorePrefs();
    try {
      return await exploreSeeds({
        listName,
        category: plan?.category,
        subcategory: plan?.subcategory,
        regione: plan?.regione,
        limit: SITES_PER_RUN,
        maxPages: pagesPerSite,
        maxPdfs: schoolPdfs,
        useAi: USE_AI,
      });
    } catch (error) {
      if (String(error).includes("Nessun sito")) return null;
      throw error;
    }
  }

  /** One checkbox of "Usa i siti già pubblici": what it reads when started. */
  const places = useMemo(() => {
    const chained = new Set<string>();
    const items: {
      key: string;
      title: string;
      meta: string;
      /** The seed list it reads, for the estimate; directories add their own pages. */
      list: string;
      directory: boolean;
      run: () => Promise<RunTotals | null>;
    }[] = [];
    for (const entry of directories ?? []) {
      const list = `Elenco: ${entry.name}`;
      chained.add(list);
      items.push({
        key: entry.id,
        title: entry.name,
        meta: entry.note,
        list,
        directory: true,
        run: async () => {
          const own = await exploreDirectory({ sourceId: entry.id, useAi: USE_AI });
          return addTotals(own, await readList(list));
        },
      });
    }
    for (const list of seedLists) {
      if (list.pending === 0 || chained.has(list.list_name)) continue;
      items.push({
        key: `list:${list.list_name}`,
        title: list.list_name,
        list: list.list_name,
        directory: false,
        meta:
          `${list.pending.toLocaleString("it-IT")} siti ancora da leggere` +
          (list.to_verify ? ` · ${list.to_verify.toLocaleString("it-IT")} da verificare` : ""),
        run: () => readList(list.list_name),
      });
    }
    return items;
  }, [directories, seedLists]);

  function togglePlace(key: string) {
    setPicked((previous) => {
      const next = new Set(previous);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  }

  async function readPlaces() {
    const { pagesPerSite, schoolPdfs } = loadExplorePrefs();
    const chosen = places.filter((place) => picked.has(place.key));
    let sites = 0;
    let credits = 0;
    try {
      for (const place of chosen) {
        const estimate = await estimateExplore({
          listName: place.list,
          limit: SITES_PER_RUN,
          maxPages: pagesPerSite,
          maxPdfs: schoolPdfs,
          useAi: USE_AI,
        });
        sites += estimate.sites;
        credits += estimate.max_credits;
      }
    } catch (error) {
      toast.error(String(error));
      return;
    }
    const lists = chosen.some((place) => place.directory)
      ? " Gli elenchi aggiungono le loro pagine (1 credito ciascuna)."
      : "";
    const confirmed = await confirmDialog({
      title: "Leggere i siti scelti?",
      description: `≈ ${sites.toLocaleString("it-IT")} siti, al massimo ≈ ${credits.toLocaleString("it-IT")} crediti context.dev (Map + ${pagesPerSite} pagine per sito). La ricerca si ferma al tetto di crediti delle Impostazioni.${lists}`,
      actionLabel: "Leggi",
    });
    if (!confirmed) return;
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
              Cerca sul web o leggi gli elenchi pubblici con context.dev. I lead restano qui, con
              gli stessi campi dei contatti, finché non li aggiungi al database.
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
                ? `${progress.done} di ${progress.total} siti, ${progress.leads.toLocaleString("it-IT")} lead, ${(progress.credits_used ?? 0).toLocaleString("it-IT")} crediti${progress.host ? ` · ${progress.host}` : ""}`
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

      <Step
        title="Cosa vuoi trovare?"
        description="Scrivilo con parole tue: l'IA sceglie gli elenchi, legge i siti e cerca anche sul web."
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
      </Step>

      {seedLists.some((list) => list.without_site > 0) && (
        <Step
          title="1 · Trova i siti"
          description="Enti noti solo per nome: la ricerca web trova il loro sito e lo controlla prima che tu lo legga."
        >
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
        </Step>
      )}

      <Step
        title="2 · Leggi i siti"
        description="Scegli gli elenchi da leggere: l'app apre quei siti con l'IA e raccoglie le email."
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
          onClick={() => void readPlaces()}
          disabled={working || picked.size === 0}
          className="justify-self-start"
        >
          <Icon name="sparkle" size={16} className="size-4" />
          Leggi i siti scelti
        </Button>
      </Step>

      {yields.length > 0 && (
        <div className="island-section">
          <Panel
            title="Resa delle fonti di siti nuovi"
            description="Crediti spesi per trovare e leggere i siti di ogni fonte, e indirizzi nuovi trovati."
          >
            <Table aria-label="Resa per fonte">
              <TableHeader>
                <TableRow>
                  <TableHead>fonte</TableHead>
                  <TableHead>siti</TableHead>
                  <TableHead>crediti</TableHead>
                  <TableHead>email nuove</TableHead>
                  <TableHead>crediti per email</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {yields.map((row) => {
                  const spent = row.discovery_credits + row.reading_credits;
                  return (
                    <TableRow key={row.channel}>
                      <TableCell>{row.channel}</TableCell>
                      <TableCell className="cell-num">
                        {row.seeds.toLocaleString("it-IT")}
                      </TableCell>
                      <TableCell className="cell-num">{spent.toLocaleString("it-IT")}</TableCell>
                      <TableCell className="cell-num">
                        {row.new_emails.toLocaleString("it-IT")}
                      </TableCell>
                      <TableCell className="cell-num">
                        {row.new_emails
                          ? (spent / row.new_emails).toLocaleString("it-IT", {
                              maximumFractionDigits: 1,
                            })
                          : "—"}
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          </Panel>
        </div>
      )}

      <div className="island-section search-section">
        <header className="search-section-head">
          <h2>Lead trovati</h2>
          <p>Filtra, controlla ed esporta i lead prima di aggiungerli al database.</p>
        </header>
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
                setFilters(emptyLeadFilters());
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
              ? "Esplora un elenco o avvia una ricerca per vedere i lead."
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

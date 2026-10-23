import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "../components/ui/toast";
import { confirmDialog, confirmDialogWithCheckbox } from "../components/ui/confirm-dialog";
import {
  deleteContact,
  controlImport,
  exportContactEmailsCsv,
  exportContactsCsv,
  getFilterOptions,
  importContactPaths,
  listContacts,
  refreshDashboard,
} from "../api/contacts";
import { MultiFilter } from "../components/MultiFilter";
import { Icon } from "../components/Icon";
import { ScrollIndicators, useScrollActivity } from "../components/ScrollIndicators";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Input } from "../components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "../components/ui/popover";
import { Progress } from "../components/ui/progress";
import { Tooltip, TooltipContent, TooltipTrigger } from "../components/ui/tooltip";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "../components/ui/table";
import { contactCategories, emptyFilterOptions, emptyFilters } from "../features/filters";
import {
  formatClassification,
  formatContactKind,
  isUntrustedClassification,
} from "../features/contactFormat";
import { ContactDetailsSheet } from "../components/ContactDetailsSheet";
import { cn } from "../lib/utils";
import { platform } from "../platform";
import type {
  Contact,
  ContactSort,
  ExportReport,
  FilterOptions,
  Filters,
  ImportProgress,
} from "../types";

type Props = {
  active: boolean;
  refreshKey: number;
  busy: boolean;
  setBusy: (value: boolean) => void;
  onSummary: (total: number, blacklist: number, reviewCount: number) => void;
  onExport: (report: ExportReport) => void;
};

/**
 * Working columns of the contacts table; everything else is in the detail sheet.
 * Cerca lead mirrors them in `LEAD_COLUMNS`, without `punteggio`: a lead is scored
 * when it becomes a contact.
 */
export const CONTACT_COLUMNS = [
  "email",
  "ente",
  "categoria",
  "sottocategoria",
  "comune",
  "provincia",
  "regione",
  "pec",
  "fonti",
  "punteggio",
  "classificazione",
] as const;

/**
 * Sort key of each sortable column. Text columns start ascending; numeric ones
 * start with the highest value. A third click goes back to the default order.
 */
const SORT_COLUMNS: Record<string, { key: string; first: "asc" | "desc" }> = {
  email: { key: "email", first: "asc" },
  ente: { key: "organization", first: "asc" },
  categoria: { key: "category", first: "asc" },
  sottocategoria: { key: "subcategory", first: "asc" },
  comune: { key: "comune", first: "asc" },
  provincia: { key: "provincia", first: "asc" },
  regione: { key: "regione", first: "asc" },
  pec: { key: "pec", first: "desc" },
  fonti: { key: "sources", first: "desc" },
  punteggio: { key: "score", first: "desc" },
  classificazione: { key: "classification", first: "asc" },
};

/**
 * Fixed widths keep the columns where they are while sorting or paging swaps
 * the rows; longer values are cut with an ellipsis (the full value is in the
 * title and in the detail sheet).
 */
const COLUMN_WIDTHS: Record<(typeof CONTACT_COLUMNS)[number], number> = {
  email: 280,
  ente: 240,
  categoria: 170,
  sottocategoria: 180,
  comune: 150,
  provincia: 120,
  regione: 140,
  pec: 80,
  fonti: 80,
  punteggio: 100,
  classificazione: 180,
};
const ACTIONS_WIDTH = 100;
const TABLE_WIDTH =
  Object.values(COLUMN_WIDTHS).reduce((sum, width) => sum + width, 0) + ACTIONS_WIDTH;

export function nextSort(current: ContactSort | null, label: string): ContactSort | null {
  const { key, first } = SORT_COLUMNS[label];
  if (current?.column !== key) return { column: key, direction: first };
  if (current.direction === first)
    return { column: key, direction: first === "asc" ? "desc" : "asc" };
  return null;
}

export function ContactsPage({ active, refreshKey, busy, setBusy, onSummary, onExport }: Props) {
  const [contacts, setContacts] = useState<Contact[]>([]);
  const [total, setTotal] = useState(0);
  const [offset, setOffset] = useState(0);
  const [loading, setLoading] = useState(false);
  const [dataLoaded, setDataLoaded] = useState(false);
  const [filters, setFilters] = useState<Filters>(emptyFilters);
  const [options, setOptions] = useState<FilterOptions>(emptyFilterOptions);
  const [progress, setProgress] = useState<ImportProgress | null>(null);
  const [queuedBatches, setQueuedBatches] = useState(0);
  const [importPaused, setImportPaused] = useState(false);
  const [pauseToggleBusy, setPauseToggleBusy] = useState(false);
  const [importMenuOpen, setImportMenuOpen] = useState(false);
  const generation = useRef(0);
  const mounted = useRef(true);
  const importQueue = useRef<string[][]>([]);
  const importing = useRef(false);
  const completedBatchesFiles = useRef(0);
  const pauseRequestId = useRef(0);
  const hasLoaded = useRef(false);
  const lastLoadedFilters = useRef<Filters | null>(null);
  const lastLoadedRefreshKey = useRef(refreshKey);
  const searchTimer = useRef<number | null>(null);
  const lastImportRefresh = useRef(0);
  const [searchText, setSearchText] = useState("");
  const scroll = useScrollActivity();
  const offsetRef = useRef(0);
  offsetRef.current = offset;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      generation.current += 1;
    };
  }, []);
  const refresh = useCallback(
    async (mode: "reset" | "preserve" | "light" | "boot" | "filters" = "reset") => {
      const current = ++generation.current;
      setLoading(true);
      try {
        if (mode === "light" || mode === "filters") {
          const pageOffset = mode === "filters" ? 0 : offsetRef.current;
          if (mode === "filters") setOffset(0);
          const page = await listContacts(filters, pageOffset, true);
          if (!mounted.current || current !== generation.current) return;
          setContacts(page.contacts);
          setTotal(page.total);
          if (mode === "light") {
            setOffset((value) => Math.min(value, Math.max(0, page.total - 1)));
          }
          hasLoaded.current = true;
          setDataLoaded(true);
          if (mode === "filters") lastLoadedFilters.current = filters;
          return;
        }
        const withFacets = mode !== "boot";
        const result = await refreshDashboard(filters, withFacets);
        if (!mounted.current || current !== generation.current) return;
        let rows = result.filtered.contacts;
        let totalRows = result.filtered.total;
        if (mode === "preserve" && offsetRef.current > 0) {
          const page = await listContacts(filters, offsetRef.current, true);
          if (!mounted.current || current !== generation.current) return;
          rows = page.contacts;
          totalRows = page.total;
          setOffset((value) => Math.min(value, Math.max(0, totalRows - 1)));
        } else {
          setOffset(0);
        }
        setContacts(rows);
        setTotal(totalRows);
        if (withFacets) setOptions(result.filters);
        hasLoaded.current = true;
        setDataLoaded(true);
        lastLoadedFilters.current = filters;
        lastLoadedRefreshKey.current = refreshKey;
        onSummary(result.database_total, result.blacklist_count, result.pending_review_count);
        if (!withFacets) {
          void getFilterOptions()
            .then((next) => {
              if (mounted.current && current === generation.current) setOptions(next);
            })
            .catch((error) => {
              if (mounted.current && current === generation.current) toast.error(String(error));
            });
        }
      } catch (error) {
        if (mounted.current && current === generation.current) toast.error(String(error));
      } finally {
        if (mounted.current && current === generation.current) setLoading(false);
      }
    },
    [filters, onSummary, refreshKey],
  );
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const [selected, setSelected] = useState<Contact | null>(null);
  const removeContact = useCallback(async (contact: Contact) => {
    const confirmed = await confirmDialog({
      title: "Cancellare il contatto?",
      description: `${contact.email} esce da Contatti ed export. Le righe sorgente restano in quarantena e non tornano con la rivalidazione DNS.`,
      actionLabel: "Cancella contatto",
      variant: "destructive",
    });
    if (!confirmed) return;
    try {
      await deleteContact(contact.id);
      toast.success("Contatto rimosso", { description: contact.email });
    } catch (error) {
      toast.error(String(error));
      return;
    }
    await refreshRef.current("preserve");
  }, []);
  useEffect(() => {
    if (!active || busy) return;
    if (!hasLoaded.current) {
      void refresh("boot");
      return;
    }
    if (lastLoadedRefreshKey.current !== refreshKey) {
      void refresh("preserve");
      return;
    }
    if (lastLoadedFilters.current !== filters) {
      void refresh("filters");
    }
  }, [active, busy, filters, refresh, refreshKey]);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void platform
      .listen<ImportProgress>("import-progress", (event) => {
        if (!mounted.current) return;
        setProgress(event.payload);

        if (event.payload.phase === "saved") {
          const now = Date.now();
          if (now - lastImportRefresh.current >= 1_500) {
            lastImportRefresh.current = now;
            void refresh("light");
          }
        }
      })
      .then((cleanup) => {
        unlisten = cleanup;
      });
    return () => unlisten?.();
  }, [refresh]);
  useEffect(() => {
    setOffset(0);
  }, [filters]);
  useEffect(() => {
    if (!active || offset === 0) return;
    const current = ++generation.current;
    setLoading(true);
    void listContacts(filters, offset, false)
      .then((result) => {
        if (mounted.current && current === generation.current) {
          setContacts(result.contacts);
          setTotal((previous) => Math.max(previous, result.total));
          setDataLoaded(true);
        }
      })
      .catch((error) => {
        if (mounted.current && current === generation.current) toast.error(String(error));
      })
      .finally(() => {
        if (mounted.current && current === generation.current) setLoading(false);
      });
  }, [active, filters, offset]);

  async function drainImportQueue() {
    if (importing.current) return;
    importing.current = true;
    completedBatchesFiles.current = 0;
    setBusy(true);
    try {
      while (importQueue.current.length > 0) {
        const paths = importQueue.current.shift() ?? [];
        setQueuedBatches(importQueue.current.length);
        setProgress((prev) => ({
          phase: "parsing",
          file: prev?.file ?? `${paths.length} elementi in coda`,
          processed_files: 0,
          total_files: prev?.total_files ?? 0,
          imported: prev?.imported ?? 0,
        }));
        try {
          const current = await importContactPaths(paths);
          completedBatchesFiles.current += current.files.length;
          await refresh();
          toast.success("Importazione completata", {
            description: `${current.new_canonical_contacts.toLocaleString("it-IT")} nuovi contatti, ${current.existing_contacts_updated.toLocaleString("it-IT")} aggiornati; ${current.candidate_occurrences.toLocaleString("it-IT")} occorrenze candidate riconciliate da ${current.files.length} file.`,
          });
        } catch (error) {
          if (String(error).includes("Importazione annullata")) {
            importQueue.current = [];
            setQueuedBatches(0);
            await refresh();
            toast.info("Importazione interrotta", {
              description:
                "L'importazione è stata fermata. I contatti dei file già completati sono stati conservati.",
            });
            break;
          } else {
            toast.error(String(error));
          }
        }
      }
    } finally {
      completedBatchesFiles.current = 0;
      importing.current = false;
      setQueuedBatches(0);
      setBusy(false);
      setProgress(null);
      setImportPaused(false);
      setPauseToggleBusy(false);
    }
  }

  async function toggleImportPause() {
    if (pauseToggleBusy) return;
    const nextPaused = !importPaused;
    const requestId = ++pauseRequestId.current;
    setPauseToggleBusy(true);
    try {
      await controlImport(nextPaused ? "pause" : "resume");

      if (requestId === pauseRequestId.current) setImportPaused(nextPaused);
    } finally {
      if (requestId === pauseRequestId.current) setPauseToggleBusy(false);
    }
  }

  async function cancelImport() {
    importQueue.current = [];
    setQueuedBatches(0);
    // The running import rejects with "Importazione annullata"; drainImportQueue
    // then refreshes and releases `busy`, so a new import cannot overlap it.
    try {
      await controlImport("cancel");
    } catch (error) {
      toast.error(String(error));
    }
  }

  async function importContacts() {
    const selected = await platform.openFiles({
      multiple: true,
      filters: [
        {
          name: "Contatti",
          extensions: ["xlsx", "xlsm", "xlsb", "xla", "xlam", "ods", "odt", "csv", "txt", "tsv"],
        },
      ],
    });
    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];
    importQueue.current.push(paths);
    setQueuedBatches(importQueue.current.length);
    void drainImportQueue();
  }

  async function importFolders() {
    const selected = await platform.openFiles({ directory: true, multiple: true });
    if (!selected) return;
    importQueue.current.push(Array.isArray(selected) ? selected : [selected]);
    setQueuedBatches(importQueue.current.length);
    void drainImportQueue();
  }
  async function exportAll() {
    try {
      const choice = await confirmDialogWithCheckbox({
        title: "Esportare tutti i campi?",
        description:
          "Per impostazione predefinita sono esclusi i contatti la cui classificazione ha evidenze insufficienti, è in conflitto o presenta una lacuna tassonomica.",
        checkboxLabel: "Includi anche classificazioni da verificare",
        actionLabel: "Scegli destinazione",
      });
      if (!choice.confirmed) return;
      const path = await platform.saveFile({
        defaultPath: `contatti-${new Date().toISOString().slice(0, 10)}.csv`,
        filters: [{ name: "CSV", extensions: ["csv"] }],
      });
      if (path) onExport(await exportContactsCsv(filters, path, choice.checked));
    } catch (error) {
      toast.error(String(error));
    }
  }
  async function exportEmails() {
    try {
      const choice = await confirmDialogWithCheckbox({
        title: "Esportare gli indirizzi email?",
        description:
          "Per impostazione predefinita sono esclusi i contatti la cui classificazione ha evidenze insufficienti, è in conflitto o presenta una lacuna tassonomica.",
        checkboxLabel: "Includi anche classificazioni da verificare",
        actionLabel: "Scegli destinazione",
      });
      if (!choice.confirmed) return;
      const path = await platform.saveFile({
        defaultPath: `email-contatti-${new Date().toISOString().slice(0, 10)}.csv`,
        filters: [{ name: "CSV", extensions: ["csv"] }],
      });
      if (path) onExport(await exportContactEmailsCsv(filters, path, choice.checked));
    } catch (error) {
      toast.error(String(error));
    }
  }
  function onSearchChange(value: string) {
    setSearchText(value);
    if (searchTimer.current) window.clearTimeout(searchTimer.current);
    searchTimer.current = window.setTimeout(() => {
      setFilters((previous) => ({ ...previous, query: value }));
    }, 300);
  }

  function previousPage() {
    if (offset <= 100) loadContactPage(0);
    else setOffset((value) => Math.max(0, value - 100));
  }

  function loadContactPage(nextOffset: number) {
    const current = ++generation.current;
    setLoading(true);
    void listContacts(filters, nextOffset, nextOffset === 0)
      .then((result) => {
        if (mounted.current && current === generation.current) {
          setContacts(result.contacts);
          setTotal((previous) =>
            nextOffset === 0 ? result.total : Math.max(previous, result.total),
          );
          setDataLoaded(true);
          setOffset(nextOffset);
        }
      })
      .catch((error) => {
        if (mounted.current && current === generation.current) toast.error(String(error));
      })
      .finally(() => {
        if (mounted.current && current === generation.current) setLoading(false);
      });
  }
  const categoryOptions = useMemo(() => {
    const counts = new Map(options.category.map((option) => [option.value, option.count]));
    const canonical = contactCategories.map((value) => ({ value, count: counts.get(value) ?? 0 }));
    const legacy = options.category.filter((option) => !contactCategories.includes(option.value));
    return [...canonical, ...legacy];
  }, [options.category]);
  if (!active) return null;
  return (
    <section className="content-island" aria-label="Contatti">
      <div className="island-section island-section-header">
        <header className="topbar">
          <div>
            <h1>Contatti</h1>
            <p className="topbar-description">Indirizzi importati, deduplicati e classificati.</p>
          </div>
          <div className="header-metrics">
            <Popover open={importMenuOpen} onOpenChange={setImportMenuOpen}>
              <PopoverTrigger render={<Button />}>
                <Icon name="upload" size={16} className="size-4" />
                Importa contatti
              </PopoverTrigger>
              <PopoverContent className="w-52 p-1" align="end">
                <div className="flex flex-col" role="menu" aria-label="Tipo di importazione">
                  <Button
                    variant="ghost"
                    className="justify-start text-foreground"
                    role="menuitem"
                    onClick={() => {
                      setImportMenuOpen(false);
                      void importContacts();
                    }}
                  >
                    <Icon name="file" size={16} className="size-4 text-faint" />
                    Seleziona file
                  </Button>
                  <Button
                    variant="ghost"
                    className="justify-start text-foreground"
                    role="menuitem"
                    onClick={() => {
                      setImportMenuOpen(false);
                      void importFolders();
                    }}
                  >
                    <Icon name="folder" size={16} className="size-4 text-faint" />
                    Seleziona cartelle
                  </Button>
                </div>
              </PopoverContent>
            </Popover>
          </div>
        </header>
      </div>
      {progress &&
        (() => {
          const currentBatchProcessed = progress.processed_files;
          const currentBatchTotal = progress.total_files;
          const displayProcessed = completedBatchesFiles.current + currentBatchProcessed;
          const displayTotal = Math.max(1, completedBatchesFiles.current + currentBatchTotal);
          const progressPercent = Math.min(
            100,
            Math.max(0, (displayProcessed / displayTotal) * 100),
          );
          return (
            <div className="island-section">
              <Card className="import-progress" role="status">
                <strong>{importPaused ? "Importazione in pausa" : "Importazione in corso"}</strong>
                <span>
                  {displayProcessed} di {displayTotal} file
                  {queuedBatches > 0 ? ` (+${queuedBatches} in coda)` : ""}
                </span>
                <Progress value={progressPercent} className="col-span-full w-full" />
                <div className="import-progress-actions">
                  <Button
                    variant="outline"
                    size="icon"
                    aria-label={
                      importPaused ? "Riprendi importazione" : "Metti in pausa importazione"
                    }
                    title={importPaused ? "Riprendi importazione" : "Metti in pausa importazione"}
                    onClick={() => void toggleImportPause()}
                    disabled={pauseToggleBusy}
                  >
                    <Icon name={importPaused ? "play" : "pause"} />
                    <span className="sr-only">{importPaused ? "Riprendi" : "Metti in pausa"}</span>
                  </Button>
                  <Button
                    variant="destructive"
                    size="icon"
                    aria-label="Ferma importazione"
                    title="Ferma importazione"
                    onClick={() => void cancelImport()}
                  >
                    <Icon name="stop" />
                    <span className="sr-only">Ferma importazione</span>
                  </Button>
                </div>
              </Card>
            </div>
          );
        })()}
      <div className="island-section">
        <div className="filter-bar-row">
          <div className="search-field" role="search">
            <Icon name="search" size={16} className="search-field-icon" />
            <Input
              className="search-wide pl-9"
              value={searchText}
              onChange={(e) => onSearchChange(e.currentTarget.value)}
              placeholder="Cerca email, file, categoria, località…"
            />
          </div>
          <div className="actions-row">
            <Tooltip>
              <TooltipTrigger
                render={<Button variant="outline" disabled={!total} onClick={exportEmails} />}
              >
                <Icon name="download" size={16} className="size-4 text-faint" />
                Esporta email
              </TooltipTrigger>
              <TooltipContent>Esporta solo gli indirizzi email filtrati.</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger
                render={<Button variant="outline" disabled={!total} onClick={exportAll} />}
              >
                <Icon name="download" size={16} className="size-4 text-faint" />
                Esporta tutto
              </TooltipTrigger>
              <TooltipContent>Esporta tutti i dati dei contatti filtrati.</TooltipContent>
            </Tooltip>
            <Button
              variant="ghost"
              onClick={() => {
                setSearchText("");
                setFilters({ ...emptyFilters(), sort: filters.sort });
              }}
            >
              Azzera filtri
            </Button>
          </div>
        </div>
        <div className="advanced-filters-grid">
          <MultiFilter
            label="File origine"
            options={options.source_file}
            value={filters.source_file}
            onChange={(source_file) => setFilters({ ...filters, source_file })}
          />
          <MultiFilter
            label="Categoria"
            options={categoryOptions}
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
            label="Tipo contatto"
            options={options.contact_kind}
            value={filters.contact_kind}
            onChange={(contact_kind) => setFilters({ ...filters, contact_kind })}
            formatValue={formatContactKind}
          />
          <MultiFilter
            label="Paese"
            options={options.paese}
            value={filters.paese}
            onChange={(paese) => setFilters({ ...filters, paese })}
          />
          <MultiFilter
            label="Macroarea"
            options={options.macroarea}
            value={filters.macroarea}
            onChange={(macroarea) => setFilters({ ...filters, macroarea })}
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
            label="Comune"
            options={options.comune}
            value={filters.comune}
            onChange={(comune) => setFilters({ ...filters, comune })}
          />
          <MultiFilter
            label="PEC"
            options={options.pec}
            value={filters.pec}
            onChange={(pec) => setFilters({ ...filters, pec })}
            formatValue={(value) => (value === "pec" ? "PEC" : "Mail ordinaria (standard)")}
          />
        </div>
        <div className="score-filter-row">
          <label htmlFor="contact-score-threshold">Punteggio minimo</label>
          <input
            id="contact-score-threshold"
            type="range"
            min={0}
            max={100}
            step={5}
            value={filters.min_score ?? 0}
            onChange={(event) => {
              const value = Number(event.currentTarget.value);
              setFilters({ ...filters, min_score: value === 0 ? null : value });
            }}
          />
          <output aria-hidden="true">{filters.min_score ?? 0}</output>
        </div>
      </div>
      <div className="island-section island-section-flush">
        <div className="contacts-table-shell">
          <div ref={scroll.ref} className="contacts-table-wrap" onScroll={scroll.onScroll}>
            <Table className="contacts-table contacts-table-fixed" style={{ width: TABLE_WIDTH }}>
              <colgroup>
                {CONTACT_COLUMNS.map((h) => (
                  <col key={h} style={{ width: COLUMN_WIDTHS[h] }} />
                ))}
                <col style={{ width: ACTIONS_WIDTH }} />
              </colgroup>
              <TableHeader>
                <TableRow>
                  {CONTACT_COLUMNS.map((h) => {
                    const active =
                      filters.sort?.column === SORT_COLUMNS[h].key ? filters.sort : null;
                    return (
                      <TableHead
                        key={h}
                        aria-sort={
                          active
                            ? active.direction === "asc"
                              ? "ascending"
                              : "descending"
                            : "none"
                        }
                      >
                        <button
                          type="button"
                          className="sort-header"
                          data-direction={active?.direction}
                          onClick={() =>
                            setFilters({ ...filters, sort: nextSort(filters.sort, h) })
                          }
                        >
                          {h}
                        </button>
                      </TableHead>
                    );
                  })}
                  <TableHead className="col-actions">azioni</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {contacts.map((c) => (
                  <ContactRow
                    key={c.id}
                    contact={c}
                    onOpen={setSelected}
                    onDelete={removeContact}
                  />
                ))}
              </TableBody>
            </Table>
          </div>
          <ScrollIndicators activity={scroll} />
        </div>
        {loading && (
          <p className="table-status" role="status">
            Caricamento contatti…
          </p>
        )}
        {dataLoaded && !loading && contacts.length === 0 && (
          <p className="table-empty-state">Nessun contatto corrisponde ai filtri.</p>
        )}
        <div className="table-toolbar">
          <div>
            <strong>Elenco contatti</strong>
            <span>{total.toLocaleString("it-IT")} risultati</span>
            <span>
              {total
                ? `${offset + 1}–${Math.min(offset + contacts.length, total)} di ${total.toLocaleString("it-IT")}`
                : "Nessun risultato"}
            </span>
          </div>
          <div className="actions-row">
            <Button variant="outline" onClick={previousPage} disabled={loading || !offset}>
              <Icon name="collapse" size={16} className="size-4" />
              Precedente
            </Button>
            <Button
              variant="outline"
              onClick={() => setOffset((v) => v + 100)}
              disabled={loading || offset + contacts.length >= total}
            >
              Successiva
              <Icon name="expand" size={16} className="size-4" />
            </Button>
          </div>
        </div>
      </div>
      <ContactDetailsSheet contact={selected} onClose={() => setSelected(null)} />
    </section>
  );
}

/** Memoized so typing in the filters does not re-render every cell of the page. */
const ContactRow = memo(function ContactRow({
  contact: c,
  onOpen,
  onDelete,
}: {
  contact: Contact;
  onOpen: (contact: Contact) => void;
  onDelete: (contact: Contact) => void;
}) {
  return (
    <TableRow>
      <TableCell className="cell-primary" title={c.email}>
        {c.email}
      </TableCell>
      <TableCell title={c.organization_name ?? c.denomination ?? undefined}>
        {c.organization_name ?? c.denomination ?? "—"}
      </TableCell>
      <TableCell title={c.category}>{c.category}</TableCell>
      <TableCell title={c.subcategory ?? undefined}>{c.subcategory ?? "—"}</TableCell>
      <TableCell title={c.comune ?? undefined}>{c.comune ?? "—"}</TableCell>
      <TableCell title={c.provincia ?? undefined}>{c.provincia ?? "—"}</TableCell>
      <TableCell title={c.regione ?? undefined}>{c.regione ?? "—"}</TableCell>
      <TableCell>{c.is_pec ? <Badge>PEC</Badge> : "—"}</TableCell>
      <TableCell className="cell-num">{c.independent_source_count}</TableCell>
      <TableCell className="cell-num">{c.quality_score ?? "—"}</TableCell>
      <TableCell>
        <span className={cn(isUntrustedClassification(c.classification_state) && "cell-warn")}>
          {formatClassification(c.classification_method, c.classification_state)}
        </span>
      </TableCell>
      <TableCell className="col-actions">
        <div className="row-actions">
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Dettagli"
            title="Dettagli"
            onClick={() => onOpen(c)}
          >
            <Icon name="view" size={16} className="size-4" />
          </Button>
          <Button
            variant="destructive"
            size="icon-sm"
            aria-label="Cancella"
            title="Cancella"
            onClick={() => onDelete(c)}
          >
            <Icon name="delete" size={16} className="size-4" />
          </Button>
        </div>
      </TableCell>
    </TableRow>
  );
});

import { useEffect, useRef, useState } from "react";
import { toast } from "../components/ui/toast";
import {
  addBlacklistEntry,
  exportBlacklistCsv,
  exportBlacklistEmailsCsv,
  importBlacklistPaths,
  listBlacklist,
  removeBlacklistEntry,
} from "../api/blacklist";
import { ScrollIndicators, useScrollActivity } from "../components/ScrollIndicators";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardHeader } from "../components/ui/card";
import { Input } from "../components/ui/input";
import { Icon } from "../components/Icon";
import { Tooltip, TooltipContent, TooltipTrigger } from "../components/ui/tooltip";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "../components/ui/table";
import { platform } from "../platform";
import type { BlacklistEntry, ExportReport } from "../types";

type Props = {
  active: boolean;
  busy: boolean;
  setBusy: (value: boolean) => void;
  count: number;
  onCount: (value: number) => void;
  /** Blacklisting deletes matching contacts: the contacts view must reload. */
  onContactsChanged: () => void;
  onExport: (report: ExportReport) => void;
};

export function BlacklistPage({
  active,
  busy,
  setBusy,
  count,
  onCount,
  onContactsChanged,
  onExport,
}: Props) {
  const [entries, setEntries] = useState<BlacklistEntry[]>([]);
  const [total, setTotal] = useState(0);
  const [query, setQuery] = useState("");
  const [offset, setOffset] = useState(0);
  const [loading, setLoading] = useState(false);
  const [hasLoaded, setHasLoaded] = useState(false);
  const [newEmail, setNewEmail] = useState("");
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<string | null>(null);
  const generation = useRef(0);
  const mounted = useRef(true);
  const scroll = useScrollActivity();
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      generation.current += 1;
    };
  }, []);
  useEffect(() => {
    setOffset(0);
  }, [query]);
  useEffect(() => {
    const current = ++generation.current;
    if (!active) return;
    const timer = window.setTimeout(() => {
      setLoading(true);
      void listBlacklist(query, offset)
        .then((result) => {
          if (!mounted.current || current !== generation.current) return;
          setEntries(result.entries);
          setTotal(result.total);
          setHasLoaded(true);
          // A search total is not the blacklist size.
          if (!query.trim()) onCount(result.total);
        })
        .catch((error) => {
          if (mounted.current && current === generation.current) toast.error(String(error));
        })
        .finally(() => {
          if (mounted.current && current === generation.current) setLoading(false);
        });
    }, 150);
    return () => {
      window.clearTimeout(timer);
      if (generation.current === current) generation.current += 1;
    };
  }, [active, count, offset, onCount, query]);
  async function importFiles() {
    const selected = await platform.openFiles({
      multiple: true,
      filters: [{ name: "Blacklist", extensions: ["csv", "txt"] }],
    });
    if (!selected) return;
    setBusy(true);
    try {
      const next = await importBlacklistPaths(Array.isArray(selected) ? selected : [selected]);
      setOffset(0);
      onCount(count + next.inserted);
      generation.current += 1;
      onContactsChanged();
      toast.success(
        `Importazione completata: ${next.inserted.toLocaleString("it-IT")} indirizzi validi e unici aggiunti alla blacklist.`,
        {
          description: `${next.duplicates.toLocaleString("it-IT")} duplicati e ${next.invalid.toLocaleString("it-IT")} indirizzi non validi ignorati.`,
        },
      );
      if (next.failed_files.length) {
        toast.error(`${next.failed_files.length} file non importati`, {
          description: next.failed_files.join("\n"),
        });
      }
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
    }
  }
  async function handleAddEmail(e?: React.FormEvent) {
    if (e) e.preventDefault();
    const clean = newEmail.trim();
    if (!clean) return;
    setAdding(true);
    try {
      const inserted = await addBlacklistEntry(clean);
      setNewEmail("");
      if (inserted) {
        onCount(count + 1);
        onContactsChanged();
        toast.success("Indirizzo aggiunto alla blacklist", { description: clean });
      } else {
        toast.info("Indirizzo già presente nella blacklist", { description: clean });
      }
      const current = ++generation.current;
      setOffset(0);
      void listBlacklist(query, 0)
        .then((result) => {
          if (!mounted.current || current !== generation.current) return;
          setEntries(result.entries);
          setTotal(result.total);
          setHasLoaded(true);
          // A search total is not the blacklist size.
          if (!query.trim()) onCount(result.total);
        })
        .catch((error) => {
          if (mounted.current && current === generation.current) toast.error(String(error));
        });
    } catch (error) {
      toast.error(String(error));
    } finally {
      setAdding(false);
    }
  }
  async function exportAll() {
    try {
      const path = await platform.saveFile({
        defaultPath: `blacklist-${new Date().toISOString().slice(0, 10)}.csv`,
        filters: [{ name: "CSV", extensions: ["csv"] }],
      });
      if (path) onExport(await exportBlacklistCsv(query, path));
    } catch (error) {
      toast.error(String(error));
    }
  }
  async function exportEmails() {
    try {
      const path = await platform.saveFile({
        defaultPath: `email-blacklist-${new Date().toISOString().slice(0, 10)}.csv`,
        filters: [{ name: "CSV", extensions: ["csv"] }],
      });
      if (path) onExport(await exportBlacklistEmailsCsv(query, path));
    } catch (error) {
      toast.error(String(error));
    }
  }
  async function remove(email: string) {
    setRemoving(email);
    try {
      await removeBlacklistEntry(email);
      setOffset(0);
      onCount(Math.max(0, count - 1));
      toast.success("Indirizzo rimosso dalla blacklist", { description: email });
    } catch (error) {
      toast.error(String(error));
    } finally {
      setRemoving(null);
    }
  }
  if (!active) return null;
  return (
    <section className="content-island" aria-label="Blacklist">
      <div className="island-section island-section-header">
        <header className="topbar">
          <div>
            <h1>Blacklist</h1>
            <p className="topbar-description">
              Indirizzi che non devono mai essere contattati. Aggiungerli rimuove subito i contatti
              corrispondenti e li esclude da ogni export.
            </p>
          </div>
          <div className="header-metrics">
            <div>
              <span>In blacklist</span>
              <strong>{count.toLocaleString("it-IT")}</strong>
            </div>
            <Button onClick={importFiles} disabled={busy}>
              <Icon name="upload" size={16} className="size-4" />
              Importa blacklist
            </Button>
          </div>
        </header>
      </div>
      <Card className="island-section blacklist-panel">
        <CardHeader className="section-heading section-heading-row">
          <h2>Indirizzi esclusi</h2>
          <div className="actions-row">
            <Tooltip>
              <TooltipTrigger
                render={<Button variant="outline" onClick={exportEmails} disabled={!count} />}
              >
                <Icon name="download" size={16} className="size-4 text-faint" />
                Esporta email
              </TooltipTrigger>
              <TooltipContent>
                Esporta solo gli indirizzi email filtrati della blacklist.
              </TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger
                render={<Button variant="outline" onClick={exportAll} disabled={!count} />}
              >
                <Icon name="download" size={16} className="size-4 text-faint" />
                Esporta tutto
              </TooltipTrigger>
              <TooltipContent>Esporta tutti i dati filtrati della blacklist.</TooltipContent>
            </Tooltip>
          </div>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          <div className="blacklist-toolbar">
            <div className="search-field" role="search">
              <Icon name="search" size={16} className="search-field-icon" />
              <Input
                className="search-wide pl-9"
                value={query}
                onChange={(e) => setQuery(e.currentTarget.value)}
                placeholder="Cerca email o file di origine…"
              />
            </div>
            <form onSubmit={handleAddEmail} className="inline-form">
              <Input
                type="email"
                value={newEmail}
                onChange={(e) => setNewEmail(e.currentTarget.value)}
                placeholder="Aggiungi un'email alla blacklist (es. info@esempio.it)…"
                disabled={adding || busy}
              />
              <Button type="submit" variant="outline" disabled={!newEmail.trim() || adding || busy}>
                <Icon name="plus" size={16} className="size-4 text-faint" />
                {adding ? "Aggiunta…" : "Aggiungi"}
              </Button>
            </form>
          </div>
          <div className="contacts-table-shell">
            <div ref={scroll.ref} className="contacts-table-wrap" onScroll={scroll.onScroll}>
              <Table className="contacts-table blacklist-table">
                <TableHeader>
                  <TableRow>
                    <TableHead>email</TableHead>
                    <TableHead>file origine</TableHead>
                    <TableHead>data importazione</TableHead>
                    <TableHead>nota</TableHead>
                    <TableHead className="col-actions">azioni</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {entries.map((entry) => (
                    <TableRow key={entry.email}>
                      <TableCell className="cell-primary">{entry.email}</TableCell>
                      <TableCell>{entry.source_file}</TableCell>
                      <TableCell>{new Date(entry.imported_at).toLocaleString("it-IT")}</TableCell>
                      <TableCell>{entry.note ?? "—"}</TableCell>
                      <TableCell className="col-actions">
                        <Button
                          variant="destructive"
                          size="sm"
                          onClick={() => void remove(entry.email)}
                          disabled={removing !== null || busy}
                        >
                          {removing === entry.email ? "Rimozione…" : "Rimuovi"}
                        </Button>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
            <ScrollIndicators activity={scroll} />
          </div>
          {loading && (
            <p className="table-status" role="status">
              Caricamento blacklist…
            </p>
          )}
          {hasLoaded && !loading && !entries.length && (
            <p className="table-empty-state">Nessuna voce corrisponde alla ricerca.</p>
          )}
          <div className="table-toolbar">
            <span>
              {total
                ? `${offset + 1}–${Math.min(offset + entries.length, total)} di ${total}`
                : "Nessun risultato"}
            </span>
            <div className="actions-row">
              <Button
                variant="outline"
                onClick={() => setOffset((v) => Math.max(0, v - 100))}
                disabled={loading || !offset}
              >
                Precedente
              </Button>
              <Button
                variant="outline"
                onClick={() => setOffset((v) => v + 100)}
                disabled={loading || offset + entries.length >= total}
              >
                Successiva
              </Button>
            </div>
          </div>
        </CardContent>
      </Card>
    </section>
  );
}

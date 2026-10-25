import { lazy, Suspense, useCallback, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { Toaster, toast } from "./components/ui/toast";
import { ConfirmDialogHost } from "./components/ui/confirm-dialog";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarTrigger,
  useSidebar,
} from "./components/ui/sidebar";
import { TooltipProvider } from "./components/ui/tooltip";
import { Icon } from "./components/Icon";
import { ContactsPage } from "./pages/ContactsPage";
import { refreshDashboard } from "./api/contacts";
import { emptyFilters } from "./features/filters";
import { resolveWindowChrome, usesMacOverlayTitlebar } from "./lib/windowChrome";
import { cn } from "./lib/utils";
import type { ExportReport, Page } from "./types";

/**
 * Unfinished pages: real page in dev, "Lavori in corso" in release builds.
 * `import.meta.env.DEV` is a build-time constant, so the dead branch (and the page's
 * chunk) is dropped from `tauri build`; remove the gate once a page is ready.
 */
function workInProgress(title: string) {
  return function WorkInProgress({ active }: { active: boolean }) {
    if (!active) return null;
    return (
      <section className="content-island" aria-label={title}>
        <div className="wip-notice" role="status">
          <h1>Lavori in corso</h1>
          <p>{title} non è ancora disponibile.</p>
        </div>
      </section>
    );
  };
}

const AiPage = lazy(async () => ({
  default: (await import("./pages/AiPage")).AiPage,
}));
const BlacklistPage = lazy(async () => ({
  default: (await import("./pages/BlacklistPage")).BlacklistPage,
}));
const SearchPage = import.meta.env.DEV
  ? lazy(async () => ({
      default: (await import("./pages/SearchPage")).SearchPage,
    }))
  : workInProgress("Cerca lead");
const SettingsPage = lazy(async () => ({
  default: (await import("./pages/SettingsPage")).SettingsPage,
}));

type DeferredPage = Exclude<Page, "contacts">;

function PageLoading({ name }: { name: string }) {
  return (
    <div className="page-loading" role="status" aria-live="polite">
      Caricamento {name}…
    </div>
  );
}

function TitlebarSidebarTrigger({ className }: { className?: string }) {
  const { state, isMobile, openMobile } = useSidebar();
  let label = state === "collapsed" ? "Espandi barra laterale" : "Comprimi barra laterale";
  if (isMobile) label = openMobile ? "Chiudi menu" : "Apri menu";
  return (
    <SidebarTrigger
      aria-label={label}
      title={label}
      className={cn("titlebar-sidebar-trigger", className)}
    />
  );
}

/** Fixed chrome: toggle stays pinned next to traffic lights and never scrolls with pages. */
function WindowChrome({ overlay }: { overlay: boolean }) {
  const { isMobile } = useSidebar();
  if (isMobile || typeof document === "undefined") return null;
  return createPortal(
    <div
      className={cn("window-chrome-layer", overlay && "window-chrome-overlay")}
      aria-hidden={false}
    >
      {overlay && <div className="window-drag-region" data-tauri-drag-region aria-hidden="true" />}
      <div className="window-traffic-toggle">
        <TitlebarSidebarTrigger className="window-traffic-toggle-button" />
      </div>
    </div>,
    document.body,
  );
}

function SidebarBrand({ overlay }: { overlay: boolean }) {
  return (
    <div className={cn("brand", overlay && "brand-overlay")}>
      <span className="brand-mark" aria-hidden="true">
        <Icon name="contacts" strokeWidth={2} />
      </span>
      <div className="sidebar-copy group-data-[collapsible=icon]:hidden">
        <strong>Database Contatti</strong>
      </div>
    </div>
  );
}

function SidebarStatus({ contacts, blacklist }: { contacts: number; blacklist: number }) {
  return (
    <div className="sidebar-status group-data-[collapsible=icon]:hidden">
      <dl>
        <div>
          <dt>Contatti</dt>
          <dd>{contacts.toLocaleString("it-IT")}</dd>
        </div>
        <div>
          <dt>Blacklist</dt>
          <dd>{blacklist.toLocaleString("it-IT")}</dd>
        </div>
      </dl>
    </div>
  );
}

function InsetChrome() {
  const { isMobile } = useSidebar();

  if (isMobile) {
    return (
      <div className="mobile-app-bar">
        <TitlebarSidebarTrigger />
      </div>
    );
  }

  return null;
}

export default function App() {
  const [activePage, setActivePage] = useState<Page>("contacts");

  const [loadedPages, setLoadedPages] = useState<Set<DeferredPage>>(() => new Set());
  const [busy, setBusy] = useState(false);
  const [databaseTotal, setDatabaseTotal] = useState(0);
  const [blacklistCount, setBlacklistCount] = useState(0);
  const [reviewCount, setReviewCount] = useState(0);
  const [databaseRefreshKey, setDatabaseRefreshKey] = useState(0);
  const [overlayChrome, setOverlayChrome] = useState(() => usesMacOverlayTitlebar());

  useEffect(() => {
    const overlay = usesMacOverlayTitlebar();
    setOverlayChrome(overlay);
    // Fallback for the value set in main.tsx before first paint; assignment is
    // idempotent so React's double-invoked StrictMode effect causes no flicker.
    document.documentElement.dataset.windowChrome = resolveWindowChrome();
  }, []);

  const updateSummary = useCallback(
    (contacts: number, blacklist: number, nextReviewCount: number) => {
      setDatabaseTotal(contacts);
      setBlacklistCount(blacklist);
      setReviewCount(nextReviewCount);
    },
    [],
  );
  const updateBlacklistCount = useCallback((value: number) => setBlacklistCount(value), []);
  const updateReviewCount = useCallback((value: number) => setReviewCount(value), []);
  const reloadContacts = useCallback(() => setDatabaseRefreshKey((value) => value + 1), []);
  const updateExport = useCallback((value: ExportReport) => {
    toast.success(`Esportazione completata: ${value.rows_written.toLocaleString("it-IT")} righe`, {
      description: `File salvato in ${value.path}.`,
    });
  }, []);
  const handleDatabaseRestored = useCallback(async () => {
    setDatabaseRefreshKey((value) => value + 1);
    try {
      const data = await refreshDashboard(emptyFilters());
      updateSummary(data.database_total, data.blacklist_count, data.pending_review_count);
    } catch {}
  }, [updateSummary]);
  const activatePage = useCallback((page: Page) => {
    if (page !== "contacts") {
      setLoadedPages((previous) => {
        if (previous.has(page)) return previous;
        const next = new Set(previous);
        next.add(page);
        return next;
      });
    }
    setActivePage(page);
  }, []);

  return (
    <TooltipProvider>
      <Toaster />
      <ConfirmDialogHost />
      <SidebarProvider>
        <WindowChrome overlay={overlayChrome} />
        <Sidebar role="navigation" aria-label="Menu principale" collapsible="offcanvas">
          <SidebarHeader>
            <SidebarBrand overlay={overlayChrome} />
          </SidebarHeader>
          <SidebarContent>
            <SidebarGroup>
              <SidebarMenu aria-label="Navigazione principale">
                <SidebarMenuItem>
                  <SidebarMenuButton
                    isActive={activePage === "contacts"}
                    onClick={() => activatePage("contacts")}
                    title="Contatti"
                    tooltip="Contatti"
                    aria-current={activePage === "contacts" ? "page" : undefined}
                  >
                    <Icon name="contacts" />
                    <span className="sidebar-copy group-data-[collapsible=icon]:hidden">
                      Contatti
                    </span>
                  </SidebarMenuButton>
                </SidebarMenuItem>
                <SidebarMenuItem>
                  <SidebarMenuButton
                    isActive={activePage === "ai"}
                    onClick={() => activatePage("ai")}
                    title="Classificazione IA"
                    tooltip="Classificazione IA"
                    aria-current={activePage === "ai" ? "page" : undefined}
                  >
                    <Icon name="ai" />
                    <span className="sidebar-copy group-data-[collapsible=icon]:hidden">
                      Classificazione IA
                    </span>
                  </SidebarMenuButton>
                  {reviewCount > 0 && <SidebarMenuBadge>{reviewCount}</SidebarMenuBadge>}
                </SidebarMenuItem>
                <SidebarMenuItem>
                  <SidebarMenuButton
                    isActive={activePage === "search"}
                    onClick={() => activatePage("search")}
                    title="Cerca lead"
                    tooltip="Cerca lead"
                    aria-current={activePage === "search" ? "page" : undefined}
                  >
                    <Icon name="search" />
                    <span className="sidebar-copy group-data-[collapsible=icon]:hidden">
                      Cerca lead
                    </span>
                  </SidebarMenuButton>
                </SidebarMenuItem>
                <SidebarMenuItem>
                  <SidebarMenuButton
                    isActive={activePage === "blacklist"}
                    onClick={() => activatePage("blacklist")}
                    title="Blacklist"
                    tooltip="Blacklist"
                    aria-current={activePage === "blacklist" ? "page" : undefined}
                  >
                    <Icon name="blacklist" />
                    <span className="sidebar-copy group-data-[collapsible=icon]:hidden">
                      Blacklist
                    </span>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              </SidebarMenu>
            </SidebarGroup>
            <SidebarGroup>
              <SidebarGroupLabel>Generale</SidebarGroupLabel>
              <SidebarMenu aria-label="Impostazioni generali">
                <SidebarMenuItem>
                  <SidebarMenuButton
                    isActive={activePage === "settings"}
                    onClick={() => activatePage("settings")}
                    title="Impostazioni"
                    tooltip="Impostazioni"
                    aria-current={activePage === "settings" ? "page" : undefined}
                  >
                    <Icon name="settings" />
                    <span className="sidebar-copy group-data-[collapsible=icon]:hidden">
                      Impostazioni
                    </span>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              </SidebarMenu>
            </SidebarGroup>
          </SidebarContent>
          <SidebarFooter>
            <SidebarStatus contacts={databaseTotal} blacklist={blacklistCount} />
          </SidebarFooter>
        </Sidebar>
        <SidebarInset>
          <InsetChrome />
          <div className="workspace">
            <ContactsPage
              active={activePage === "contacts"}
              refreshKey={databaseRefreshKey}
              busy={busy}
              setBusy={setBusy}
              onSummary={updateSummary}
              onExport={updateExport}
            />
            {loadedPages.has("ai") && (
              <Suspense
                fallback={
                  activePage === "ai" ? <PageLoading name="della classificazione IA" /> : null
                }
              >
                <AiPage
                  active={activePage === "ai"}
                  onContactsChanged={reloadContacts}
                  onReviewCount={updateReviewCount}
                  sharedDatabaseTotal={databaseTotal}
                />
              </Suspense>
            )}
            {loadedPages.has("blacklist") && (
              <Suspense
                fallback={
                  activePage === "blacklist" ? <PageLoading name="della blacklist" /> : null
                }
              >
                <BlacklistPage
                  active={activePage === "blacklist"}
                  busy={busy}
                  setBusy={setBusy}
                  count={blacklistCount}
                  onCount={updateBlacklistCount}
                  onContactsChanged={reloadContacts}
                  onExport={updateExport}
                />
              </Suspense>
            )}
            {loadedPages.has("search") && (
              <Suspense
                fallback={
                  activePage === "search" ? <PageLoading name="della ricerca lead" /> : null
                }
              >
                <SearchPage active={activePage === "search"} onContactsChanged={reloadContacts} />
              </Suspense>
            )}
            {loadedPages.has("settings") && (
              <Suspense
                fallback={
                  activePage === "settings" ? <PageLoading name="delle impostazioni" /> : null
                }
              >
                <SettingsPage
                  active={activePage === "settings"}
                  busy={busy}
                  setBusy={setBusy}
                  databaseTotal={databaseTotal}
                  blacklistCount={blacklistCount}
                  onSummary={updateSummary}
                  onDatabaseCleared={reloadContacts}
                  onDatabaseRestored={handleDatabaseRestored}
                  onBlacklistCount={updateBlacklistCount}
                />
              </Suspense>
            )}
          </div>
        </SidebarInset>
      </SidebarProvider>
    </TooltipProvider>
  );
}

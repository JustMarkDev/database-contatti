import { StrictMode } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import App from "./App";

const invoke = vi.fn();
const { openFiles } = vi.hoisted(() => ({ openFiles: vi.fn() }));
let progressListener: ((event: { payload: unknown }) => void) | undefined;

vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((_event: string, listener: (event: { payload: unknown }) => void) => {
    progressListener = listener;
    return Promise.resolve(() => {});
  }),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openFiles, save: vi.fn() }));

const contact = {
  id: "contact-1",
  email: "info@example.it",
  source_file: "test.csv",
  category: "Scuole",
  subcategory: "Primaria",
  contact_kind: "istituzionale",
  paese: "Italia",
  comune: "Torino",
  provincia: "Torino",
  regione: "Piemonte",
  macroarea: "Nord-Ovest",
  website: null,
  source_url: null,
  classification_method: "file",
  classification_confidence: 0.9,
  needs_review: false,
  taxonomy_version_id: 1,
  taxonomy_version: "taxonomy-v1",
  classification_state: "classified",
  classification_state_reason: null,
  quality_score: 72,
};
const review = {
  id: "review-1",
  contact_id: "contact-1",
  email: "review@example.it",
  current_category: "Varie",
  current_subcategory: null,
  proposed_category: "Scuole",
  proposed_subcategory: "Primaria",
  confidence: 0.7,
  evidence: ["dominio"],
  model: "test",
  profile: "laptop",
  status: "pending",
};
const dashboard = {
  filtered: { contacts: [contact], total: 1 },
  database_total: 1,
  filters: {
    source_file: [],
    category: [],
    subcategory: [],
    contact_kind: [],
    paese: [],
    macroarea: [],
    regione: [],
    provincia: [],
    comune: [],
    pec: [],
    classification_method: [],
    classification_confidence: [],
  },
  blacklist_count: 0,
  pending_review_count: 1,
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function contactListResult(label: string, count: number) {
  return {
    contacts: [{ ...contact, id: label, email: `${label}@example.it`, category: label }],
    total: count,
    limit: 100,
    offset: 0,
    has_previous: false,
    has_next: false,
  };
}

function dashboardResult(label: string, count: number) {
  return {
    ...dashboard,
    filtered: {
      contacts: [{ ...contact, id: label, email: `${label}@example.it`, category: label }],
      total: count,
    },
    database_total: count,
    filters: {
      ...dashboard.filters,
      source_file: [{ value: `${label}-facet`, count }],
      category: [{ value: `${label}-facet`, count }],
    },
    blacklist_count: count,
    pending_review_count: count,
  };
}

const blacklistResult = (label: string, total: number) => ({
  entries: [
    {
      email: `${label}@blocked.it`,
      source_file: `${label}.txt`,
      imported_at: "2026-01-01T00:00:00Z",
    },
  ],
  total,
});

function defaultInvoke(command: string) {
  if (command === "refresh_dashboard") return Promise.resolve(dashboard);
  if (command === "get_filter_options") return Promise.resolve(dashboard.filters);
  if (command === "list_classification_reviews")
    return Promise.resolve({ reviews: [review], total: 1 });
  if (command === "list_contacts")
    return Promise.resolve({
      contacts: [contact],
      total: 1,
      limit: 100,
      offset: 0,
      has_previous: false,
      has_next: false,
    });
  if (
    ["list_search_runs", "list_seed_lists", "list_lead_sources", "list_lead_taxonomy"].includes(
      command,
    )
  )
    return Promise.resolve([]);
  if (command === "get_lead_filter_options")
    return Promise.resolve({
      run: [],
      category: [],
      subcategory: [],
      regione: [],
      provincia: [],
      method: [],
      pec: [],
      stato: [],
    });
  if (command === "search_leads" || command === "list_search_leads")
    return Promise.resolve({
      leads: [],
      total: 0,
      limit: 100,
      offset: 0,
      has_previous: false,
      has_next: false,
    });
  if (command === "detect_opencode")
    return Promise.resolve({
      available: true,
      installed: true,
      authenticated: true,
      models: [
        {
          id: "opencode-go/muse-spark-1.3-contributor",
          variants: ["low", "high", "max"],
        },
      ],
      selected_model: "opencode-go/muse-spark-1.3-contributor",
      selected_effort: "high",
      version: "2.0.18",
      error: null,
    });
  if (command === "detect_typesafe")
    return Promise.resolve({
      available: true,
      configured: true,
      model: "jev-latest",
      error: null,
    });
  if (command === "get_typesafe_api_key")
    return Promise.resolve({
      configured: true,
      from_env: true,
    });
  if (command === "get_contextdev_settings")
    return Promise.resolve({ concurrency: 2, maps_enabled: false });
  if (command === "list_deterministic_catalogs") return Promise.resolve([]);
  if (command === "list_ai_run_failures") return Promise.resolve([]);
  if (command === "list_ai_runs") return Promise.resolve([]);
  if (command === "get_resumable_ai_run") return Promise.resolve(null);
  if (command === "list_ai_field_changes") return Promise.resolve({ changes: [], total: 0 });
  if (command === "list_candidates")
    return Promise.resolve({
      candidates: [],
      total: 0,
      limit: 50,
      offset: 0,
    });
  return Promise.resolve({});
}

describe("App orchestration", () => {
  it("places mailbox verification directly below contacts in the sidebar", async () => {
    render(<App />);
    await screen.findByText("info@example.it");

    const navigation = screen.getByRole("list", { name: "Navigazione principale" });
    const sidebarItems = within(navigation)
      .getAllByRole("button")
      .map((button) => button.getAttribute("title"));

    expect(sidebarItems.slice(0, 2)).toEqual(["Contatti", "Verifica caselle"]);
    expect(sidebarItems).not.toContain("Candidati");
    expect(sidebarItems).not.toContain("Impostazioni");

    const general = screen.getByRole("list", { name: "Impostazioni generali" });
    expect(within(general).getByTitle("Impostazioni")).toBeInTheDocument();
  });

  it("places lead search above the blacklist and renders it on activation", async () => {
    render(<App />);
    await screen.findByText("info@example.it");

    const navigation = screen.getByRole("list", { name: "Navigazione principale" });
    const titles = within(navigation)
      .getAllByRole("button")
      .map((button) => button.getAttribute("title"));
    expect(titles.slice(0, 2)).toEqual(["Contatti", "Verifica caselle"]);
    expect(titles.indexOf("Cerca lead")).toBeLessThan(titles.indexOf("Blacklist"));

    await userEvent.click(screen.getByTitle("Cerca lead"));
    const island = await screen.findByRole("region", { name: "Cerca lead" });
    expect(island).toHaveClass("content-island");
    expect(
      within(island).getByRole("heading", { level: 1, name: "Cerca lead" }),
    ).toBeInTheDocument();
  });

  it("keeps mailbox verification behind identity preflight and confirms the full verification", async () => {
    let preflightReady = false;
    const verificationOverview = () => ({
      eligible_contacts: 123,
      eligible_domains: 8,
      eligible_mx_hosts: 4,
      blacklisted_contacts: 3,
      suppressed_contacts: 2,
      non_contact_mailboxes: 1,
      non_routable_contacts: 6,
      temporary_dns_failures: 0,
      preflight_ok: preflightReady,
      preflight_message: preflightReady ? null : "Configura l'identità SMTP",
      settings: preflightReady
        ? {
            ehlo_hostname: "verifier.example.it",
            envelope_sender: "verifier@example.it",
            starttls_required: true,
            global_rate_per_minute: 5,
            max_concurrency: 1,
          }
        : null,
      status: null,
      last_completed_at: null,
    });
    invoke.mockImplementation((command: string) => {
      if (command === "get_mailbox_verification_overview")
        return Promise.resolve(verificationOverview());
      if (command === "get_mailbox_verification_status") return Promise.resolve(null);
      if (command === "save_mailbox_verification_settings") {
        preflightReady = true;
        return Promise.resolve();
      }
      if (command === "revalidate_mailbox_dns")
        return Promise.resolve({
          domains_examined: 8,
          domains_revalidated: 3,
          routing_ok: 2,
          no_routing: 0,
          temporary_failures: 1,
          promoted_contacts: 1,
          separated_contacts: 0,
        });
      if (command === "start_mailbox_verification")
        return Promise.resolve({ state: "running", mode: "everything", processed: 0, total: 123 });
      return defaultInvoke(command);
    });

    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Verifica caselle"));
    expect(await screen.findByRole("heading", { name: "Verifica caselle" })).toBeInTheDocument();
    const island = screen.getByRole("region", { name: "Verifica caselle" });
    expect(island.closest(".workspace")).not.toBeNull();
    expect(
      within(island).getByRole("heading", {
        name: "Recupero DNS (Indirizzi con errore temporaneo)",
      }),
    ).toBeInTheDocument();
    expect(within(island).getByRole("heading", { name: "Identità SMTP" })).toBeInTheDocument();
    expect(within(island).getByRole("heading", { name: "Avvia verifica" })).toBeInTheDocument();
    expect(
      within(island).getByRole("heading", { name: "Attività precedente" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Da configurare")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Avvia verifica graduale" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Verifica completa" })).toBeDisabled();

    await userEvent.type(screen.getByLabelText("Hostname EHLO"), "verifier.example.it");
    await userEvent.type(screen.getByLabelText("Mittente envelope"), "verifier@example.it");
    await userEvent.click(screen.getByRole("button", { name: "Salva identità" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("save_mailbox_verification_settings", {
        settings: expect.objectContaining({
          ehlo_hostname: "verifier.example.it",
          envelope_sender: "verifier@example.it",
          starttls_required: true,
          max_concurrency: 1,
        }),
      }),
    );
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Avvia verifica graduale" })).toBeEnabled(),
    );
    await userEvent.click(screen.getByRole("button", { name: "Rivalida DNS" }));
    expect(await screen.findByText(/Ultimo DNS:/)).toHaveTextContent("3 domini");
    await userEvent.click(screen.getByRole("button", { name: "Verifica completa" }));
    expect(await screen.findByRole("alertdialog")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Conferma verifica completa" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Conferma verifica completa" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("start_mailbox_verification", { mode: "everything" }),
    );
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  beforeEach(() => {
    localStorage.clear();
    progressListener = undefined;
    invoke.mockReset();
    openFiles.mockReset();
    invoke.mockImplementation(defaultInvoke);
  });

  it("renders dashboard results returned by the backend", async () => {
    render(<App />);
    expect(await screen.findByText("info@example.it")).toBeInTheDocument();
    expect(screen.getByText("Primaria")).toBeInTheDocument();
    expect(screen.getByText("1 risultati")).toBeInTheDocument();
  });

  it("hides the sidebar completely when collapsed", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Comprimi barra laterale"));
    expect(screen.getByTitle("Espandi barra laterale")).toBeInTheDocument();
    const shell = document.querySelector(
      '[data-slot="sidebar"][data-state="collapsed"][data-collapsible="offcanvas"]',
    );
    expect(shell).toBeTruthy();
  });

  it("keeps the sidebar toggle in the fixed chrome layer outside the sidebar", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    const toggle = screen.getByTitle("Comprimi barra laterale");
    const chrome = toggle.closest(".window-chrome-layer");
    expect(chrome?.parentElement).toBe(document.body);
    expect(toggle.closest('[data-slot="sidebar"]')).toBeNull();
    expect(toggle.closest(".workspace")).toBeNull();
  });

  it("renders the contacts page title, filters, and results in a single content island", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    expect(invoke).toHaveBeenCalledWith("refresh_dashboard", {
      filters: expect.any(Object),
      limit: 100,
      offset: 0,
      includeFacets: false,
    });
    const islands = document.querySelectorAll(".content-island");
    expect(islands).toHaveLength(1);
    expect(
      within(islands[0] as HTMLElement).getByRole("heading", { level: 1, name: "Contatti" }),
    ).toBeInTheDocument();
    expect(islands[0].querySelector(".advanced-filters-grid")).not.toBeNull();
    expect(islands[0].querySelector(".contacts-table")).not.toBeNull();
    expect(
      within(islands[0] as HTMLElement).getByRole("button", { name: "Importa contatti" }),
    ).toBeInTheDocument();
  });

  it("uses a content island for every main page", async () => {
    render(<App />);
    await screen.findByText("info@example.it");

    for (const [title, heading] of [
      ["Classificazione IA", "Classificazione IA"],
      ["Verifica caselle", "Verifica caselle"],
      ["Blacklist", "Blacklist"],
      ["Impostazioni", "Impostazioni"],
      ["Contatti", "Contatti"],
    ] as const) {
      await userEvent.click(screen.getByTitle(title));
      const island = await screen.findByRole("region", {
        name: title === "Classificazione IA" ? "Classificazione IA" : heading,
      });
      expect(island).toHaveClass("content-island");
      expect(within(island).getByRole("heading", { level: 1, name: heading })).toBeInTheDocument();
    }
  });

  it("opens the mobile sidebar and closes it after navigation", async () => {
    const previousWidth = window.innerWidth;
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 480 });
    try {
      render(<App />);
      await screen.findByText("info@example.it");
      await waitFor(() =>
        expect(
          screen.queryByRole("list", { name: "Navigazione principale" }),
        ).not.toBeInTheDocument(),
      );

      await userEvent.click(screen.getByRole("button", { name: "Apri menu" }));
      const navigation = await screen.findByRole("list", { name: "Navigazione principale" });
      await userEvent.click(within(navigation).getByTitle("Classificazione IA"));

      await waitFor(() =>
        expect(
          screen.queryByRole("list", { name: "Navigazione principale" }),
        ).not.toBeInTheDocument(),
      );
      expect(
        await screen.findByRole("heading", { name: "Classificazione IA" }),
      ).toBeInTheDocument();
    } finally {
      Object.defineProperty(window, "innerWidth", { configurable: true, value: previousWidth });
    }
  });

  it("opens one contact import menu for both files and folders", async () => {
    openFiles.mockResolvedValue(null);
    render(<App />);
    await screen.findByText("info@example.it");

    await userEvent.click(screen.getByRole("button", { name: "Importa contatti" }));
    await userEvent.click(await screen.findByRole("menuitem", { name: "Seleziona cartelle" }));
    expect(openFiles).toHaveBeenCalledWith({ directory: true, multiple: true });

    await userEvent.click(screen.getByRole("button", { name: "Importa contatti" }));
    await userEvent.click(await screen.findByRole("menuitem", { name: "Seleziona file" }));
    expect(openFiles).toHaveBeenCalledWith(
      expect.objectContaining({ multiple: true, filters: expect.any(Array) }),
    );
  });

  it("shows concise explanations for both contact exports", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.hover(screen.getByRole("button", { name: "Esporta email" }));
    expect(
      await screen.findByText("Esporta solo gli indirizzi email filtrati."),
    ).toBeInTheDocument();
    await userEvent.unhover(screen.getByRole("button", { name: "Esporta email" }));
    await userEvent.hover(screen.getByRole("button", { name: "Esporta tutto" }));
    expect(
      await screen.findByText("Esporta tutti i dati dei contatti filtrati."),
    ).toBeInTheDocument();
  });

  it("uses the streamlined blacklist heading and explains both exports", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "refresh_dashboard")
        return Promise.resolve({ ...dashboard, blacklist_count: 1 });
      if (command === "list_blacklist") return Promise.resolve(blacklistResult("export", 1));
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Blacklist"));
    expect(await screen.findByRole("heading", { name: "Indirizzi esclusi" })).toBeInTheDocument();
    expect(
      screen.queryByText(
        "Le email presenti in questo elenco vengono escluse automaticamente dalle importazioni.",
      ),
    ).not.toBeInTheDocument();

    await userEvent.hover(screen.getByRole("button", { name: "Esporta email" }));
    expect(
      await screen.findByText("Esporta solo gli indirizzi email filtrati della blacklist."),
    ).toBeInTheDocument();
    await userEvent.unhover(screen.getByRole("button", { name: "Esporta email" }));
    await userEvent.hover(screen.getByRole("button", { name: "Esporta tutto" }));
    expect(
      await screen.findByText("Esporta tutti i dati filtrati della blacklist."),
    ).toBeInTheDocument();
  });

  it("keeps destructive actions collapsed in Settings", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    expect(screen.queryByRole("button", { name: "Ripulisci database" })).not.toBeInTheDocument();
    await userEvent.click(screen.getByTitle("Impostazioni"));
    const summary = await screen.findByText("Azioni distruttive");
    expect(summary.closest("details")).not.toHaveAttribute("open");
    await userEvent.click(summary);
    expect(screen.getByRole("button", { name: "Ripulisci database" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Ripulisci blacklist" })).toBeDisabled();
  });

  it("shows a single cascade enrichment control", async () => {
    render(<App />);
    fireEvent.click(screen.getByTitle("Classificazione IA"));
    expect(await screen.findByRole("heading", { name: "Classificazione IA" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Arricchisci database" })).toBeInTheDocument();
    expect(screen.queryByRole("radiogroup", { name: "Provider IA" })).not.toBeInTheDocument();
  });

  it("enables enrichment when Jev and OpenCode are available", async () => {
    render(<App />);
    fireEvent.click(screen.getByTitle("Classificazione IA"));
    expect(await screen.findByRole("button", { name: "Arricchisci database" })).toBeEnabled();
  });

  it("shows a loading state instead of a stale key prompt while checking availability", async () => {
    render(<App />);
    fireEvent.click(screen.getByTitle("Classificazione IA"));
    expect(await screen.findByRole("heading", { name: "Classificazione IA" })).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByText("Verifica disponibilità…")).not.toBeInTheDocument(),
    );
    expect(screen.queryByLabelText("Chiave API TypeSafe")).not.toBeInTheDocument();
    expect(screen.queryByText(/non pronto/)).not.toBeInTheDocument();
  });

  it("checks TypeSafe availability every five seconds while the IA page is active", async () => {
    vi.useFakeTimers();
    render(<App />);
    await act(async () => {});
    expect(invoke.mock.calls.filter(([command]) => command === "detect_typesafe")).toHaveLength(0);
    fireEvent.click(screen.getByTitle("Classificazione IA"));
    await act(async () => {});
    expect(invoke.mock.calls.filter(([command]) => command === "detect_typesafe")).toHaveLength(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(invoke.mock.calls.filter(([command]) => command === "detect_typesafe")).toHaveLength(2);
  });

  it("completes contact and blacklist loading under Strict Mode", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "refresh_dashboard")
        return Promise.resolve({
          ...dashboard,
          filtered: { contacts: [contact], total: 101 },
          database_total: 101,
          blacklist_count: 101,
        });
      if (command === "list_blacklist") return Promise.resolve(blacklistResult("strict", 101));
      return defaultInvoke(command);
    });

    render(
      <StrictMode>
        <App />
      </StrictMode>,
    );
    const next = await screen.findByRole("button", { name: "Successiva" });
    await waitFor(() => expect(next).toBeEnabled());

    await userEvent.click(screen.getByTitle("Blacklist"));
    expect(await screen.findByText("strict@blocked.it")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "Successiva" })).toBeEnabled());
    expect(screen.queryByText(/Caricamento (contatti|blacklist)/)).not.toBeInTheDocument();
  });

  it("explains that only valid unique addresses are added to the blacklist", async () => {
    openFiles.mockResolvedValue("C:\\imports\\blacklist.txt");
    invoke.mockImplementation((command: string) => {
      if (command === "list_blacklist") return Promise.resolve(blacklistResult("imported", 9_955));
      if (command === "import_blacklist_paths") {
        return Promise.resolve({
          inserted: 9_955,
          duplicates: 2_775,
          invalid: 8,
          removed_contacts: 0,
          failed_files: [],
        });
      }
      return defaultInvoke(command);
    });

    render(<App />);
    await userEvent.click(screen.getByTitle("Blacklist"));
    await screen.findByText("imported@blocked.it");
    await userEvent.click(screen.getByRole("button", { name: "Importa blacklist" }));

    expect(
      await screen.findByText(
        /Importazione completata: 9[. ]?955 indirizzi validi e unici aggiunti alla blacklist\./,
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/2[. ]?775 duplicati e 8 indirizzi non validi ignorati\./),
    ).toBeInTheDocument();
    expect(screen.queryByText(/contatti rimossi/i)).not.toBeInTheDocument();
  });

  it("adds a single email to the blacklist from the textbox", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "add_blacklist_entry") {
        return Promise.resolve(true);
      }
      if (command === "list_blacklist") {
        return Promise.resolve(blacklistResult("manual", 1));
      }
      return defaultInvoke(command);
    });
    render(<App />);
    await userEvent.click(screen.getByTitle("Blacklist"));
    await screen.findByPlaceholderText("Aggiungi un'email alla blacklist (es. info@esempio.it)…");
    const input = screen.getByPlaceholderText(
      "Aggiungi un'email alla blacklist (es. info@esempio.it)…",
    );
    await userEvent.type(input, "manual@example.test");
    await userEvent.click(screen.getByRole("button", { name: "Aggiungi" }));
    expect(invoke).toHaveBeenCalledWith("add_blacklist_entry", {
      email: "manual@example.test",
      note: undefined,
    });
    expect(await screen.findByText("Indirizzo aggiunto alla blacklist")).toBeInTheDocument();
  });

  it("disables conflicting actions and renders AI progress while a run is active", async () => {
    let started = false;
    invoke.mockImplementation((command: string) => {
      if (command === "start_ai_classification") {
        started = true;
        return Promise.resolve({
          processed: 0,
          updated: 0,
          skipped: 0,
          errors: [],
          misclassified: [],
        });
      }
      if (command === "list_ai_runs" && started) {
        return Promise.resolve([
          {
            id: "run-1",
            status: "running",
            model: "test",
            profile: "laptop",
            total: 4,
            processed: 0,
            updated: 0,
            reviews_created: 0,
            skipped: 0,
            errors: 0,
            interruption_reason: null,
          },
        ]);
      }
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Classificazione IA"));
    const start = screen.getByRole("button", { name: "Arricchisci database" });
    await userEvent.click(start);
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "start_ai_classification",
        expect.objectContaining({
          request: expect.objectContaining({
            provider: "cascade",
            mode: "full",
            escalationProvider: "opencode",
            escalationModel: "opencode-go/muse-spark-1.3-contributor",
            escalationEffort: "xhigh",
          }),
        }),
      ),
    );
    expect(await screen.findByRole("button", { name: "Ferma" })).toBeEnabled();
    fireEvent.click(screen.getByTitle("Contatti"));
    expect(screen.getByRole("button", { name: /Importa contatti/ })).toBeEnabled();
    progressListener?.({
      payload: {
        phase: "running",
        processed: 2,
        total: 4,
        updated: 1,
        skipped: 0,
        error: null,
        message: "in corso",
      },
    });
    fireEvent.click(screen.getByTitle("Classificazione IA"));
    expect(await screen.findByText(/50% · 2\/4/)).toBeInTheDocument();
    expect(screen.getByRole("progressbar")).toBeInTheDocument();
    expect(
      within(screen.getByLabelText("Riepilogo esecuzione")).getByText("in corso"),
    ).toBeInTheDocument();
  });

  it("shows the full pending review total when only the oldest page is loaded", async () => {
    invoke.mockImplementation((command: string) =>
      command === "list_classification_reviews"
        ? Promise.resolve({ reviews: [review], total: 250 })
        : defaultInvoke(command),
    );
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Classificazione IA"));
    expect(
      await screen.findByText(/250 proposte in attesa di approvazione; mostrate le 1 più vecchie/),
    ).toBeInTheDocument();
  });

  it("routes review approval and rejection to their commands", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Classificazione IA"));
    await userEvent.click(screen.getByRole("button", { name: "Approva" }));
    expect(invoke).toHaveBeenCalledWith("approve_classification_review", {
      reviewId: "review-1",
      category: null,
      subcategory: null,
    });
    await userEvent.click(screen.getByRole("button", { name: "Rifiuta" }));
    expect(invoke).toHaveBeenCalledWith("reject_classification_review", { reviewId: "review-1" });
  });

  it("renders resumable lifecycle controls", async () => {
    const pausedRun = {
      id: "run-1",
      status: "paused",
      model: "test",
      profile: "laptop",
      total: 3,
      processed: 1,
      updated: 1,
      reviews_created: 1,
      skipped: 0,
      errors: 0,
      interruption_reason: "Esecuzione interrotta",
    };
    invoke.mockImplementation((command: string) => {
      if (command === "get_resumable_ai_run") return Promise.resolve(pausedRun);
      if (command === "list_ai_runs") return Promise.resolve([pausedRun]);
      if (command === "list_ai_field_changes")
        return Promise.resolve({
          total: 1,
          changes: [
            {
              id: "h1",
              run_id: "run-1",
              contact_id: "c1",
              review_id: null,
              email: "a@example.it",
              field_name: "comune",
              old_value: null,
              new_value: "Torino",
              change_kind: "aggiunto",
              status: "applied",
              confidence: 0.9,
              evidence: ["via:jev", "dominio"],
              created_at: "2026-01-01T10:00:00Z",
              resolved_at: null,
            },
          ],
        });
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Classificazione IA"));
    expect(await screen.findByRole("button", { name: "Riprendi" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Annulla" })).toBeEnabled();
    expect(screen.getByText("Esecuzione interrotta")).toBeInTheDocument();
    expect(
      await screen.findByRole("heading", { name: "Attività in tempo reale" }),
    ).toBeInTheDocument();
    expect(
      within(screen.getByLabelText("Riepilogo esecuzione")).getByText("in pausa"),
    ).toBeInTheDocument();
    expect(screen.queryByText(/cascade\/|test · paused/)).not.toBeInTheDocument();
    expect(await screen.findByText(/a@example\.it · comune → Torino · Jev/)).toBeInTheDocument();
  });

  it("shows pausing as a distinct non-interactive state", async () => {
    const pausingRun = {
      id: "run-1",
      status: "pausing",
      model: "test",
      profile: "laptop",
      total: 2,
      processed: 1,
      updated: 0,
      reviews_created: 0,
      skipped: 1,
      errors: 0,
      interruption_reason: null,
    };
    invoke.mockImplementation((command: string) =>
      command === "list_ai_runs" ? Promise.resolve([pausingRun]) : defaultInvoke(command),
    );
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Classificazione IA"));
    expect(await screen.findByRole("button", { name: "Arresto in corso…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Annulla" })).toBeEnabled();
  });

  it("automatically resumes a run paused by application shutdown", async () => {
    const shutdownRun = {
      id: "run-shutdown",
      status: "paused",
      model: "test",
      profile: "laptop",
      total: 4,
      processed: 2,
      updated: 1,
      reviews_created: 0,
      skipped: 1,
      errors: 0,
      interruption_reason: "Pausa automatica alla chiusura dell'app",
    };
    invoke.mockImplementation((command: string) => {
      if (command === "get_resumable_ai_run") return Promise.resolve(shutdownRun);
      if (command === "resume_ai_classification")
        return Promise.resolve({
          processed: 2,
          updated: 1,
          skipped: 1,
          errors: [],
          misclassified: [],
        });
      if (command === "list_ai_runs") return Promise.resolve([shutdownRun]);
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    expect(invoke).not.toHaveBeenCalledWith("resume_ai_classification", expect.anything());
    await userEvent.click(screen.getByTitle("Classificazione IA"));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("resume_ai_classification", {
        runId: "run-shutdown",
      }),
    );
  });

  it("navigates contact pages without refreshing dashboard facets", async () => {
    const pageContacts = Array.from({ length: 100 }, (_, index) => ({
      ...contact,
      id: `page-${index}`,
      email: `page-${index}@example.it`,
    }));
    invoke.mockImplementation((command: string, args?: { offset?: number }) => {
      if (command === "refresh_dashboard") {
        return Promise.resolve({
          ...dashboard,
          filtered: { contacts: pageContacts, total: 150 },
          database_total: 150,
        });
      }
      if (command === "list_contacts") {
        return Promise.resolve(
          args?.offset === 100
            ? {
                contacts: [{ ...contact, id: "last", email: "last-page@example.it" }],
                total: 150,
                limit: 100,
                offset: 100,
                has_previous: true,
                has_next: false,
              }
            : {
                contacts: pageContacts,
                total: 150,
                limit: 100,
                offset: 0,
                has_previous: false,
                has_next: true,
              },
        );
      }
      return defaultInvoke(command);
    });
    render(<App />);
    const next = await screen.findByRole("button", { name: "Successiva" });
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("list_contacts", {
        filters: expect.any(Object),
        limit: 100,
        offset: 100,
        includeTotal: false,
      }),
    );
    expect(await screen.findByText("last-page@example.it")).toBeInTheDocument();
    expect(invoke.mock.calls.filter(([command]) => command === "refresh_dashboard")).toHaveLength(
      1,
    );

    await userEvent.click(screen.getByRole("button", { name: "Precedente" }));
    expect(await screen.findByText("page-0@example.it")).toBeInTheDocument();
    expect(screen.queryByText("last-page@example.it")).not.toBeInTheDocument();
    expect(screen.getByText("1–100 di 150")).toBeInTheDocument();
    expect(invoke.mock.calls.filter(([command]) => command === "refresh_dashboard")).toHaveLength(
      1,
    );
  });

  it("loads deferred pages on first activation and preserves their state", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    expect(screen.queryByRole("heading", { name: "Verifica caselle" })).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Classificazione IA" })).not.toBeInTheDocument();
    expect(invoke.mock.calls.filter(([command]) => command === "detect_typesafe")).toHaveLength(0);

    await userEvent.click(screen.getByTitle("Verifica caselle"));
    expect(await screen.findByRole("heading", { name: "Verifica caselle" })).toBeInTheDocument();
    const search = screen.getByRole("textbox", { name: "Cerca candidati recuperabili" });
    await userEvent.type(search, "archived@example.it");

    await userEvent.click(screen.getByTitle("Contatti"));
    expect(screen.queryByRole("heading", { name: "Verifica caselle" })).not.toBeInTheDocument();
    await userEvent.click(screen.getByTitle("Verifica caselle"));
    expect(await screen.findByDisplayValue("archived@example.it")).toBeInTheDocument();
  });

  it("applies contact filters without rebuilding dashboard facets", async () => {
    invoke.mockImplementation((command: string, args?: { filters?: { query?: string } }) => {
      if (command === "list_contacts") {
        const query = args?.filters?.query ?? "";
        return Promise.resolve({
          contacts: query
            ? [{ ...contact, id: "filtered", email: "filtered@example.it" }]
            : [contact],
          total: query ? 1 : 1,
          limit: 100,
          offset: 0,
          has_previous: false,
          has_next: false,
        });
      }
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    const dashboardCalls = () =>
      invoke.mock.calls.filter(([command]) => command === "refresh_dashboard").length;
    expect(dashboardCalls()).toBe(1);

    const search = screen.getByPlaceholderText("Cerca email, file, categoria, località…");
    await userEvent.type(search, "filtered");
    await waitFor(
      () =>
        expect(invoke).toHaveBeenCalledWith("list_contacts", {
          filters: expect.objectContaining({ query: "filtered" }),
          limit: 100,
          offset: 0,
          includeTotal: true,
        }),
      { timeout: 2_000 },
    );
    expect(await screen.findByText("filtered@example.it")).toBeInTheDocument();
    expect(dashboardCalls()).toBe(1);
  });

  it("keeps the contacts snapshot when returning from another page", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    const dashboardCalls = () =>
      invoke.mock.calls.filter(([command]) => command === "refresh_dashboard").length;
    expect(dashboardCalls()).toBe(1);

    await userEvent.click(screen.getByTitle("Blacklist"));
    await screen.findByRole("heading", { name: "Blacklist" });
    await userEvent.click(screen.getByTitle("Contatti"));
    expect(await screen.findByText("info@example.it")).toBeInTheDocument();
    expect(dashboardCalls()).toBe(1);
  });

  it("does not let an old contact page replace an authoritative mutation refresh", async () => {
    const oldPage = deferred<{
      contacts: Array<typeof contact>;
      total: number;
      limit: number;
      offset: number;
      has_previous: boolean;
      has_next: boolean;
    }>();
    let refreshCalls = 0;
    invoke.mockImplementation((command: string) => {
      if (command === "refresh_dashboard") {
        refreshCalls += 1;
        return Promise.resolve(
          refreshCalls === 1
            ? dashboard
            : { ...dashboard, filtered: { contacts: [], total: 0 }, database_total: 0 },
        );
      }
      if (command === "list_contacts") return oldPage.promise;
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByRole("button", { name: "Cancella" }));
    await userEvent.click(await screen.findByRole("button", { name: "Cancella contatto" }));
    await waitFor(() => expect(refreshCalls).toBe(2));
    await act(() => {
      oldPage.resolve({
        contacts: [contact],
        total: 1,
        limit: 100,
        offset: 0,
        has_previous: false,
        has_next: false,
      });
      return oldPage.promise;
    });
    expect(screen.queryByRole("cell", { name: "info@example.it" })).not.toBeInTheDocument();
  });

  it("uses native streaming export after an explicit save choice", async () => {
    const { save } = await import("@tauri-apps/plugin-dialog");
    vi.mocked(save).mockResolvedValue("C:\\exports\\contacts.csv");
    invoke.mockImplementation((command: string) =>
      command === "export_contacts_csv"
        ? Promise.resolve({ path: "C:\\exports\\contacts.csv", rows_written: 42 })
        : defaultInvoke(command),
    );
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByRole("button", { name: "Esporta tutto" }));
    await userEvent.click(
      screen.getByRole("checkbox", { name: "Includi anche classificazioni da verificare" }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Scegli destinazione" }));
    expect(invoke).toHaveBeenCalledWith("export_contacts_csv", {
      filters: expect.any(Object),
      path: "C:\\exports\\contacts.csv",
      includeUntrustedClassifications: true,
    });
    expect(await screen.findByText(/Esportazione completata: 42 righe/)).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith(
      "list_contacts",
      expect.objectContaining({ limit: 1_000_000 }),
    );
  });

  it("exports only filtered email rows without requesting the paginated list", async () => {
    const { save } = await import("@tauri-apps/plugin-dialog");
    vi.mocked(save).mockResolvedValue("C:\\exports\\emails.csv");
    invoke.mockImplementation((command: string) =>
      command === "export_contact_emails_csv"
        ? Promise.resolve({ path: "C:\\exports\\emails.csv", rows_written: 1 })
        : defaultInvoke(command),
    );
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByRole("button", { name: "Esporta email" }));
    await userEvent.click(screen.getByRole("button", { name: "Scegli destinazione" }));
    expect(invoke).toHaveBeenCalledWith("export_contact_emails_csv", {
      filters: expect.any(Object),
      path: "C:\\exports\\emails.csv",
      includeUntrustedClassifications: false,
    });
    expect(await screen.findByText(/Esportazione completata: 1 righe/)).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith(
      "list_contacts",
      expect.objectContaining({ limit: 1_000_000 }),
    );
  });

  it("paginates blacklist, resets search offset, and ignores the stale old page", async () => {
    const oldPage = deferred<ReturnType<typeof blacklistResult>>();
    const newPage = deferred<ReturnType<typeof blacklistResult>>();
    const firstEntries = Array.from({ length: 100 }, (_, index) => ({
      email: `blocked-${index}@example.it`,
      source_file: "first.txt",
      imported_at: "2026-01-01T00:00:00Z",
    }));
    invoke.mockImplementation((command: string, args?: { query?: string; offset?: number }) => {
      if (command === "refresh_dashboard")
        return Promise.resolve({ ...dashboard, blacklist_count: 150 });
      if (command === "list_blacklist") {
        if (args?.query === "new" && args.offset === 0) return newPage.promise;
        if (!args?.query && args?.offset === 100) return oldPage.promise;
        return Promise.resolve({ entries: firstEntries, total: 150 });
      }
      return defaultInvoke(command);
    });
    render(<App />);
    fireEvent.click(screen.getByTitle("Blacklist"));
    const next = await screen.findByRole("button", { name: "Successiva" });
    await waitFor(() => expect(next).toBeEnabled());
    fireEvent.click(next);
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("list_blacklist", {
        query: "",
        limit: 100,
        offset: 100,
      }),
    );
    fireEvent.change(screen.getByPlaceholderText("Cerca email o file di origine…"), {
      target: { value: "new" },
    });
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("list_blacklist", {
        query: "new",
        limit: 100,
        offset: 0,
      }),
    );
    await act(() => {
      newPage.resolve(blacklistResult("new", 1));
      return newPage.promise;
    });
    expect(screen.getByText("new@blocked.it")).toBeInTheDocument();
    await act(() => {
      oldPage.resolve(blacklistResult("stale", 150));
      return oldPage.promise;
    });
    expect(screen.queryByText("stale@blocked.it")).not.toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("list_blacklist", { query: "new", limit: 100, offset: 0 });
  });

  it("uses native blacklist export and displays export failures", async () => {
    const { save } = await import("@tauri-apps/plugin-dialog");
    vi.mocked(save)
      .mockResolvedValueOnce("C:\\exports\\blacklist.csv")
      .mockResolvedValueOnce("C:\\exports\\contacts.csv");
    invoke.mockImplementation((command: string) => {
      if (command === "refresh_dashboard")
        return Promise.resolve({ ...dashboard, blacklist_count: 1 });
      if (command === "export_blacklist_csv")
        return Promise.resolve({ path: "C:\\exports\\blacklist.csv", rows_written: 3 });
      if (command === "export_contacts_csv")
        return Promise.reject(new Error("target already exists"));
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    fireEvent.click(screen.getByTitle("Blacklist"));
    await userEvent.click(screen.getByRole("button", { name: "Esporta tutto" }));
    expect(invoke).toHaveBeenCalledWith("export_blacklist_csv", {
      query: "",
      path: "C:\\exports\\blacklist.csv",
    });
    expect(await screen.findByText(/Esportazione completata: 3 righe/)).toBeInTheDocument();
    fireEvent.click(screen.getByTitle("Contatti"));
    await userEvent.click(screen.getByRole("button", { name: "Esporta tutto" }));
    await userEvent.click(screen.getByRole("button", { name: "Scegli destinazione" }));
    expect(await screen.findByText(/target already exists/)).toBeInTheDocument();
  });

  it("exports blacklist emails without the metadata columns", async () => {
    const { save } = await import("@tauri-apps/plugin-dialog");
    vi.mocked(save).mockResolvedValue("C:\\exports\\blacklist-emails.csv");
    invoke.mockImplementation((command: string) => {
      if (command === "refresh_dashboard")
        return Promise.resolve({ ...dashboard, blacklist_count: 1 });
      if (command === "list_blacklist") return Promise.resolve(blacklistResult("export", 1));
      if (command === "export_blacklist_emails_csv")
        return Promise.resolve({ path: "C:\\exports\\blacklist-emails.csv", rows_written: 1 });
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Blacklist"));
    await screen.findByText("export@blocked.it");
    await userEvent.click(screen.getByRole("button", { name: "Esporta email" }));
    expect(invoke).toHaveBeenCalledWith("export_blacklist_emails_csv", {
      query: "",
      path: "C:\\exports\\blacklist-emails.csv",
    });
    expect(await screen.findByText(/Esportazione completata: 1 righe/)).toBeInTheDocument();
  });

  it("gates database clearing behind confirmation", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Impostazioni"));
    await userEvent.click(screen.getByText("Azioni distruttive"));
    const clear = screen.getByRole("button", { name: "Ripulisci database" });
    await userEvent.click(clear);
    await userEvent.click(await screen.findByRole("button", { name: "Annulla" }));
    expect(invoke).not.toHaveBeenCalledWith("clear_database");
    await userEvent.click(clear);
    await userEvent.click(await screen.findByRole("button", { name: "Continua" }));
    expect(invoke).not.toHaveBeenCalledWith("clear_database");
    await userEvent.click(await screen.findByRole("button", { name: "Elimina definitivamente" }));
    expect(invoke).toHaveBeenCalledWith("clear_database");
  });

  it("saves the context.dev API key from Settings", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "set_contextdev_api_key")
        return Promise.resolve({ configured: true, from_env: false });
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Impostazioni"));
    await userEvent.type(await screen.findByLabelText("Chiave API context.dev"), " cd-key ");
    const form = screen.getByLabelText("Chiave API context.dev").closest("form")!;
    await userEvent.click(within(form).getByRole("button", { name: "Salva chiave" }));
    expect(invoke).toHaveBeenCalledWith("set_contextdev_api_key", { apiKey: "cd-key" });
    expect(await within(form.parentElement!).findByText("Chiave configurata")).toBeInTheDocument();
    expect(screen.getByLabelText("Chiave API context.dev")).toHaveValue("");
  });

  it("turns Google Maps discovery on from Settings", async () => {
    invoke.mockImplementation((command: string, args?: { settings?: unknown }) => {
      if (command === "set_contextdev_settings") return Promise.resolve(args?.settings);
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Impostazioni"));
    const maps = await screen.findByLabelText(
      "Cerca i siti anche su Google Maps (costo da misurare)",
    );
    expect(maps).not.toBeChecked();
    await userEvent.click(maps);
    expect(invoke).toHaveBeenCalledWith("set_contextdev_settings", {
      settings: { concurrency: 2, maps_enabled: true },
    });
    await waitFor(() => expect(maps).toBeChecked());
  });

  it("gates blacklist clearing behind two confirmations", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "refresh_dashboard")
        return Promise.resolve({ ...dashboard, blacklist_count: 1 });
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Impostazioni"));
    await userEvent.click(screen.getByText("Azioni distruttive"));
    await userEvent.click(screen.getByRole("button", { name: "Ripulisci blacklist" }));
    await userEvent.click(await screen.findByRole("button", { name: "Continua" }));
    expect(invoke).not.toHaveBeenCalledWith("clear_blacklist");
    await userEvent.click(await screen.findByRole("button", { name: "Elimina definitivamente" }));
    expect(invoke).toHaveBeenCalledWith("clear_blacklist");
  });

  it("exports database backup from Settings page", async () => {
    const { save } = await import("@tauri-apps/plugin-dialog");
    vi.mocked(save).mockResolvedValue("backup.zip" as never);
    invoke.mockImplementation((command: string) => {
      if (command === "export_database_backup") {
        return Promise.resolve({
          path: "backup.zip",
          size_bytes: 1024 * 1024 * 5,
          contact_count: 10,
          blacklist_count: 2,
        });
      }
      return defaultInvoke(command);
    });

    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Impostazioni"));
    const exportBtn = screen.getByRole("button", { name: /Esporta backup database/i });
    await userEvent.click(exportBtn);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("export_database_backup", {
        destinationPath: "backup.zip",
      });
    });
    expect(await screen.findByText(/Backup esportato con successo/i)).toBeInTheDocument();
  });

  it("restores database backup from Settings page after confirmation", async () => {
    openFiles.mockResolvedValue("backup.zip" as never);
    invoke.mockImplementation((command: string) => {
      if (command === "import_database_backup") {
        return Promise.resolve({
          contact_count: 50,
          blacklist_count: 5,
          candidates_count: 60,
        });
      }
      return defaultInvoke(command);
    });

    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByTitle("Impostazioni"));
    const importBtn = screen.getByRole("button", { name: /Importa \/ Ripristina database/i });
    await userEvent.click(importBtn);

    const confirmBtn = await screen.findByRole("button", { name: "Ripristina database" });
    await userEvent.click(confirmBtn);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("import_database_backup", {
        sourcePath: "backup.zip",
      });
    });
    expect(await screen.findByText(/Database ripristinato con successo/i)).toBeInTheDocument();
  });

  it("keeps individual contact import errors suppressed in the UI", async () => {
    const { open } = await import("@tauri-apps/plugin-dialog");
    vi.mocked(open).mockResolvedValue(["broken.csv"] as never);
    invoke.mockImplementation((command: string) =>
      command === "import_contact_paths"
        ? Promise.reject(new Error("bad row"))
        : defaultInvoke(command),
    );
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByRole("button", { name: /Importa contatti/ }));
    await userEvent.click(await screen.findByRole("menuitem", { name: "Seleziona file" }));
    expect(await screen.findByText("Error: bad row")).toBeInTheDocument();
  });

  it("keeps the newest contact search when an older list resolves last", async () => {
    const oldRequest = deferred<ReturnType<typeof contactListResult>>();
    const newRequest = deferred<ReturnType<typeof contactListResult>>();
    let listCalls = 0;
    invoke.mockImplementation((command: string) => {
      if (command === "list_contacts")
        return ++listCalls === 1 ? oldRequest.promise : newRequest.promise;
      return defaultInvoke(command);
    });

    render(<App />);
    await screen.findByText("info@example.it");
    const search = screen.getByPlaceholderText("Cerca email, file, categoria, località…");
    fireEvent.change(search, { target: { value: "old" } });
    await waitFor(() => expect(listCalls).toBe(1));
    fireEvent.change(search, { target: { value: "new" } });
    await waitFor(() => expect(listCalls).toBe(2));
    await act(() => {
      newRequest.resolve(contactListResult("new", 2));
      return newRequest.promise;
    });
    expect(screen.getByText("new@example.it")).toBeInTheDocument();
    expect(screen.getByText("2 risultati")).toBeInTheDocument();

    await act(() => {
      oldRequest.resolve(contactListResult("old", 9));
      return oldRequest.promise;
    });
    expect(screen.getByText("new@example.it")).toBeInTheDocument();
    expect(screen.queryByText("old@example.it")).not.toBeInTheDocument();
    expect(screen.getByText("2 risultati")).toBeInTheDocument();
  });

  it("ignores an older list rejection but displays the newest rejection", async () => {
    const staleRequest = deferred<ReturnType<typeof contactListResult>>();
    const currentRequest = deferred<ReturnType<typeof contactListResult>>();
    const latestRequest = deferred<ReturnType<typeof contactListResult>>();
    let listCalls = 0;
    invoke.mockImplementation((command: string) => {
      if (command !== "list_contacts") return defaultInvoke(command);
      listCalls += 1;
      return [staleRequest.promise, currentRequest.promise, latestRequest.promise][listCalls - 1];
    });

    render(<App />);
    await screen.findByText("info@example.it");
    const search = screen.getByPlaceholderText("Cerca email, file, categoria, località…");
    fireEvent.change(search, { target: { value: "current" } });
    await waitFor(() => expect(listCalls).toBe(1));
    fireEvent.change(search, { target: { value: "current2" } });
    await waitFor(() => expect(listCalls).toBe(2));
    await act(() => {
      currentRequest.resolve(contactListResult("current", 3));
      return currentRequest.promise;
    });
    await act(async () => staleRequest.reject(new Error("stale list error")));
    expect(screen.queryByText(/stale list error/)).not.toBeInTheDocument();
    expect(screen.getByText("current@example.it")).toBeInTheDocument();

    fireEvent.change(search, { target: { value: "latest" } });
    await waitFor(() => expect(listCalls).toBe(3));
    await act(async () => latestRequest.reject(new Error("latest list error")));
    expect(screen.getByText(/latest list error/)).toBeInTheDocument();
  });

  it("keeps the newest blacklist result when an older search resolves last", async () => {
    vi.useFakeTimers();
    const oldRequest = deferred<ReturnType<typeof blacklistResult>>();
    const newRequest = deferred<ReturnType<typeof blacklistResult>>();
    let blacklistCalls = 0;
    invoke.mockImplementation((command: string) => {
      if (command === "list_blacklist")
        return ++blacklistCalls === 1 ? oldRequest.promise : newRequest.promise;
      return defaultInvoke(command);
    });

    render(<App />);
    fireEvent.click(screen.getByTitle("Blacklist"));
    await act(() => vi.advanceTimersByTimeAsync(150));
    expect(blacklistCalls).toBe(1);
    fireEvent.change(screen.getByPlaceholderText("Cerca email o file di origine…"), {
      target: { value: "new" },
    });
    await act(() => vi.advanceTimersByTimeAsync(150));
    expect(blacklistCalls).toBe(2);
    await act(() => {
      newRequest.resolve(blacklistResult("new", 2));
      return newRequest.promise;
    });
    expect(screen.getByText("new@blocked.it")).toBeInTheDocument();
    expect(screen.getByText(/1–1 di 2/)).toBeInTheDocument();

    await act(() => {
      oldRequest.resolve(blacklistResult("old", 8));
      return oldRequest.promise;
    });
    expect(screen.getByText("new@blocked.it")).toBeInTheDocument();
    expect(screen.queryByText("old@blocked.it")).not.toBeInTheDocument();
    expect(screen.getByText(/1–1 di 2/)).toBeInTheDocument();
    vi.useRealTimers();
  });

  it("ignores a blacklist rejection after page exit and displays the newest active rejection", async () => {
    vi.useFakeTimers();
    const staleRequest = deferred<ReturnType<typeof blacklistResult>>();
    const latestRequest = deferred<ReturnType<typeof blacklistResult>>();
    let blacklistCalls = 0;
    invoke.mockImplementation((command: string) => {
      if (command === "list_blacklist")
        return ++blacklistCalls === 1 ? staleRequest.promise : latestRequest.promise;
      return defaultInvoke(command);
    });

    render(<App />);
    fireEvent.click(screen.getByTitle("Blacklist"));
    await act(() => vi.advanceTimersByTimeAsync(150));
    fireEvent.click(screen.getByTitle("Contatti"));
    await act(async () => staleRequest.reject(new Error("inactive blacklist error")));
    expect(screen.queryByText(/inactive blacklist error/)).not.toBeInTheDocument();

    fireEvent.click(screen.getByTitle("Blacklist"));
    await act(() => vi.advanceTimersByTimeAsync(150));
    await act(async () => latestRequest.reject(new Error("latest blacklist error")));
    expect(screen.getByText(/latest blacklist error/)).toBeInTheDocument();
    vi.useRealTimers();
  });

  it("keeps dashboard and blacklist request generations independent", async () => {
    vi.useFakeTimers();
    const dashboardRequest = deferred<ReturnType<typeof dashboardResult>>();
    const blacklistRequest = deferred<ReturnType<typeof blacklistResult>>();
    invoke.mockImplementation((command: string) => {
      if (command === "refresh_dashboard") return dashboardRequest.promise;
      if (command === "list_blacklist") return blacklistRequest.promise;
      return defaultInvoke(command);
    });

    render(<App />);
    fireEvent.click(screen.getByTitle("Blacklist"));
    await act(() => vi.advanceTimersByTimeAsync(150));
    await act(() => {
      dashboardRequest.resolve({ ...dashboardResult("dashboard", 4), blacklist_count: 0 });
      return dashboardRequest.promise;
    });
    await act(() => {
      blacklistRequest.resolve(blacklistResult("blacklist", 5));
      return blacklistRequest.promise;
    });
    expect(screen.getByText("blacklist@blocked.it")).toBeInTheDocument();
    fireEvent.click(screen.getByTitle("Contatti"));
    expect(screen.getByText("dashboard@example.it")).toBeInTheDocument();
    expect(screen.getByText("4 risultati")).toBeInTheDocument();
  });

  it("does not update state after unmounting with dashboard and blacklist requests pending", async () => {
    vi.useFakeTimers();
    const dashboardRequest = deferred<ReturnType<typeof dashboardResult>>();
    const blacklistRequest = deferred<ReturnType<typeof blacklistResult>>();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    invoke.mockImplementation((command: string) => {
      if (command === "refresh_dashboard") return dashboardRequest.promise;
      if (command === "list_blacklist") return blacklistRequest.promise;
      return defaultInvoke(command);
    });

    const view = render(<App />);
    fireEvent.click(screen.getByTitle("Blacklist"));
    await act(() => vi.advanceTimersByTimeAsync(150));
    view.unmount();
    await act(async () => {
      dashboardRequest.resolve(dashboardResult("late", 1));
      blacklistRequest.resolve(blacklistResult("late", 1));
      await Promise.all([dashboardRequest.promise, blacklistRequest.promise]);
    });
    expect(consoleError).not.toHaveBeenCalled();
    vi.useRealTimers();
  });

  it("keeps the blacklist size when a search narrows the list", async () => {
    invoke.mockImplementation((command: string, args?: { query?: string }) => {
      if (command === "list_blacklist")
        return Promise.resolve(
          args?.query ? blacklistResult("found", 3) : blacklistResult("all", 40),
        );
      return defaultInvoke(command);
    });
    render(<App />);
    await userEvent.click(screen.getByTitle("Blacklist"));
    await screen.findByText("all@blocked.it");
    const size = () => screen.getByText("In blacklist").nextElementSibling?.textContent;
    await waitFor(() => expect(size()).toBe("40"));
    await userEvent.type(screen.getByPlaceholderText("Cerca email o file di origine…"), "found");
    await screen.findByText("found@blocked.it");
    expect(size()).toBe("40");
  });

  it("reloads contacts after an address is blacklisted", async () => {
    let dashboards = 0;
    invoke.mockImplementation((command: string) => {
      if (command === "refresh_dashboard") dashboards += 1;
      if (command === "add_blacklist_entry") return Promise.resolve(true);
      if (command === "list_blacklist") return Promise.resolve(blacklistResult("manual", 1));
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    const before = dashboards;
    await userEvent.click(screen.getByTitle("Blacklist"));
    await userEvent.type(
      await screen.findByPlaceholderText("Aggiungi un'email alla blacklist (es. info@esempio.it)…"),
      "info@example.it",
    );
    await userEvent.click(screen.getByRole("button", { name: "Aggiungi" }));
    await screen.findByText("Indirizzo aggiunto alla blacklist");
    await userEvent.click(screen.getByTitle("Contatti"));
    await waitFor(() => expect(dashboards).toBeGreaterThan(before));
  });

  it("shows working columns and the full record in the detail sheet", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    expect(screen.getAllByRole("columnheader").map((header) => header.textContent)).toEqual([
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
      "azioni",
    ]);
    // The stored confidence is fixed per method, so the table does not show it.
    expect(screen.getByText("file")).toBeInTheDocument();
    expect(screen.queryByText(/90\s*%/)).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Dettagli" }));
    const sheet = await screen.findByRole("dialog");
    expect(within(sheet).getByText("Macroarea")).toBeInTheDocument();
    expect(within(sheet).getByText("Nord-Ovest")).toBeInTheDocument();
    expect(within(sheet).getByText("contact-1")).toBeInTheDocument();
    expect(within(sheet).getByText("Punteggio")).toBeInTheDocument();
    expect(within(sheet).getByText("72")).toBeInTheDocument();
  });

  it("filters contacts by minimum quality score", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    fireEvent.change(screen.getByLabelText("Punteggio minimo"), { target: { value: "60" } });
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("list_contacts", {
        filters: expect.objectContaining({ min_score: 60 }),
        limit: 100,
        offset: 0,
        includeTotal: true,
      }),
    );
  });

  it("sorts the list from the column headers and returns to the default order", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    const sortedBy = (sort: unknown) =>
      waitFor(() =>
        expect(invoke).toHaveBeenLastCalledWith("list_contacts", {
          filters: expect.objectContaining({ sort }),
          limit: 100,
          offset: 0,
          includeTotal: true,
        }),
      );
    const header = () => screen.getByRole("columnheader", { name: "punteggio" });
    // Numeric columns start with the highest value, then flip, then reset.
    await userEvent.click(within(header()).getByRole("button"));
    await sortedBy({ column: "score", direction: "desc" });
    expect(header()).toHaveAttribute("aria-sort", "descending");
    await userEvent.click(within(header()).getByRole("button"));
    await sortedBy({ column: "score", direction: "asc" });
    await userEvent.click(within(header()).getByRole("button"));
    await sortedBy(null);
    // Text columns start alphabetically.
    await userEvent.click(
      within(screen.getByRole("columnheader", { name: "comune" })).getByRole("button"),
    );
    await sortedBy({ column: "comune", direction: "asc" });
  });

  it("deletes a contact only after confirmation", async () => {
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByRole("button", { name: "Cancella" }));
    await userEvent.click(await screen.findByRole("button", { name: "Annulla" }));
    expect(invoke).not.toHaveBeenCalledWith("delete_contact", expect.anything());
    expect(screen.getByText("info@example.it")).toBeInTheDocument();
  });

  it("keeps the import busy until the backend confirms cancellation", async () => {
    openFiles.mockResolvedValue(["slow.csv"]);
    const running = deferred<never>();
    invoke.mockImplementation((command: string) => {
      if (command === "import_contact_paths") return running.promise;
      if (command === "control_import") return Promise.resolve(null);
      return defaultInvoke(command);
    });
    render(<App />);
    await screen.findByText("info@example.it");
    await userEvent.click(screen.getByRole("button", { name: /Importa contatti/ }));
    await userEvent.click(await screen.findByRole("menuitem", { name: "Seleziona file" }));
    await userEvent.click(await screen.findByRole("button", { name: "Ferma importazione" }));
    expect(invoke).toHaveBeenCalledWith("control_import", { action: "cancel" });
    expect(screen.getByText("Importazione in corso")).toBeInTheDocument();
    await act(async () => {
      running.reject(new Error("Importazione annullata"));
      await running.promise.catch(() => {});
    });
    await waitFor(() =>
      expect(screen.queryByText("Importazione in corso")).not.toBeInTheDocument(),
    );
  });
});

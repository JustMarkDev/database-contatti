import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { LeadDirectory, SearchLeadList, SearchLeadView } from "../types";
import { emptyLeadFilters, formatMethod, LEAD_COLUMNS, SearchPage } from "./SearchPage";

const {
  invoke,
  toastError,
  toastSuccess,
  toastWarning,
  onContactsChanged,
  confirmDialog,
  listeners,
} = vi.hoisted(() => ({
  invoke: vi.fn(),
  toastWarning: vi.fn(),
  toastError: vi.fn(),
  toastSuccess: vi.fn(),
  onContactsChanged: vi.fn(),
  confirmDialog: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("../components/ui/toast", () => ({
  toast: { success: toastSuccess, error: toastError, warning: toastWarning, info: vi.fn() },
}));
vi.mock("../components/ui/confirm-dialog", () => ({ confirmDialog }));
vi.mock("../platform", () => ({
  platform: {
    listen: (event: string, callback: (event: { payload: unknown }) => void) => {
      listeners.set(event, callback);
      return Promise.resolve(() => listeners.delete(event));
    },
    saveFile: vi.fn(() => Promise.resolve("/tmp/lead.txt")),
    openFiles: vi.fn(),
  },
}));

type LeadOverrides = Partial<SearchLeadView>;

/** Text of one lead-table column, looked up by its header label. */
function column(label: string): (string | null)[] {
  const table = screen.getByRole("table", { name: "Lead" });
  const index = within(table)
    .getAllByRole("columnheader")
    .map((header) => header.textContent)
    .indexOf(label);
  return within(table)
    .getAllByRole("row")
    .slice(1)
    .map((row) => row.querySelectorAll("td")[index]?.textContent ?? null);
}

function makeLead(id: string, overrides: LeadOverrides = {}): SearchLeadView {
  return {
    email: `${id}@example.test`,
    email_local_part: id,
    email_domain: "example",
    email_tld: ".test",
    email_comparison_key: `${id}@example.test`,
    website: null,
    macroarea: null,
    source_url: `https://www.example.test/${id}`,
    retrieved_at: "2026-09-22T10:00:00Z",
    is_pec: false,
    person_name: null,
    role: null,
    category: null,
    subcategory: null,
    organization: null,
    comune: null,
    provincia: null,
    regione: null,
    official_code: null,
    department: null,
    id: `lead-${id}`,
    run_id: "run-1",
    run_label: "Fonte: test",
    address: null,
    postal_code: null,
    paese: null,
    method: "sito",
    source_count: 1,
    in_database: false,
    stato: "nuovo",
    ...overrides,
  };
}

function leadPage(leads: SearchLeadView[], total = leads.length, offset = 0): SearchLeadList {
  return {
    leads,
    total,
    limit: 100,
    offset,
    has_previous: offset > 0,
    has_next: offset + leads.length < total,
  };
}

const run = { run_id: "run-1", lead_count: 2, page_count: 3, pdf_found: 4 };

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

const catalog: LeadDirectory[] = [
  {
    id: "dir-cai",
    name: "CAI sezioni",
    publisher: "Club Alpino Italiano",
    landing_url: "https://www.cai.example.test/sezioni/",
    category: "Associazioni",
    subcategory: "CAI",
    note: "Elenco per regione",
  },
];

const high = makeLead("high");
const low = makeLead("low");

function defaultInvoke(command: string): Promise<unknown> {
  switch (command) {
    case "discover_by_search":
      return Promise.resolve({
        found: 4,
        known: 1,
        rejected: 0,
        accepted: 2,
        to_verify: 1,
        credits: 3,
      });
    case "find_seed_sites":
      return Promise.resolve(3);
    case "estimate_explore":
      return Promise.resolve({ sites: 3, max_credits: 12 });
    case "list_channel_yields":
      return Promise.resolve([
        {
          channel: "Scoperta: ricerca web",
          discovery_credits: 2,
          reading_credits: 8,
          seeds: 2,
          new_emails: 4,
        },
      ]);
    case "get_contextdev_settings":
      return Promise.resolve({ concurrency: 2, maps_enabled: false });
    case "discover_by_maps":
      return Promise.resolve({
        found: 2,
        known: 0,
        rejected: 0,
        accepted: 1,
        to_verify: 0,
        credits: 1,
      });
    case "list_lead_directories":
      return Promise.resolve(catalog);
    case "list_lead_taxonomy":
      return Promise.resolve([
        ["Enti religiosi", ["Parrocchia", "Oratorio / centro giovanile"]],
        ["Associazioni", ["CAI"]],
      ]);
    case "list_search_runs":
      return Promise.resolve([
        {
          id: "run-1",
          label: "Fonte: test",
          kind: "source",
          status: "completed",
          lead_count: 2,
          page_count: 0,
          error: null,
          created_at: "2026-09-29T10:00:00Z",
        },
      ]);
    case "list_seed_lists":
      return Promise.resolve([
        {
          list_name: "Fonte: OSM",
          total: 5,
          pending: 3,
          crawled: 1,
          failed: 0,
          without_site: 1,
          to_verify: 0,
        },
      ]);
    case "get_lead_filter_options":
      return Promise.resolve({
        run: [{ value: "run-1", count: 2 }],
        category: [],
        subcategory: [],
        regione: [{ value: "Lombardia", count: 2 }],
        provincia: [],
        method: [],
        pec: [],
        stato: [],
      });
    case "explore_seeds":
    case "explore_directory":
      return Promise.resolve(run);
    case "list_search_leads":
      return Promise.resolve(leadPage([high, low], 2));
    case "promote_search_leads":
      return Promise.resolve({ added: 2, updated: 0, skipped_blacklist: 1, unverified: 1 });
    default:
      return Promise.resolve({});
  }
}

async function renderPage() {
  render(<SearchPage active={true} onContactsChanged={onContactsChanged} />);
  await screen.findAllByText("high@example.test");
}

afterEach(() => {
  cleanup();
});

beforeEach(() => {
  vi.clearAllMocks();
  invoke.mockImplementation(defaultInvoke);
  confirmDialog.mockResolvedValue(true);
});

describe("SearchPage", () => {
  it("renders nothing and performs no read while inactive", () => {
    const { container } = render(
      <SearchPage active={false} onContactsChanged={onContactsChanged} />,
    );
    expect(container).toBeEmptyDOMElement();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("lists the whole sandbox with the contact columns and opens the details", async () => {
    const school = makeLead("dirigente", {
      role: "Responsabile",
      organization: "IC Esempio",
      department: "Segreteria",
      category: "Istruzione",
      subcategory: "Istituto comprensivo",
      comune: "Bergamo",
      provincia: "Bergamo",
      regione: "Lombardia",
      address: "Via Roma 1",
      postal_code: "24100",
      method: "archivio+ia",
      source_count: 2,
      in_database: true,
      stato: "completa",
      is_pec: true,
    });
    invoke.mockImplementation((command: string) =>
      command === "list_search_leads"
        ? Promise.resolve(leadPage([school], 1))
        : defaultInvoke(command),
    );
    render(<SearchPage active={true} onContactsChanged={onContactsChanged} />);
    await screen.findByText("IC Esempio · Segreteria");
    expect(invoke).toHaveBeenCalledWith("list_search_leads", {
      filters: emptyLeadFilters(),
      limit: 100,
      offset: 0,
    });
    const leadTable = screen.getByRole("table", { name: "Lead" });
    expect(
      within(leadTable)
        .getAllByRole("columnheader")
        .map((header) => header.textContent),
    ).toEqual([...LEAD_COLUMNS, "azioni"]);
    expect(column("sottocategoria")).toEqual(["Istituto comprensivo"]);
    expect(column("pec")).toEqual(["PEC"]);
    expect(column("fonti")).toEqual(["2"]);
    expect(column("stato")).toEqual(["Completa il contatto"]);

    await userEvent.click(screen.getByRole("button", { name: "Dettagli" }));
    const place = await screen.findByRole("region", { name: "Località" });
    expect(within(place).getByText("Via Roma 1")).toBeInTheDocument();
    expect(within(place).getByText("24100")).toBeInTheDocument();
    const origin = screen.getByRole("region", { name: "Fonte" });
    expect(within(origin).getByText("Archivio web + IA")).toBeInTheDocument();
  });

  it("reads the chosen public sites with the IA, chaining each list's sites", async () => {
    await renderPage();
    const start = screen.getByRole("button", { name: "Leggi i siti scelti" });
    expect(start).toBeDisabled();
    await userEvent.click(
      within(screen.getByText("CAI sezioni").closest("li")!).getByRole("checkbox"),
    );
    await userEvent.click(
      within(screen.getByText("Fonte: OSM").closest("li")!).getByRole("checkbox"),
    );
    await userEvent.click(start);
    const exploreCalls = () =>
      invoke.mock.calls.filter(([command]) => command === "explore_seeds").map(([, args]) => args);
    await waitFor(() => expect(exploreCalls()).toHaveLength(2));
    expect(invoke).toHaveBeenCalledWith("explore_directory", {
      request: { sourceId: "dir-cai", useAi: true },
    });
    const base = { limit: 50, maxPages: 3, maxPdfs: 0, useAi: true };
    expect(confirmDialog).toHaveBeenCalledWith(
      expect.objectContaining({
        description: expect.stringContaining("≈ 6 siti, al massimo ≈ 24 crediti"),
      }),
    );
    expect(invoke).toHaveBeenCalledWith("estimate_explore", {
      request: { ...base, listName: "Fonte: OSM" },
    });
    expect(exploreCalls()).toEqual([
      { request: { ...base, listName: "Elenco: CAI sezioni" } },
      { request: { ...base, listName: "Fonte: OSM" } },
    ]);
  });

  it("reads nothing when the estimate is declined, and uses the chosen pages per site", async () => {
    await renderPage();
    await userEvent.selectOptions(screen.getByLabelText("Pagine per sito"), "5");
    await userEvent.click(
      within(screen.getByText("Fonte: OSM").closest("li")!).getByRole("checkbox"),
    );
    confirmDialog.mockResolvedValueOnce(false);
    await userEvent.click(screen.getByRole("button", { name: "Leggi i siti scelti" }));
    await waitFor(() => expect(confirmDialog).toHaveBeenCalled());
    expect(invoke).toHaveBeenCalledWith("estimate_explore", {
      request: expect.objectContaining({ listName: "Fonte: OSM", maxPages: 5 }),
    });
    expect(invoke).not.toHaveBeenCalledWith("explore_seeds", expect.anything());
  });

  it("shows the credits per new address of each discovery channel", async () => {
    await renderPage();
    const table = await screen.findByRole("table", { name: "Resa per fonte" });
    const cells = within(table)
      .getAllByRole("cell")
      .map((cell) => cell.textContent);
    expect(cells).toEqual(["Scoperta: ricerca web", "2", "10", "4", "2,5"]);
  });

  it("keeps reading the other chosen sites when one of them fails", async () => {
    invoke.mockImplementation((command: string) =>
      command === "explore_directory" ? Promise.reject("HTTP 500") : defaultInvoke(command),
    );
    await renderPage();
    await userEvent.click(
      within(screen.getByText("CAI sezioni").closest("li")!).getByRole("checkbox"),
    );
    await userEvent.click(
      within(screen.getByText("Fonte: OSM").closest("li")!).getByRole("checkbox"),
    );
    await userEvent.click(screen.getByRole("button", { name: "Leggi i siti scelti" }));
    await waitFor(() => expect(toastError).toHaveBeenCalledWith("CAI sezioni: HTTP 500"));
    expect(invoke).toHaveBeenCalledWith("explore_seeds", {
      request: expect.objectContaining({ listName: "Fonte: OSM" }),
    });
  });

  it("shows crawl progress and stops the crawl", async () => {
    const pending = deferred<unknown>();
    invoke.mockImplementation((command: string) =>
      command === "explore_seeds" ? pending.promise : defaultInvoke(command),
    );
    await renderPage();
    await userEvent.click(
      within(screen.getByText("Fonte: OSM").closest("li")!).getByRole("checkbox"),
    );
    await userEvent.click(screen.getByRole("button", { name: "Leggi i siti scelti" }));
    act(() => {
      listeners.get("lead-crawl-progress")?.({
        payload: {
          run_id: "r",
          label: "x",
          done: 2,
          total: 5,
          leads: 7,
          host: "a.example.test",
          finished: false,
          credits_used: 4,
          credits_remaining: 900,
        },
      });
    });
    expect(
      await screen.findByText("2 di 5 siti, 7 lead, 4 crediti · a.example.test"),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Ferma esplorazione" }));
    expect(invoke).toHaveBeenCalledWith("cancel_lead_crawl");
    await act(async () => {
      pending.resolve(run);
      await pending.promise;
    });
    await waitFor(() => expect(screen.queryByText(/di 5 siti/)).not.toBeInTheDocument());
  });

  const plan = {
    category: "Turismo e promozione territoriale",
    subcategory: "Pro Loco",
    regione: "Lombardia",
    source_ids: ["dir-cai"],
    web_query: "pro loco Lombardia email",
  };

  it("turns a free-form request into directories, collected sites and a web search", async () => {
    invoke.mockImplementation((command: string) =>
      command === "plan_lead_request" ? Promise.resolve(plan) : defaultInvoke(command),
    );
    await renderPage();
    await userEvent.type(
      screen.getByLabelText("Descrizione della ricerca lead"),
      "tutte le pro loco della Lombardia",
    );
    await userEvent.click(screen.getByRole("button", { name: "Cerca" }));
    const scoped = {
      category: "Turismo e promozione territoriale",
      subcategory: "Pro Loco",
      regione: "Lombardia",
      limit: 50,
      maxPages: 3,
      maxPdfs: 0,
      useAi: true,
    };
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("explore_seeds", {
        request: { listName: "Scoperta: ricerca web", ...scoped },
      }),
    );
    expect(invoke).not.toHaveBeenCalledWith("discover_by_maps", expect.anything());
    expect(invoke).toHaveBeenCalledWith("discover_by_search", {
      request: {
        query: "pro loco Lombardia email",
        category: "Turismo e promozione territoriale",
        subcategory: "Pro Loco",
        regione: "Lombardia",
      },
    });
    expect(toastSuccess).toHaveBeenCalledWith(expect.stringContaining("2 siti nuovi"));
    expect(invoke).toHaveBeenCalledWith("plan_lead_request", {
      request: "tutte le pro loco della Lombardia",
    });
    expect(invoke).toHaveBeenCalledWith("explore_directory", {
      request: { sourceId: "dir-cai", useAi: true },
    });
    expect(invoke).toHaveBeenCalledWith("explore_seeds", {
      request: { listName: null, ...scoped },
    });
  });

  it("adds Google Maps to the request only when it is enabled", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "get_contextdev_settings") {
        return Promise.resolve({ concurrency: 2, maps_enabled: true });
      }
      return command === "plan_lead_request" ? Promise.resolve(plan) : defaultInvoke(command);
    });
    await renderPage();
    await userEvent.type(screen.getByLabelText("Descrizione della ricerca lead"), "pro loco");
    await userEvent.click(screen.getByRole("button", { name: "Cerca" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("explore_seeds", {
        request: expect.objectContaining({ listName: "Scoperta: Google Maps" }),
      }),
    );
    expect(invoke).toHaveBeenCalledWith("discover_by_maps", {
      request: {
        category: "Turismo e promozione territoriale",
        subcategory: "Pro Loco",
        regione: "Lombardia",
      },
    });
    expect(toastSuccess).toHaveBeenCalledWith(expect.stringContaining("3 siti nuovi"));
  });

  it("does not read the search list when the web search finds no new site", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "discover_by_search") {
        return Promise.resolve({
          found: 2,
          known: 2,
          rejected: 0,
          accepted: 0,
          to_verify: 0,
          credits: 1,
        });
      }
      return command === "plan_lead_request" ? Promise.resolve(plan) : defaultInvoke(command);
    });
    await renderPage();
    await userEvent.type(screen.getByLabelText("Descrizione della ricerca lead"), "pro loco");
    await userEvent.click(screen.getByRole("button", { name: "Cerca" }));
    await waitFor(() => expect(toastSuccess).toHaveBeenCalled());
    expect(invoke).not.toHaveBeenCalledWith("explore_seeds", {
      request: expect.objectContaining({ listName: "Scoperta: ricerca web" }),
    });
  });

  it("finds the sites of organizations known only by name", async () => {
    await renderPage();
    await userEvent.click(screen.getByRole("button", { name: "Trova i siti di Fonte: OSM" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("find_seed_sites", { listName: "Fonte: OSM", limit: 50 }),
    );
    expect(toastSuccess).toHaveBeenCalledWith("Fonte: OSM: 3 siti trovati da leggere");
  });

  it("paginates in bounded 100-row pages", async () => {
    const firstPage = Array.from({ length: 100 }, (_, index) => makeLead(`lead-${index}`));
    invoke.mockImplementation((command: string, args?: { offset?: number }) => {
      if (command !== "list_search_leads") return defaultInvoke(command);
      return Promise.resolve(
        args?.offset === 100 ? leadPage([makeLead("last")], 101, 100) : leadPage(firstPage, 101),
      );
    });
    render(<SearchPage active={true} onContactsChanged={onContactsChanged} />);
    const next = await screen.findByRole("button", { name: "Successiva" });
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);
    expect(await screen.findAllByText("last@example.test")).not.toHaveLength(0);
    expect(screen.getByRole("button", { name: "Successiva" })).toBeDisabled();
  });

  it("ignores a stale list response that resolves after a newer one", async () => {
    const first = deferred<SearchLeadList>();
    const second = deferred<SearchLeadList>();
    let listCalls = 0;
    invoke.mockImplementation((command: string) => {
      if (command === "list_search_leads") {
        listCalls += 1;
        if (listCalls === 1) return Promise.resolve(leadPage([makeLead("initial")], 1));
        return listCalls === 2 ? first.promise : second.promise;
      }
      return defaultInvoke(command);
    });
    render(<SearchPage active={true} onContactsChanged={onContactsChanged} />);
    await screen.findAllByText("initial@example.test");
    const search = screen.getByLabelText("Cerca nei lead");
    fireEvent.change(search, { target: { value: "ab" } });
    await waitFor(() => expect(listCalls).toBe(2));
    fireEvent.change(search, { target: { value: "abc" } });
    await waitFor(() => expect(listCalls).toBe(3));
    await act(() => {
      second.resolve(leadPage([makeLead("new")], 1));
      return second.promise;
    });
    expect(await screen.findAllByText("new@example.test")).not.toHaveLength(0);
    await act(() => {
      first.resolve(leadPage([makeLead("old")], 1));
      return first.promise;
    });
    expect(screen.queryAllByText("old@example.test")).toHaveLength(0);
  });

  it("promotes the filtered leads after confirmation", async () => {
    await renderPage();
    await userEvent.click(screen.getByRole("button", { name: "Aggiungi al database" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("promote_search_leads", { filters: emptyLeadFilters() }),
    );
    expect(toastSuccess).toHaveBeenCalledWith(
      "Lead aggiunti al database: 2 nuovi, 0 aggiornati, 1 in blacklist, 1 in attesa di verifica del dominio",
    );
    expect(onContactsChanged).toHaveBeenCalled();
    confirmDialog.mockResolvedValueOnce(false);
    invoke.mockClear();
    await userEvent.click(screen.getByRole("button", { name: "Aggiungi al database" }));
    expect(invoke).not.toHaveBeenCalledWith("promote_search_leads", expect.anything());
  });

  it("lists and promotes every run kind from one table", async () => {
    await renderPage();
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("list_search_leads", {
        filters: emptyLeadFilters(),
        limit: 100,
        offset: 0,
      }),
    );
    expect(invoke).toHaveBeenCalledWith("get_lead_filter_options", { kinds: [] });
    expect(screen.queryByRole("tab")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Importa/ })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Aggiungi al database" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("promote_search_leads", { filters: emptyLeadFilters() }),
    );
  });

  it("formats extraction methods in words", () => {
    expect(formatMethod("open_data")).toBe("Dati aperti");
    expect(formatMethod("contextdev+ia")).toBe("Sito web (context.dev) + IA");
    expect(formatMethod("tinyfish+ia")).toBe("Sito (TinyFish) + IA");
    expect(formatMethod(null)).toBe("—");
  });
});

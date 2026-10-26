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
    case "list_subcategory_coverage":
      return Promise.resolve([
        {
          category: "Associazioni",
          subcategory: "CAI",
          sites: 4,
          pending: 0,
          crawled: 4,
          failed: 0,
          to_verify: 0,
          leads: 9,
          new_leads: 6,
          credits: 12,
        },
        {
          category: "Istruzione",
          subcategory: "Infanzia",
          sites: 10,
          pending: 6,
          crawled: 3,
          failed: 1,
          to_verify: 0,
          leads: 12,
          new_leads: 5,
          credits: 20,
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

/** Opens a searchable menu and picks the option whose name starts with `option`. */
async function choose(field: string, option: string) {
  await userEvent.click(screen.getByRole("button", { name: field }));
  await userEvent.click(await screen.findByRole("option", { name: new RegExp(`^${option}`) }));
}

async function renderPage() {
  render(<SearchPage active={true} onContactsChanged={onContactsChanged} />);
  await screen.findAllByText("high@example.test");
}

afterEach(() => {
  cleanup();
  localStorage.clear();
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

  const exploreCalls = () =>
    invoke.mock.calls.filter(([command]) => command === "explore_seeds").map(([, args]) => args);
  const coverageRow = (pending: number) => ({
    category: "Istruzione",
    subcategory: "Infanzia",
    sites: 10,
    pending,
    crawled: 10 - pending,
    failed: 0,
    to_verify: 0,
    leads: 12,
    new_leads: 5,
    credits: 20,
  });

  it("lists the askable types of ente with their known sites and shows how far the reading is", async () => {
    await renderPage();
    await userEvent.click(screen.getByRole("button", { name: "Tipo di ente" }));
    expect(await screen.findByRole("option", { name: /^CAI/ })).toHaveTextContent("CAI4");
    const infanzia = screen.getByRole("option", { name: /^Infanzia/ });
    expect(infanzia).toHaveTextContent("Infanzia10");
    await userEvent.click(infanzia);
    expect(
      await screen.findByText(
        /10 siti noti · 3 letti · 6 da leggere · 12 indirizzi trovati \(5 nuovi\) · 20 crediti \(4 per indirizzo nuovo\)/,
      ),
    ).toBeInTheDocument();
    expect(screen.getByText(/1 non raggiungibili/)).toBeInTheDocument();
  });

  it("reads every unread site of the type of ente, batch after batch, until none is left", async () => {
    let pending = 6;
    invoke.mockImplementation((command: string) => {
      if (command === "list_subcategory_coverage") return Promise.resolve([coverageRow(pending)]);
      if (command === "explore_seeds") {
        if (pending === 0) return Promise.reject("Nessun sito da esplorare in questo elenco");
        pending = 0;
        return Promise.resolve(run);
      }
      return defaultInvoke(command);
    });
    await renderPage();
    await choose("Tipo di ente", "Infanzia");
    await userEvent.click(await screen.findByRole("button", { name: "Leggi i siti" }));
    await waitFor(() =>
      expect(toastSuccess).toHaveBeenCalledWith("Lettura conclusa: 2 lead, 3 pagine lette"),
    );
    expect(confirmDialog).toHaveBeenCalledWith(
      expect.objectContaining({
        description: expect.stringContaining("≈ 6 siti, ≈ 24 crediti"),
      }),
    );
    const request = {
      listName: null,
      category: "Istruzione",
      subcategory: "Infanzia",
      regione: null,
      limit: 500,
      maxPages: 3,
      maxPdfs: 0,
    };
    expect(exploreCalls()).toEqual([{ request }, { request }]);
  });

  it("stops instead of repeating a batch that read nothing new", async () => {
    await renderPage();
    await choose("Tipo di ente", "Infanzia");
    await userEvent.click(await screen.findByRole("button", { name: "Leggi i siti" }));
    await waitFor(() =>
      expect(toastSuccess).toHaveBeenCalledWith(expect.stringContaining("6 ancora da leggere")),
    );
    expect(exploreCalls()).toHaveLength(1);
  });

  it("reads nothing when the estimate is declined, and uses the pages per site saved in Settings", async () => {
    localStorage.setItem("explore_prefs", JSON.stringify({ pagesPerSite: 5, schoolPdfs: 1 }));
    await renderPage();
    await choose("Tipo di ente", "Infanzia");
    confirmDialog.mockResolvedValueOnce(false);
    await userEvent.click(await screen.findByRole("button", { name: "Leggi i siti" }));
    await waitFor(() => expect(confirmDialog).toHaveBeenCalled());
    // 6 sites x (Map + 5 pages + 1 PDF of a school)
    expect(confirmDialog).toHaveBeenCalledWith(
      expect.objectContaining({ description: expect.stringContaining("≈ 42 crediti") }),
    );
    expect(invoke).not.toHaveBeenCalledWith("explore_seeds", expect.anything());
  });

  it("sizes each batch so one run stays under the per-run credit cap", async () => {
    invoke.mockImplementation((command: string) =>
      command === "get_contextdev_settings"
        ? Promise.resolve({ concurrency: 2, maps_enabled: false, max_credits_per_run: 100 })
        : defaultInvoke(command),
    );
    await renderPage();
    await choose("Tipo di ente", "Infanzia");
    await userEvent.click(await screen.findByRole("button", { name: "Leggi i siti" }));
    await waitFor(() => expect(exploreCalls()).toHaveLength(1));
    // 100 credits / (Map + 3 pages)
    expect(exploreCalls()[0]).toEqual({ request: expect.objectContaining({ limit: 25 }) });
  });

  it("shows the credits per new address of each discovery channel", async () => {
    await renderPage();
    const table = await screen.findByRole("table", { name: "Resa per fonte" });
    const cells = within(table)
      .getAllByRole("cell")
      .map((cell) => cell.textContent);
    expect(cells).toEqual(["Scoperta: ricerca web", "2", "10", "4", "2,5"]);
  });

  it("keeps looking for more sites when one public list fails", async () => {
    invoke.mockImplementation((command: string) =>
      command === "explore_directory" ? Promise.reject("HTTP 500") : defaultInvoke(command),
    );
    await renderPage();
    await choose("Tipo di ente", "CAI");
    await choose("Regione da cercare", "Lombardia");
    await userEvent.click(await screen.findByRole("button", { name: "Cerca altri siti" }));
    await waitFor(() => expect(toastError).toHaveBeenCalledWith("CAI sezioni: HTTP 500"));
    expect(invoke).toHaveBeenCalledWith("discover_by_search", expect.anything());
  });

  it("shows crawl progress and stops the crawl", async () => {
    const pending = deferred<unknown>();
    invoke.mockImplementation((command: string) =>
      command === "explore_seeds" ? pending.promise : defaultInvoke(command),
    );
    await renderPage();
    await choose("Tipo di ente", "Infanzia");
    await userEvent.click(await screen.findByRole("button", { name: "Leggi i siti" }));
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
    category: "Istruzione",
    subcategory: "Infanzia",
    regione: "Lombardia",
    source_ids: [],
    web_query: "scuole infanzia Lombardia",
  };

  it("turns a free-form request into the picked type of ente and region, and spends nothing", async () => {
    invoke.mockImplementation((command: string) =>
      command === "plan_lead_request" ? Promise.resolve(plan) : defaultInvoke(command),
    );
    await renderPage();
    await userEvent.type(
      screen.getByLabelText("Descrizione della ricerca lead"),
      "tutte le scuole dell'infanzia della Lombardia",
    );
    await userEvent.click(screen.getByRole("button", { name: "Cerca" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Tipo di ente" })).toHaveTextContent("Infanzia"),
    );
    expect(screen.getByRole("button", { name: "Regione da cercare" })).toHaveTextContent(
      "Lombardia",
    );
    expect(invoke).toHaveBeenCalledWith("plan_lead_request", {
      request: "tutte le scuole dell'infanzia della Lombardia",
    });
    expect(invoke).toHaveBeenCalledWith("list_subcategory_coverage", { regione: "Lombardia" });
    for (const spending of ["explore_seeds", "explore_directory", "discover_by_search"]) {
      expect(invoke).not.toHaveBeenCalledWith(spending, expect.anything());
    }
  });

  it("asks to pick a type when the request names none", async () => {
    invoke.mockImplementation((command: string) =>
      command === "plan_lead_request"
        ? Promise.resolve({ ...plan, category: null, subcategory: null })
        : defaultInvoke(command),
    );
    await renderPage();
    await userEvent.type(screen.getByLabelText("Descrizione della ricerca lead"), "qualcosa");
    await userEvent.click(screen.getByRole("button", { name: "Cerca" }));
    await waitFor(() =>
      expect(toastError).toHaveBeenCalledWith(expect.stringContaining("sceglilo dall'elenco")),
    );
  });

  it("looks for more sites only once the known ones are read, and only in a region", async () => {
    await renderPage();
    const more = () => screen.findByRole("button", { name: "Cerca altri siti" });
    await choose("Tipo di ente", "Infanzia");
    expect(await more()).toBeDisabled(); // 6 sites still to read
    await choose("Tipo di ente", "CAI");
    expect(await more()).toBeDisabled(); // nothing left, but no region yet
    await choose("Regione da cercare", "Lombardia");
    await userEvent.click(await more());
    await waitFor(() => expect(toastSuccess).toHaveBeenCalledWith("2 siti nuovi da leggere"));
    expect(invoke).toHaveBeenCalledWith("explore_directory", {
      request: { sourceId: "dir-cai" },
    });
    expect(invoke).toHaveBeenCalledWith("discover_by_search", {
      request: { category: "Associazioni", subcategory: "CAI", regione: "Lombardia" },
    });
    expect(invoke).not.toHaveBeenCalledWith("discover_by_maps", expect.anything());
    expect(invoke).not.toHaveBeenCalledWith("explore_seeds", expect.anything());
  });

  it("adds Google Maps to the search for more sites only when it is enabled", async () => {
    invoke.mockImplementation((command: string) =>
      command === "get_contextdev_settings"
        ? Promise.resolve({ concurrency: 2, maps_enabled: true })
        : defaultInvoke(command),
    );
    await renderPage();
    await choose("Tipo di ente", "CAI");
    await choose("Regione da cercare", "Lombardia");
    await userEvent.click(await screen.findByRole("button", { name: "Cerca altri siti" }));
    await waitFor(() => expect(toastSuccess).toHaveBeenCalledWith("3 siti nuovi da leggere"));
    expect(invoke).toHaveBeenCalledWith("discover_by_maps", {
      request: { category: "Associazioni", subcategory: "CAI", regione: "Lombardia" },
    });
  });

  it("finds the sites of organizations known only by name, batch after batch, until none is left", async () => {
    let withoutSite = 5;
    invoke.mockImplementation((command: string) => {
      if (command === "get_contextdev_settings") {
        return Promise.resolve({ concurrency: 2, maps_enabled: false, max_credits_per_run: 2 });
      }
      if (command === "find_seed_sites") {
        withoutSite = Math.max(0, withoutSite - 2);
        return Promise.resolve(1);
      }
      if (command === "list_seed_lists") {
        return Promise.resolve([
          {
            list_name: "Fonte: OSM",
            total: 5,
            pending: 3,
            crawled: 0,
            failed: 0,
            without_site: withoutSite,
            to_verify: 0,
          },
        ]);
      }
      return defaultInvoke(command);
    });
    await renderPage();
    await userEvent.click(
      await screen.findByRole("button", { name: "Trova i siti di Fonte: OSM" }),
    );
    await waitFor(() =>
      expect(toastSuccess).toHaveBeenCalledWith("Fonte: OSM: 3 siti trovati da leggere"),
    );
    expect(confirmDialog).toHaveBeenCalledWith(
      expect.objectContaining({ description: expect.stringContaining("≈ 5 crediti") }),
    );
    const calls = invoke.mock.calls.filter(([command]) => command === "find_seed_sites");
    expect(calls).toEqual([
      ["find_seed_sites", { listName: "Fonte: OSM", limit: 2 }],
      ["find_seed_sites", { listName: "Fonte: OSM", limit: 2 }],
      ["find_seed_sites", { listName: "Fonte: OSM", limit: 2 }],
    ]);
  });

  it("stops looking for sites when a batch settles no organization", async () => {
    await renderPage();
    await userEvent.click(screen.getByRole("button", { name: "Trova i siti di Fonte: OSM" }));
    await waitFor(() =>
      expect(toastSuccess).toHaveBeenCalledWith(
        "Fonte: OSM: 3 siti trovati da leggere · 1 ancora senza sito",
      ),
    );
    expect(invoke).toHaveBeenCalledWith("find_seed_sites", { listName: "Fonte: OSM", limit: 200 });
    expect(invoke.mock.calls.filter(([command]) => command === "find_seed_sites")).toHaveLength(1);
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

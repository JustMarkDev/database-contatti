import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { LeadSource, SearchLeadList, SearchLeadView } from "../types";
import { formatMethod, LEAD_COLUMNS, SearchPage, tabFilters } from "./SearchPage";

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

const source = (overrides: Partial<LeadSource>): LeadSource => ({
  id: "x",
  name: "x",
  publisher: "Editore",
  landing_url: "https://example.test/",
  license: "CC0",
  coverage: [],
  emails: "si",
  importable: true,
  note: "",
  kind: "dataset",
  category: null,
  subcategory: null,
  ...overrides,
});

const catalog: LeadSource[] = [
  source({ id: "dir-cai", name: "CAI sezioni", kind: "directory", category: "Associazioni" }),
  source({ id: "miur-statali", name: "Scuole statali" }),
  source({ id: "wikidata-religiosi", name: "Wikidata diocesi", emails: "no" }),
];

const high = makeLead("high");
const low = makeLead("low");

function defaultInvoke(command: string): Promise<unknown> {
  switch (command) {
    case "get_tinyfish_api_key":
    case "set_tinyfish_api_key":
      return Promise.resolve({ configured: true, from_env: false });
    case "find_seed_urls":
      return Promise.resolve([
        "https://www.scuola.example.test/",
        "https://ic.example.test/bullismo",
      ]);
    case "list_lead_sources":
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
        { list_name: "Fonte: OSM", total: 5, pending: 3, crawled: 1, failed: 0, without_site: 1 },
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
    case "import_lead_source":
      return Promise.resolve({ ...run, run_id: "run-src", page_count: 0 });
    case "start_search_run":
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

async function renderPage(tab: "Dati aperti" | "Ricerca web" = "Dati aperti") {
  render(<SearchPage active={true} onContactsChanged={onContactsChanged} />);
  await screen.findAllByText("high@example.test");
  if (tab === "Ricerca web") {
    await userEvent.click(screen.getByRole("tab", { name: /Ricerca web/ }));
  }
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
      filters: tabFilters("open"),
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

  it("imports a dataset and narrows the table to that run", async () => {
    await renderPage();
    const names = screen
      .getAllByRole("listitem")
      .map((item) => item.textContent ?? "")
      .filter((text) => text.includes("Editore"));
    expect(names).toHaveLength(1);
    expect(names[0]).toContain("Scuole statali");
    expect(names.join()).not.toContain("CAI sezioni");
    expect(screen.queryByLabelText("Descrizione della ricerca lead")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Importa Scuole statali" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("import_lead_source", { sourceId: "miur-statali" }),
    );
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("list_search_leads", {
        filters: { ...tabFilters("open"), run: { include: ["run-src"], exclude: [] } },
        limit: 100,
        offset: 0,
      }),
    );
    expect(toastSuccess).toHaveBeenCalledWith("Scuole statali: 2 lead");
  });

  it("reads the chosen public sites with the IA, chaining each list's sites", async () => {
    await renderPage("Ricerca web");
    const start = screen.getByRole("button", { name: "Leggi i siti scelti" });
    expect(start).toBeDisabled();
    await userEvent.click(
      within(screen.getByText("CAI sezioni").closest("li")!).getByRole("checkbox"),
    );
    await userEvent.click(
      within(screen.getByText("Wikidata diocesi").closest("li")!).getByRole("checkbox"),
    );
    await userEvent.click(
      within(screen.getByText("Fonte: OSM").closest("li")!).getByRole("checkbox"),
    );
    await userEvent.click(start);
    const exploreCalls = () =>
      invoke.mock.calls.filter(([command]) => command === "explore_seeds").map(([, args]) => args);
    await waitFor(() => expect(exploreCalls()).toHaveLength(3));
    expect(invoke).toHaveBeenCalledWith("explore_directory", {
      request: { sourceId: "dir-cai", useAi: true },
    });
    expect(invoke).toHaveBeenCalledWith("import_lead_source", { sourceId: "wikidata-religiosi" });
    const base = { limit: 50, maxPages: 10, maxPdfs: 5, useAi: true };
    expect(exploreCalls()).toEqual([
      { request: { ...base, listName: "Fonte: Wikidata diocesi" } },
      { request: { ...base, listName: "Elenco: CAI sezioni" } },
      { request: { ...base, listName: "Fonte: OSM" } },
    ]);
  });

  it("keeps reading the other chosen sites when one of them fails", async () => {
    invoke.mockImplementation((command: string) =>
      command === "explore_directory" ? Promise.reject("HTTP 500") : defaultInvoke(command),
    );
    await renderPage("Ricerca web");
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
    await renderPage("Ricerca web");
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
        },
      });
    });
    expect(await screen.findByText("2 di 5 siti, 7 lead · a.example.test")).toBeInTheDocument();
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
    source_ids: ["miur-statali", "dir-cai"],
    web_query: "pro loco Lombardia email",
  };

  it("turns a free-form request into sources, collected sites and a web search", async () => {
    invoke.mockImplementation((command: string) =>
      command === "plan_lead_request" ? Promise.resolve(plan) : defaultInvoke(command),
    );
    await renderPage("Ricerca web");
    await userEvent.type(
      screen.getByLabelText("Descrizione della ricerca lead"),
      "tutte le pro loco della Lombardia",
    );
    await userEvent.click(screen.getByRole("button", { name: "Cerca" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("start_search_run", {
        query: "tutte le pro loco della Lombardia",
        seedUrls: ["https://www.scuola.example.test/", "https://ic.example.test/bullismo"],
        maxPagesPerDomain: 20,
        maxPdfs: 5,
        useAi: true,
      }),
    );
    expect(invoke).toHaveBeenCalledWith("plan_lead_request", {
      request: "tutte le pro loco della Lombardia",
    });
    expect(invoke).toHaveBeenCalledWith("import_lead_source", { sourceId: "miur-statali" });
    expect(invoke).toHaveBeenCalledWith("explore_directory", {
      request: { sourceId: "dir-cai", useAi: true },
    });
    expect(invoke).toHaveBeenCalledWith("explore_seeds", {
      request: {
        listName: null,
        category: "Turismo e promozione territoriale",
        subcategory: "Pro Loco",
        regione: "Lombardia",
        limit: 50,
        maxPages: 10,
        maxPdfs: 5,
        useAi: true,
      },
    });
    expect(invoke).toHaveBeenCalledWith("find_seed_urls", { query: "pro loco Lombardia email" });
  });

  it("still runs the planned sources without a TinyFish key and offers the key form", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "get_tinyfish_api_key") {
        return Promise.resolve({ configured: false, from_env: false });
      }
      return command === "plan_lead_request" ? Promise.resolve(plan) : defaultInvoke(command);
    });
    await renderPage("Ricerca web");
    await userEvent.type(screen.getByLabelText("Descrizione della ricerca lead"), "pro loco");
    await userEvent.click(screen.getByRole("button", { name: "Cerca" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("import_lead_source", { sourceId: "miur-statali" }),
    );
    expect(invoke).not.toHaveBeenCalledWith("find_seed_urls", expect.anything());
    await userEvent.type(screen.getByLabelText("Chiave API TinyFish"), " tf-key ");
    await userEvent.click(screen.getByRole("button", { name: "Salva chiave" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("set_tinyfish_api_key", { apiKey: "tf-key" }),
    );
  });

  it("warns about the tables of an import that did not load", async () => {
    invoke.mockImplementation((command: string) =>
      command === "import_lead_source"
        ? Promise.resolve({ ...run, run_id: "run-src", errors: ["albo-x: HTTP 500"] })
        : defaultInvoke(command),
    );
    await renderPage();
    await userEvent.click(screen.getByRole("button", { name: "Importa Scuole statali" }));
    await waitFor(() =>
      expect(toastWarning).toHaveBeenCalledWith("Tabelle non caricate: albo-x: HTTP 500"),
    );
  });

  it("imports every open-data registry with one click", async () => {
    await renderPage();
    await userEvent.click(screen.getByRole("button", { name: "Importa tutto" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("import_lead_source", { sourceId: "miur-statali" }),
    );
    expect(invoke).not.toHaveBeenCalledWith("import_lead_source", {
      sourceId: "wikidata-religiosi",
    });
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
      expect(invoke).toHaveBeenCalledWith("promote_search_leads", { filters: tabFilters("open") }),
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

  it("keeps the two tabs apart: each lists and promotes only its own runs", async () => {
    await renderPage("Ricerca web");
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("list_search_leads", {
        filters: tabFilters("web"),
        limit: 100,
        offset: 0,
      }),
    );
    expect(invoke).toHaveBeenCalledWith("get_lead_filter_options", {
      kinds: ["web", "directory", "crawl"],
    });
    expect(
      screen.queryByRole("button", { name: "Importa Scuole statali" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("Fonte: OSM")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Aggiungi al database" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("promote_search_leads", { filters: tabFilters("web") }),
    );
  });

  it("formats extraction methods in words", () => {
    expect(formatMethod("open_data")).toBe("Dati aperti");
    expect(formatMethod("tinyfish+ia")).toBe("Sito (TinyFish) + IA");
    expect(formatMethod(null)).toBe("—");
  });
});

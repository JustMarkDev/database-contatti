import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(undefined)) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(), save: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.unstubAllEnvs();
  vi.resetModules();
});

describe("release build gate", () => {
  it.each(["Verifica caselle", "Cerca lead"])(
    "shows 'Lavori in corso' instead of %s outside dev",
    async (title) => {
      vi.stubEnv("DEV", false);
      vi.resetModules();
      const { default: App } = await import("./App");
      render(<App />);

      await userEvent.click(screen.getByTitle(title));
      const island = await screen.findByRole("region", { name: title });
      expect(island).toHaveTextContent("Lavori in corso");
    },
  );
});

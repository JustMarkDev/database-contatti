import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const { listen, open, save } = vi.hoisted(() => ({
  listen: vi.fn(),
  open: vi.fn(),
  save: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open, save }));

import { platform } from "./platform";

describe("typed platform boundary", () => {
  beforeEach(() => vi.clearAllMocks());

  it("delegates event subscriptions and file dialogs through one adapter", async () => {
    const unlisten = vi.fn();
    listen.mockResolvedValue(unlisten);
    open.mockResolvedValue(["contacts.csv"]);
    save.mockResolvedValue("export.csv");
    const callback = vi.fn();

    await expect(platform.listen("ai-progress", callback)).resolves.toBe(unlisten);
    await expect(platform.openFiles({ multiple: true })).resolves.toEqual(["contacts.csv"]);
    await expect(platform.saveFile({ defaultPath: "export.csv" })).resolves.toBe("export.csv");
    expect(listen).toHaveBeenCalledWith("ai-progress", callback);
  });
});

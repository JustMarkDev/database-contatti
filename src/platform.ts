import { listen as tauriListen, type EventCallback, type UnlistenFn } from "@tauri-apps/api/event";
import { open as tauriOpen, save as tauriSave } from "@tauri-apps/plugin-dialog";

export const platform = {
  listen<T>(event: string, callback: EventCallback<T>): Promise<UnlistenFn> {
    return tauriListen(event, callback);
  },
  openFiles(options: Parameters<typeof tauriOpen>[0]) {
    return tauriOpen(options);
  },
  saveFile(options: Parameters<typeof tauriSave>[0]) {
    return tauriSave(options);
  },
};

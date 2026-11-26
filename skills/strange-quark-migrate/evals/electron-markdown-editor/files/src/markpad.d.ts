export {};
declare global {
  interface Window {
    markpad: {
      openFile(): Promise<{ path: string; text: string } | null>;
      saveFile(doc: { path: string | null; text: string }): Promise<string | null>;
      recentFiles(): Promise<string[]>;
      openLink(url: string): Promise<void>;
      onMenu(cb: (action: "open" | "save") => void): void;
    };
  }
}

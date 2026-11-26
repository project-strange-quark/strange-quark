import { useEffect, useMemo, useState } from "react";
import { marked } from "marked";

export function App() {
  const [doc, setDoc] = useState<{ path: string | null; text: string }>({ path: null, text: "# Hello\n\nStart typing **markdown**." });
  const [recent, setRecent] = useState<string[]>([]);
  const html = useMemo(() => marked.parse(doc.text) as string, [doc.text]);

  const open = async () => {
    const file = await window.markpad.openFile();
    if (file) setDoc(file);
    setRecent(await window.markpad.recentFiles());
  };
  const save = async () => {
    const saved = await window.markpad.saveFile(doc);
    if (saved) setDoc((d) => ({ ...d, path: saved }));
  };

  useEffect(() => {
    window.markpad.recentFiles().then(setRecent);
    window.markpad.onMenu((a) => (a === "open" ? open() : save()));
  }, []);

  return (
    <div className="layout">
      <aside>
        <button onClick={open}>Open</button>
        <button onClick={save}>Save</button>
        <h3>Recent</h3>
        <ul>{recent.map((p) => <li key={p}>{p.split("/").pop()}</li>)}</ul>
      </aside>
      <textarea value={doc.text} onChange={(e) => setDoc({ ...doc, text: e.target.value })} />
      <section
        className="preview"
        dangerouslySetInnerHTML={{ __html: html }}
        onClick={(e) => {
          const a = (e.target as HTMLElement).closest("a");
          if (a) { e.preventDefault(); window.markpad.openLink(a.href); }
        }}
      />
    </div>
  );
}

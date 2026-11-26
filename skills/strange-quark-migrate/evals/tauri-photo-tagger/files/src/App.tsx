import { useEffect, useState } from "react";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { load } from "@tauri-apps/plugin-store";

type Photo = { path: string; width: number; height: number; tags: string[] };

export function App() {
  const [folder, setFolder] = useState<string | null>(null);
  const [photos, setPhotos] = useState<Photo[]>([]);
  const [filter, setFilter] = useState("");

  useEffect(() => {
    load("prefs.json").then(async (s) => setFolder((await s.get<string>("folder")) ?? null));
  }, []);
  useEffect(() => {
    if (folder) invoke<Photo[]>("scan_folder", { folder }).then(setPhotos);
  }, [folder]);

  const pick = async () => {
    const dir = await open({ directory: true });
    if (typeof dir === "string") {
      setFolder(dir);
      (await load("prefs.json")).set("folder", dir);
    }
  };
  const addTag = async (p: Photo, tag: string) => {
    const tags = await invoke<string[]>("add_tag", { path: p.path, tag });
    setPhotos((ps) => ps.map((x) => (x.path === p.path ? { ...x, tags } : x)));
  };

  const shown = photos.filter((p) => !filter || p.tags.some((t) => t.includes(filter)));
  return (
    <div className="app">
      <header>
        <button onClick={pick}>{folder ?? "Choose a photo folder"}</button>
        <input placeholder="Filter by tag" value={filter} onChange={(e) => setFilter(e.target.value)} />
      </header>
      <div className="grid">
        {shown.map((p) => (
          <figure key={p.path}>
            <img src={convertFileSrc(p.path)} width={p.width} height={p.height} loading="lazy" />
            <figcaption>
              {p.tags.map((t) => <span key={t} className="tag">{t}</span>)}
              <form onSubmit={(e) => { e.preventDefault(); const f = e.currentTarget; addTag(p, f.tag.value); f.reset(); }}>
                <input name="tag" placeholder="+ tag" />
              </form>
            </figcaption>
          </figure>
        ))}
      </div>
    </div>
  );
}

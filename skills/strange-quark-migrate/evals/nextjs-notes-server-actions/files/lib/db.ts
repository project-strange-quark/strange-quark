import "server-only";
import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";

export type Note = { id: string; title: string; body: string; author: string };
const file = path.join(process.cwd(), "data", "notes.json");

export async function listNotes(): Promise<Note[]> {
  return JSON.parse(await readFile(file, "utf8"));
}
export async function getNote(id: string) {
  return (await listNotes()).find((n) => n.id === id) ?? null;
}
export async function insertNote(title: string, body: string) {
  const notes = await listNotes();
  const note = { id: String(Date.now()), title, body, author: "me" };
  await writeFile(file, JSON.stringify([note, ...notes], null, 2));
  return note;
}

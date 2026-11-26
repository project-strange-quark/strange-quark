import Link from "next/link";
import { listNotes } from "@/lib/db";
import { NoteForm } from "@/components/NoteForm";

export const revalidate = 0;

export default async function Home() {
  const notes = await listNotes();
  return (
    <>
      <NoteForm />
      {notes.map((n) => (
        <article key={n.id} className="note">
          <h2><Link href={`/notes/${n.id}`}>{n.title}</Link></h2>
          <p>{n.body}</p>
        </article>
      ))}
    </>
  );
}

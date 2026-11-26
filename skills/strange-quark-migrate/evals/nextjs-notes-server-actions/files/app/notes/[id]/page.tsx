import Image from "next/image";
import { notFound } from "next/navigation";
import { getNote } from "@/lib/db";

export default async function NotePage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  const note = await getNote(id);
  if (!note) notFound();
  return (
    <article className="note">
      <Image src={`https://avatars.example.com/${note.author}.png`} alt="" width={32} height={32} />
      <h2>{note.title}</h2>
      <p>{note.body}</p>
    </article>
  );
}

"use client";
import { useActionState } from "react";
import { addNote } from "@/app/actions";

export function NoteForm() {
  const [error, action, pending] = useActionState(addNote, null);
  return (
    <form action={action}>
      <input name="title" placeholder="Title" />
      <textarea name="body" placeholder="Write something" rows={3} />
      {error && <p role="alert">{error}</p>}
      <button disabled={pending}>{pending ? "Saving..." : "Add note"}</button>
    </form>
  );
}

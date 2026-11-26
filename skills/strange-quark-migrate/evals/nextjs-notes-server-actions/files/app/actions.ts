"use server";
import { revalidatePath } from "next/cache";
import { insertNote } from "@/lib/db";

export async function addNote(_prev: string | null, form: FormData) {
  const title = String(form.get("title") ?? "").trim();
  if (!title) return "A title is required";
  await insertNote(title, String(form.get("body") ?? ""));
  revalidatePath("/");
  return null;
}

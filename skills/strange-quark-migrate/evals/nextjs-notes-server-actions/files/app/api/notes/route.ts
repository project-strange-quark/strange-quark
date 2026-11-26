import { NextResponse } from "next/server";
import { listNotes } from "@/lib/db";

export async function GET() {
  return NextResponse.json(await listNotes());
}

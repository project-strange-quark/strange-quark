import { NextResponse, type NextRequest } from "next/server";
export function middleware(req: NextRequest) {
  if (req.nextUrl.pathname.startsWith("/old")) return NextResponse.redirect(new URL("/", req.url));
}
export const config = { matcher: ["/old/:path*"] };

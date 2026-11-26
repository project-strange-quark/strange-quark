import type { Metadata } from "next";
import { Inter } from "next/font/google";
import Link from "next/link";
import "./globals.css";

const inter = Inter({ subsets: ["latin"] });
export const metadata: Metadata = { title: "QuickNotes" };

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body className={inter.className}>
        <header className="bar"><Link href="/">QuickNotes</Link></header>
        <main>{children}</main>
      </body>
    </html>
  );
}

# Next.js to Strange Quark

Target: a Vite + React single-page app. Routing uses React Router in data
mode (`createBrowserRouter`), or TanStack Router if the app already leans
that way. Keep TypeScript, the path aliases (`vite-tsconfig-paths`, or
`resolve.alias`), Tailwind or CSS Modules, and every component.

Next's static export (`output: 'export'`) does not change this. It still
ships Next's client runtime and router, which Strange Quark does not support,
so migrate it the same way.

## Scaffolding

1. Add `vite`, `@vitejs/plugin-react` and the router. Add a root
   `index.html` with `<div id="root">` and a `src/main.tsx` that calls
   `createRoot`. Change the scripts to `vite`, `vite build` and
   `vite preview`.
2. Remove `next` from the dependencies only once nothing imports it. Grep
   for `from 'next` and `from "next`.
3. Environment variables: `NEXT_PUBLIC_*` becomes `VITE_*`, read through
   `import.meta.env`. Server-only variables move to the `server/` package,
   or disappear with the code that used them.

## Routing

| Next.js | React Router (data mode) |
| --- | --- |
| `app/**/page.tsx` | A route whose `element` is the page component |
| `app/**/layout.tsx` | A layout route that renders `<Outlet />` |
| `loading.tsx` | `<Suspense fallback>` around the outlet, or `HydrateFallback` |
| `error.tsx` | `errorElement` |
| `not-found.tsx` | A `*` route |
| `[id]`, `[...slug]`, `[[...slug]]` | `:id`, `*`, and optional segments |
| `(group)` folders | No path segment; keep them as file organization |
| Parallel routes (`@slot`) and intercepting routes (`(.)`) | No direct equivalent. Nest components or use a modal route, and record it as *behavioural* |
| `pages/**` (Pages Router) | The same table, with `_app.tsx` becoming the root layout route |

APIs:

| Next.js | Replacement |
| --- | --- |
| `next/link` | `Link` from the router (`href` becomes `to`) |
| `useRouter`, `usePathname`, `useSearchParams`, `useParams` | `useNavigate`, `useLocation`, `useSearchParams`, `useParams` |
| `redirect()`, `notFound()` called from server code | A `redirect` or a thrown `Response` in a client loader, or `navigate()` in an effect |
| `next/dynamic` | `React.lazy` with `Suspense`; `ssr: false` has no meaning any more |
| `next/head`, the `metadata` export | React 19 `<title>` and `<meta>` in the component, or `document.title` |
| `next/image` | `<img>` with `width`, `height` and `loading="lazy"`. Remote hosts go in `network.allow` |
| `next/font/google`, `next/font/local` | Self-hosted fonts (`@fontsource/*` or local files) through `@font-face`. Strange Quark does not fetch Google Fonts unless the host is allowlisted, so bundle them |
| `next/script` | A plain `<script>` in `index.html`, or a dynamic import |
| `'use client'` | Delete it; everything is client code now |
| styled-jsx (`<style jsx>`) | No longer available. Convert it to CSS Modules (*mechanical*) |

## Server code

Look at each server construct and decide what happens to it. Record every
decision in `MIGRATION.md`.

- **Server components that only render props or static data:** they become
  ordinary components.
- **Server components that fetch** (from a database, a secret API, the file
  system): the fetch moves behind an HTTP endpoint, and the component
  becomes a client component that calls it, through TanStack Query, a
  router loader or `useEffect`. This is *behavioural*: there is a loading
  state where none existed.
- **Server actions** (`'use server'`): each one becomes a `POST` endpoint,
  and the form posts to it with `fetch`. `useFormStatus` and
  `useActionState` still work on the client when they are given a client
  action that calls `fetch`.
- **Route handlers** (`app/**/route.ts`, `pages/api/**`): move them into
  `server/`, a small Hono or Express app. Their logic usually moves
  unchanged.
- **`middleware.ts`:** auth redirects become client route guards, plus auth
  checks in the server endpoints. Rewrites, geo and headers usually have no
  client equivalent. Flag them.
- **`getServerSideProps` and `getStaticProps`:** server data becomes client
  fetches. Data that is truly static can become a JSON import at build
  time.
- **ISR, `revalidate`, caching, `unstable_cache`:** these have no meaning
  without a server. Drop them, and record it as *behavioural*.

When `server/` exists, the native app needs it running somewhere. Put its
URL in `network.allow`, and say plainly in the report that the Strange Quark
build is a client of that server, not a standalone app. If extracting a
server would be a large piece of work, stub the calls instead. The verdict
then counts them as stubs.

## Checks particular to Next apps

- **Auth libraries** (NextAuth or Auth.js, Clerk's Next SDK) are tied to
  Next's server. Usually this is the biggest *behavioural* change, and
  often a blocker. Look for a client SDK first.
- **`next-intl` and similar** libraries have client-only modes. Switch to
  them.
- **Image optimization:** Strange Quark shows the original image. Very large
  originals cost memory and decode time. Say so if you measure it.

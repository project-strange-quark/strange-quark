# Remix, or React Router framework mode, to Strange Quark

React Router v7 framework mode is Remix's successor. It uses
`@react-router/dev`, a `react-router.config.ts` and `app/routes.ts`. Even
with `ssr: false`, it builds through the framework's Vite plugin and
runtime. Migrate it to **data mode**: `createBrowserRouter` and
`RouterProvider`, with the `react-router` package alone. Remix v2 apps follow
the same path; `@remix-run/react` imports become `react-router` imports.

## Steps

1. **Replace the framework plugin with plain Vite.**
   - Remove `reactRouter()` from `vite.config.ts` and add
     `@vitejs/plugin-react`.
   - Delete `react-router.config.ts`, `entry.server.tsx`, and
     `entry.client.tsx` once its contents have moved.
   - Add `index.html` and `main.tsx`.
2. **Turn the route config into route objects.** `app/routes.ts`, or the
   flat-route file convention, becomes an array passed to
   `createBrowserRouter`. Each route module's default export is the
   `Component` (or `element`); its `ErrorBoundary` is the `errorElement`;
   `HydrateFallback` stays.
3. **Server `loader`s and `action`s.**
   - When the route module also has `clientLoader` or `clientAction`, keep
     those.
   - Otherwise move the server logic to `server/` endpoints, and write a
     client `loader` or `action` that calls them with `fetch`.
   - `useLoaderData`, `useActionData`, `useFetcher` and `<Form>` keep
     working in data mode against client loaders and actions.
4. **`links`, `meta` and `headers` exports.**
   - `meta` becomes React 19 `<title>` and `<meta>`.
   - `links` stylesheets become CSS imports.
   - `headers` has no meaning without a server, so drop it (*behavioural*).
5. **`root.tsx`:** the `<html>`, `<head>` and `<body>` it renders move to
   `index.html`. `Layout` and `App` become the root route's component.
   `<Scripts />`, `<ScrollRestoration />` (keep its data-mode version) and
   `<Links />` are removed.
6. **Sessions and cookies** (`createCookieSessionStorage`): Strange Quark has
   no cookies. Auth has to move to tokens the client keeps, in memory or in
   `localStorage`, and sends to `server/`. Usually this is the largest
   *behavioural* change. Say so in the report.

`Link`, `NavLink`, `useNavigate`, `useParams`, `useSearchParams`,
`Outlet` and `useNavigation` stay the same.

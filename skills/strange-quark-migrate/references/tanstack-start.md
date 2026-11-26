# TanStack Start to Strange Quark

TanStack Start is TanStack Router plus a server: server functions, SSR,
and API routes. The router itself works in a client-rendered app, so this
migration is mostly about removing the server.

## Steps

1. **Make TanStack Router a single-page app.**
   - Keep the file-based routes, through `@tanstack/router-plugin/vite`.
   - Replace Start's `app.config.ts` or `vite.config.ts` integration with a
     plain Vite config that uses the router plugin.
   - Add `index.html` and a `main.tsx` that calls `createRoot` and renders
     `<RouterProvider router={router} />`.
   - Remove the server entry and the SSR entry (`ssr.tsx`, `client.tsx`,
     `createStartHandler`), and `<Scripts />`, `<Meta />` and
     `HeadContent` from the root route, keeping `<Outlet />`.
2. **Server functions (`createServerFn`).** Each becomes an HTTP endpoint
   in `server/`, and the call site becomes a `fetch` wrapper with the same
   signature. Validators (Zod) can be shared from a common module.
   Middleware attached to server functions moves to the server.
3. **API routes** (`createAPIFileRoute`, `server.handlers`): move them to
   `server/`.
4. **Loaders.**
   - Loaders that only call server functions keep working, because the
     server functions become `fetch` calls.
   - Loaders that touched server-only resources directly have to go
     through the new endpoints.
   - `beforeLoad` auth checks stay, as client guards.
5. **`head()` route options:** use React 19 `<title>` and `<meta>`, or
   `document.title`.
6. **`createIsomorphicFn` and `serverOnly`:** keep the client branch, and
   move the server branch to `server/` or drop it.

Static data, search params, `Link`, `useNavigate` and route context all
behave the same. Those changes are *mechanical*.

When `server/` exists, the report must say the native app depends on it.

// SPDX-License-Identifier: MIT

import { createServer } from "vite";
import { createServer as reservePort } from "node:net";

const address = reservePort();
await new Promise(resolve => address.listen(0, "127.0.0.1", resolve));
const { port } = address.address();
await new Promise(resolve => address.close(resolve));

let attempts = 0;
let sseConnections = 0;
const developmentModules = new Map([
  ["/dev-entry.js", `
    import { createHotContext } from "/@vite/client";
    import { left } from "./dev-left.js";
    import { right } from "./dev-right.js";
    import { packed } from "./dev-packed.js";
    const hot = createHotContext("/dev-entry.js");
    hot.accept(next => { globalThis.update = [next.version, next.url]; });
    hot.on("vite:ws:connect", () => hot.send("squark:request", 41));
    hot.on("squark:response", data => { globalThis.hmrResult = data; });
    globalThis.devEntry = [left(), right(), packed];
  `],
  ["/dev-left.js", `
    import { shared } from "./dev-shared.js";
    export function left() { return shared + 1; }
  `],
  ["/dev-right.js", `
    import { shared } from "./dev-shared.js";
    export function right() { return shared + 2; }
  `],
  ["/dev-shared.js", `
    import { left } from "./dev-left.js";
    export const shared = typeof left === "function" ? 40 : 0;
    globalThis.devSharedEvaluations = (globalThis.devSharedEvaluations ?? 0) + 1;
  `],
  ["/dev-entry.js?t=1", 'export const version = 1, url = import.meta.url;'],
  ["/dev-entry.js?t=2", 'export const version = 2, url = import.meta.url;'],
]);

const vite = await createServer({
  configFile: false,
  logLevel: "silent",
  root: process.cwd(),
  server: { host: "127.0.0.1", watch: null, port, strictPort: true },
  plugins: [{
    name: "squark-network-fixture",
    configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        const module = developmentModules.get(req.url);
        if (module !== undefined) {
          res.writeHead(200, { "Content-Type": "text/javascript" }).end(module);
          return;
        }
        if (req.url.startsWith("/parallel?request=")) {
          const id = req.url.split("=")[1];
          res.writeHead(200, { "Content-Type": "text/plain" });
          res.write(id + ":");
          setImmediate(() => res.end("complete"));
          return;
        }
        switch (req.url) {
          case "/invalid-module.js":
            res.writeHead(200, { "Content-Type": "text/html" }).end("<p>not a module</p>");
            break;
          case "/echo": {
            const body = [];
            for await (const chunk of req) body.push(chunk);
            res.setHeader("Content-Type", "application/json");
            res.setHeader("X-Duplicate", ["first", "second"]);
            res.end(JSON.stringify({ method: req.method, body: Buffer.concat(body).toString() }));
            break;
          }
          case "/redirect-local":
            res.writeHead(303, { location: "/echo" }).end();
            break;
          case "/redirect-blocked":
            res.writeHead(302, { location: `http://localhost:${req.socket.localPort}/forbidden` }).end();
            break;
          case "/forbidden":
            attempts++;
            res.end("unexpected request");
            break;
          case "/classic":
            res.writeHead(200, { "Content-Type": "text/javascript" });
            res.end('startupEvents.push("ran")');
            break;
          case "/attempts":
            res.end(String(attempts));
            break;
          case "/abort-stream":
            res.writeHead(200, { "Content-Type": "text/plain" });
            res.write("open");
            break;
          case "/events":
            sseConnections++;
            if (sseConnections === 1) {
              res.writeHead(200, { "Content-Type": "text/event-stream; charset=utf-8" });
              const event = Buffer.from("\ufeffid: 7\r\nretry: 1\r\nevent: ping\r\ndata: café\r\ndata: second\r\n\r\n");
              const split = event.indexOf(0xc3) + 1;
              res.write(event.subarray(0, split));
              setTimeout(() => res.end(event.subarray(split)), 20);
            } else {
              res.writeHead(200, { "Content-Type": "text/event-stream" });
              res.end(`data: ${req.headers["last-event-id"]}\n\n`);
            }
            break;
          default:
            next();
        }
      });
      server.ws.on("squark:request", (data, client) => client.send("squark:response", data + 1));
      server.ws.on("squark:update", (_, client) => client.send({
        type: "update",
        updates: [{
          type: "js-update",
          path: "/dev-entry.js",
          acceptedPath: "/dev-entry.js",
          timestamp: 2,
        }],
      }));
    },
  }],
});
await vite.listen();
process.stdout.write(port + "\n");
process.stdin.resume();
process.stdin.on("end", async () => {
  await vite.close();
});

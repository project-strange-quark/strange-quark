// SPDX-License-Identifier: MIT
// The Vite majors @strange-quark/vite-plugin supports, each with the React
// plugins create-vite's templates use, and a helper that lays out a test app
// the way npm installs a user's app.
//
// Vite 6 and its plugins are installed at the repository root (the root
// package.json pins @vitejs/plugin-react 4.7.0 there; the apps use the SWC
// plugin). Vite 7 and 8 live in the fixture workspaces tests/vite-7 and
// tests/vite-8, where an app resolves that fixture's packages first, as a
// user's app resolves its own. Vite 8's @vitejs/plugin-react 6 handles both
// templates, so it has one entry.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const repoRoot = path.resolve(__dirname, "../../..");

export const VITE_MATRIX = [
  {
    vite: "6.4.6",
    react: { name: "@vitejs/plugin-react", version: "4.7.0" },
    home: path.join(repoRoot, "target/squark-dev-test"),
  },
  {
    vite: "6.4.6",
    react: { name: "@vitejs/plugin-react-swc", version: "4.2.2" },
    home: path.join(repoRoot, "target/squark-dev-test"),
  },
  {
    vite: "7.3.8",
    react: { name: "@vitejs/plugin-react", version: "5.2.0" },
    home: path.join(repoRoot, "tests/vite-7"),
  },
  {
    vite: "7.3.8",
    react: { name: "@vitejs/plugin-react-swc", version: "4.3.3" },
    home: path.join(repoRoot, "tests/vite-7"),
  },
  {
    vite: "8.4.0",
    react: { name: "@vitejs/plugin-react", version: "6.3.0" },
    home: path.join(repoRoot, "tests/vite-8"),
  },
];

/** The directory Node's resolution finds package `name` in from `fromDir`, or null. */
export function packageDirFrom(name, fromDir) {
  for (let dir = fromDir; ; dir = path.dirname(dir)) {
    const candidate = path.join(dir, "node_modules", name);
    if (fs.existsSync(path.join(candidate, "package.json"))) return fs.realpathSync(candidate);
    if (path.dirname(dir) === dir) return null;
  }
}

export function packageVersion(dir) {
  return JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8")).version;
}

/**
 * Creates `apps/counter` with a stock config for the fixture's React plugin
 * inside the fixture `home`, and installs the built plugin into it as a copy,
 * so its `import "vite"` finds the fixture's Vite rather than the workspace's.
 *
 * Throws unless the plugin and the React plugin both load the expected Vite:
 * the React plugins read `vite.version` to pick their transforms, so a
 * hoisted copy would test the wrong pairing.
 */
export function createFixtureApp({ vite, react, home }, label = "app") {
  const id = `${label}-${react.name.replace(/^@vitejs\//, "")}-${process.pid}`;
  const appDir = path.join(home, `.tmp-${id}`);
  const workDir = path.join(home, `.tmp-${id}-work`);
  const cleanup = () => {
    fs.rmSync(appDir, { recursive: true, force: true });
    fs.rmSync(workDir, { recursive: true, force: true });
  };
  cleanup();
  try {
    fs.cpSync(path.join(repoRoot, "apps/counter"), appDir, {
      recursive: true,
      filter: (src) => !["node_modules", "dist", "target"].includes(path.basename(src)),
    });
    fs.writeFileSync(
      path.join(appDir, "vite.config.ts"),
      'import { defineConfig } from "vite";\n' +
        `import react from "${react.name}";\n\n` +
        "export default defineConfig({ plugins: [react()] });\n",
    );

    const pluginSource = path.join(repoRoot, "packages/@strange-quark/vite-plugin");
    const plugin = path.join(appDir, "node_modules/@strange-quark/vite-plugin");
    fs.mkdirSync(plugin, { recursive: true });
    fs.copyFileSync(path.join(pluginSource, "package.json"), path.join(plugin, "package.json"));
    fs.cpSync(path.join(pluginSource, "dist"), path.join(plugin, "dist"), { recursive: true });

    const found = {
      pluginVite: versionFrom("vite", path.join(plugin, "dist")),
      react: versionFrom(react.name, appDir),
    };
    const reactDir = packageDirFrom(react.name, appDir);
    found.reactVite = reactDir ? versionFrom("vite", reactDir) : null;
    if (found.pluginVite !== vite || found.react !== react.version || found.reactVite !== vite) {
      throw new Error(
        `fixture ${home} resolves the wrong packages: expected Vite ${vite} and ` +
          `${react.name} ${react.version}, found ${JSON.stringify(found)}`,
      );
    }
    return { appDir, workDir, plugin, cleanup };
  } catch (error) {
    cleanup();
    throw error;
  }
}

function versionFrom(name, fromDir) {
  const dir = packageDirFrom(name, fromDir);
  return dir ? packageVersion(dir) : null;
}

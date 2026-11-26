// SPDX-License-Identifier: MIT
// Excalidraw packaging check: verify and measure the standalone Excalidraw binary.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

import { SquarkDriver } from "../../compat/src/squark-driver.mjs";
import { STATE_PROBE } from "./flow/state.mjs";
import {
  buildDir,
  buildWithYarn,
  EXCALIDRAW_D2_ENV,
  gpuAdapter,
  launchable,
  packExcalidraw,
  repoRoot,
  resolveRuntime,
  submoduleStatus,
} from "./build.mjs";
import { readPe } from "./pe.mjs";
import { platformSuffix, repoRelative, scratchTempDir } from "./platform.mjs";

const isMac = process.platform === "darwin";
const isWindows = process.platform === "win32";
// --skip-app-build packs the existing vendor/excalidraw/excalidraw-app/build
// instead of rebuilding it with yarn, for machines with the built web output
// but no installed submodule.
const skipAppBuild = process.argv.includes("--skip-app-build");
const exe = isWindows ? ".exe" : "";
const resultPath = path.join(
  repoRoot,
  "tests/results/excalidraw",
  `packaging${platformSuffix()}.json`,
);
const distDir = path.join(repoRoot, "dist");
const binaryPath = path.join(distDir, "excalidraw");
const runtimePath = path.join(repoRoot, `target/dist/squark-runtime${exe}`);
const sizeLimitBytes = 150 * 1024 * 1024;
// The submodule checkout can be tagless (shallow clone); this is the commit
// v0.18.1 resolves to, the same one .github/workflows/ci.yml pins.
const EXCALIDRAW_V0181_COMMIT = "a2ec2889babf7d2295469c6d90ebe77fae57df84";
let artifact = null;

function excalidrawTagCommit() {
  try {
    return run("git", ["-C", "vendor/excalidraw", "rev-parse", "--verify", "v0.18.1^{commit}"]).trim();
  } catch {
    return EXCALIDRAW_V0181_COMMIT;
  }
}
const commonSystemPackages = new Set([
  "bzip2",
  "brotli",
  "dbus",
  "expat",
  "fontconfig",
  "freetype2",
  "gcc-libs",
  "glib2",
  "glibc",
  "graphite",
  "harfbuzz",
  "libdrm",
  "libffi",
  "libglvnd",
  "libgcc",
  "libice",
  "libpng",
  "libsm",
  "libx11",
  "libxau",
  "libxcb",
  "libxcomposite",
  "libxcursor",
  "libxdamage",
  "libxdmcp",
  "libxext",
  "libxfixes",
  "libxi",
  "libxinerama",
  "libxkbcommon",
  "libxrandr",
  "libxrender",
  "libxshmfence",
  "libxxf86vm",
  "mesa",
  "pcre2",
  "systemd-libs",
  "vulkan-icd-loader",
  "wayland",
  "zlib",
]);

const report = {
  schemaVersion: 1,
  createdAt: new Date().toISOString(),
  expectedTag: "v0.18.1",
  submodule: {
    commit: null,
    tagCommit: null,
    statusBefore: null,
    statusAfter: null,
    clean: false,
    matchesTag: false,
  },
  build: {
    command: "squark build vendor/excalidraw/excalidraw-app --skip-app-build --dist vendor/excalidraw/excalidraw-app/build --runtime target/dist/squark-runtime --config apps/excalidraw/squark.config.ts --out dist/excalidraw",
    appBuildCommand: skipAppBuild
      ? "skipped (--skip-app-build): packed the existing vendor/excalidraw/excalidraw-app/build"
      : "npx --yes yarn@1.22.22 --ignore-engines --cwd vendor/excalidraw/excalidraw-app vite build",
    runtimeBuildCommand: "cargo build --profile dist -p squark-host --bin squark-runtime",
    runtimeBuildDurationMs: null,
    appAndPackDurationMs: null,
    totalDurationMs: null,
    environment: EXCALIDRAW_D2_ENV,
    collaboration: {
      enabled: EXCALIDRAW_D2_ENV.VITE_APP_WS_SERVER_URL !== "",
      websocketUrl: EXCALIDRAW_D2_ENV.VITE_APP_WS_SERVER_URL,
    },
    config: "apps/excalidraw/squark.config.ts",
    runtime: null,
  },
  binary: {
    path: "dist/excalidraw",
    regularFile: false,
    executable: false,
    fileType: null,
    elfHeader: null,
    class: null,
    type: null,
    machine: null,
    sizeBytes: null,
    sizeMiB: null,
    sizeLimitBytes,
    xz9Bytes: null,
    zstd19Bytes: null,
    debugSections: [],
    stripped: false,
    isolatedDirectoryContents: [],
  },
  libraries: {
    lddOutput: null,
    resolved: [],
    systemOnly: false,
    commonPackagesOnly: false,
    dynamicLoadInfo: null,
  },
  smoke: {
    ran: false,
    copiedBinaryOnly: false,
    appMounted: false,
    domEvalProbe: null,
  },
  checks: {},
  pass: false,
};

function run(command, args, options = {}) {
  return execFileSync(command, args, {
    cwd: repoRoot,
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
    timeout: 300_000,
    ...options,
  });
}

function recordCheck(name, passed, detail = null) {
  report.checks[name] = { pass: passed, ...(detail === null ? {} : { detail }) };
}

async function check(name, action) {
  try {
    const detail = await action();
    recordCheck(name, true, detail);
  } catch (error) {
    recordCheck(name, false, error.message);
  }
}

function parseLdd(output) {
  const resolved = [];
  const missing = [];
  for (const line of output.split("\n")) {
    const arrow = line.match(/^\s*(\S+)\s+=>\s+(\S+)/);
    if (arrow) {
      if (arrow[2] === "not") {
        missing.push(arrow[1]);
      } else {
        resolved.push({ name: arrow[1], path: arrow[2] });
      }
      continue;
    }
    const direct = line.match(/^\s*(\/\S+)\s+\(/);
    if (direct) resolved.push({ name: path.basename(direct[1]), path: direct[1] });
  }
  return { resolved, missing };
}

function isSystemLibrary(filePath) {
  const canonicalPath = fs.realpathSync(filePath);
  return {
    canonicalPath,
    allowed: canonicalPath.startsWith("/usr/lib/") || canonicalPath.startsWith("/lib/"),
  };
}

function dynamicLoadInfo(resolved) {
  const candidates = [
    "libvulkan.so.1",
    "libwayland-client.so.0",
    "libxkbcommon.so.0",
    "libX11.so.6",
  ];
  let linkerCache = "";
  try {
    linkerCache = run("ldconfig", ["-p"]);
  } catch (error) {
    return { note: `Could not inspect ldconfig cache: ${error.message}`, candidates, icds: [] };
  }
  const icdDir = "/usr/share/vulkan/icd.d";
  const icds = fs.existsSync(icdDir)
    ? fs.readdirSync(icdDir).filter((entry) => entry.endsWith(".json")).sort()
    : [];
  return {
    note: "Informational candidates only: ldd shows startup dependencies, not all runtime dlopen calls.",
    candidates: candidates.map((name) => ({
      name,
      inLdd: resolved.some((library) => library.name === name),
      availableOnHost: linkerCache.split("\n").some((line) => line.includes(`${name} `)),
    })),
    vulkanIcdDirectory: icdDir,
    icds,
  };
}

async function runSmoke() {
  const isolatedDir = scratchTempDir(repoRoot, "squark-excalidraw-packaging-");
  const isolatedBinary = path.join(isolatedDir, `excalidraw${exe}`);
  const isolatedBundle = `${isolatedBinary}.app`;
  const isolatedExecutable = isMac
    ? path.join(isolatedBundle, "Contents", "MacOS", "excalidraw")
    : isolatedBinary;
  let driver = null;
  try {
    if (isMac) {
      fs.cpSync(artifact.bundle, isolatedBundle, { recursive: true });
    } else {
      fs.copyFileSync(artifact.binary, isolatedBinary);
      fs.chmodSync(isolatedBinary, 0o755);
    }
    const entries = fs.readdirSync(isolatedDir).sort();
    report.binary.isolatedDirectoryContents = entries;
    const expectedName = isMac ? "excalidraw.app" : `excalidraw${exe}`;
    report.smoke.copiedBinaryOnly = entries.length === 1 && entries[0] === expectedName;
    assert.ok(
      report.smoke.copiedBinaryOnly,
      `unexpected files beside the copied ${expectedName}: ${entries}`,
    );

    driver = await SquarkDriver.launch({
      binaryPath: isolatedExecutable,
      headless: true,
      width: 1280,
      height: 800,
      gpuAdapter: gpuAdapter === "auto" ? null : gpuAdapter,
      timeoutMs: 120_000,
    });
    await driver.waitForSelector(".excalidraw", 60_000);
    const probe = await driver.eval(STATE_PROBE);
    report.smoke.domEvalProbe = probe;
    report.smoke.appMounted = Boolean(probe?.dom?.excalidraw && probe.dom.canvases > 0);
    assert.ok(report.smoke.appMounted, `mount probe failed: ${JSON.stringify(probe)}`);
    report.smoke.ran = true;
    assert.deepEqual(fs.readdirSync(isolatedDir).sort(), [expectedName]);
    return {
      appMounted: report.smoke.appMounted,
      canvases: probe.dom.canvases,
      isolatedDirectoryContents: entries,
    };
  } finally {
    await driver?.close();
    fs.rmSync(isolatedDir, { recursive: true, force: true });
  }
}

await check("Excalidraw tag and cleanliness", async () => {
  report.submodule.commit = run("git", ["-C", "vendor/excalidraw", "rev-parse", "HEAD"]).trim();
  report.submodule.tagCommit = excalidrawTagCommit();
  report.submodule.statusBefore = submoduleStatus();
  report.submodule.clean = report.submodule.statusBefore === "";
  report.submodule.matchesTag = report.submodule.commit === report.submodule.tagCommit;
  assert.ok(report.submodule.clean, `submodule is dirty:\n${report.submodule.statusBefore}`);
  assert.ok(report.submodule.matchesTag, `${report.submodule.commit} does not match v0.18.1 ${report.submodule.tagCommit}`);
  return {
    commit: report.submodule.commit,
    tagCommit: report.submodule.tagCommit,
    status: report.submodule.statusBefore,
  };
});

await check("required build flags and collaboration disabled", async () => {
  assert.equal(EXCALIDRAW_D2_ENV.VITE_APP_DISABLE_SENTRY, "true");
  assert.equal(EXCALIDRAW_D2_ENV.VITE_APP_ENABLE_PWA, "false");
  assert.equal(EXCALIDRAW_D2_ENV.VITE_APP_ENABLE_TRACKING, "false");
  assert.equal(EXCALIDRAW_D2_ENV.VITE_APP_WS_SERVER_URL, "");
  return {
    disableSentry: EXCALIDRAW_D2_ENV.VITE_APP_DISABLE_SENTRY,
    enablePwa: EXCALIDRAW_D2_ENV.VITE_APP_ENABLE_PWA,
    enableTracking: EXCALIDRAW_D2_ENV.VITE_APP_ENABLE_TRACKING,
    collaboration: report.build.collaboration,
  };
});

await check("stripped runtime build and Excalidraw pack", async () => {
  const totalStarted = performance.now();
  const runtimeStarted = performance.now();
  try {
    run("cargo", ["build", "--profile", "dist", "-p", "squark-host", "--bin", "squark-runtime"], {
      encoding: "utf8",
      timeout: 1_800_000,
      maxBuffer: 64 * 1024 * 1024,
    });
  } finally {
    report.build.runtimeBuildDurationMs = Number((performance.now() - runtimeStarted).toFixed(2));
  }
  try {
    const runtime = resolveRuntime();
    assert.equal(runtime.path, runtimePath, `expected stripped runtime at ${runtimePath}; got ${runtime.path}`);
    report.build.runtime = {
      path: repoRelative(repoRoot, runtime.path),
      profile: runtime.profile,
      bytes: runtime.bytes,
    };

    const appAndPackStarted = performance.now();
    try {
      if (skipAppBuild) {
        assert.ok(
          fs.existsSync(path.join(buildDir, "index.html")),
          "--skip-app-build needs a built vendor/excalidraw/excalidraw-app/build",
        );
      } else {
        buildWithYarn();
      }
      const packed = packExcalidraw({ name: "excalidraw", outputDir: distDir });
      artifact = packed;
      assert.equal(packed.binary, launchable(binaryPath, "excalidraw").executable);
      if (isMac) {
        assert.equal(packed.bundle, `${binaryPath}.app`);
      } else {
        assert.equal(packed.binary, `${binaryPath}${exe}`);
        assert.equal(packed.bundle, null);
      }
    } finally {
      report.build.appAndPackDurationMs = Number((performance.now() - appAndPackStarted).toFixed(2));
    }
    return {
      runtime: report.build.runtime,
      runtimeBuildDurationMs: report.build.runtimeBuildDurationMs,
      appAndPackDurationMs: report.build.appAndPackDurationMs,
    };
  } finally {
    report.build.totalDurationMs = Number((performance.now() - totalStarted).toFixed(2));
  }
});

await check(
  isMac
    ? "Mach-O app bundle, code signature and size limits"
    : isWindows
      ? "PE executable, GUI subsystem and size limits"
      : "single x86-64 ELF and stripped-size limits",
  async () => {
    if (isWindows) {
      const stats = fs.lstatSync(artifact.binary);
      const pe = readPe(artifact.binary);
      const sizeBytes = stats.size;
      Object.assign(report.binary, {
        path: repoRelative(repoRoot, artifact.binary),
        regularFile: stats.isFile(),
        executable: pe?.executableImage ?? false,
        fileType: pe ? `${pe.format} ${pe.machine} ${pe.subsystem}` : "not a PE image",
        elfHeader: null,
        class: pe?.format ?? null,
        type: pe?.dll ? "dll" : "executable",
        machine: pe?.machine ?? null,
        subsystem: pe?.subsystem ?? null,
        sizeBytes,
        sizeMiB: Number((sizeBytes / 1024 / 1024).toFixed(2)),
        xz9Bytes: null,
        zstd19Bytes: null,
        debugSections: [],
        stripped: null,
      });
      assert.ok(stats.isFile(), "dist/excalidraw.exe is not a regular file");
      assert.ok(pe, "dist/excalidraw.exe is not a PE image");
      assert.equal(pe.format, "PE32+", `not a 64-bit PE: ${pe.format}`);
      assert.ok(pe.executableImage && !pe.dll, "dist/excalidraw.exe is not an executable image");
      assert.match(pe.machine, /^(ARM64|x86-64)$/, `unexpected PE machine: ${pe.machine}`);
      // Shipped apps are GUI-subsystem (2): no console window opens with them.
      assert.equal(pe.subsystem, "GUI", `the dist runtime's PE subsystem is ${pe.subsystem}, not GUI (2)`);
      assert.ok(sizeBytes < sizeLimitBytes, `binary ${sizeBytes} bytes is not under ${sizeLimitBytes}`);
      return { fileType: report.binary.fileType, machine: pe.machine, subsystem: pe.subsystem, sizeBytes };
    }
    if (isMac) {
      const stats = fs.lstatSync(artifact.binary);
      const fileType = run("file", ["-b", artifact.binary]).trim();
      const plistPath = path.join(artifact.bundle, "Contents", "Info.plist");
      const plistLint = run("plutil", ["-lint", plistPath]).trim().replace(`${plistPath}: `, "");
      // Throws on an invalid or modified signature.
      run("codesign", ["--verify", "--strict", artifact.bundle]);
      const machine = /arm64/.test(fileType)
        ? "arm64"
        : /x86_64/.test(fileType)
          ? "x86_64"
          : null;
      const executableBytes = stats.size;
      const sizeBytes = artifact.bytes;

      Object.assign(report.binary, {
        path: repoRelative(repoRoot, artifact.bundle),
        regularFile: stats.isFile(),
        executable: (stats.mode & 0o111) !== 0,
        executablePath: repoRelative(repoRoot, artifact.binary),
        executableBytes,
        fileType,
        elfHeader: null,
        class: "Mach-O 64-bit",
        type: "executable",
        machine,
        sizeBytes,
        sizeMiB: Number((sizeBytes / 1024 / 1024).toFixed(2)),
        xz9Bytes: null,
        zstd19Bytes: null,
        debugSections: [],
        stripped: null,
        infoPlist: { path: repoRelative(repoRoot, plistPath), lint: plistLint },
        codesign: { strict: true },
      });

      assert.ok(stats.isFile(), "the app bundle executable is not a regular file");
      assert.ok(report.binary.executable, "the app bundle executable is not executable");
      assert.match(fileType, /Mach-O 64-bit/i, `not a 64-bit Mach-O: ${fileType}`);
      assert.match(fileType, /executable/i, `not an executable: ${fileType}`);
      assert.ok(machine, `unexpected Mach-O architecture: ${fileType}`);
      assert.equal(plistLint, "OK", `Info.plist did not parse: ${plistLint}`);
      assert.ok(sizeBytes < sizeLimitBytes, `runtime plus pack ${sizeBytes} bytes is not under ${sizeLimitBytes}`);
      return {
        fileType,
        class: "Mach-O 64-bit",
        type: "executable",
        machine,
        sizeBytes,
        executableBytes,
        infoPlist: plistLint,
        codesign: "valid",
      };
    }

    const stats = fs.lstatSync(binaryPath);
    const fileType = run("file", ["-b", binaryPath]).trim();
    const header = run("readelf", ["-hW", binaryPath]);
    const sections = run("readelf", ["-SW", binaryPath]);
    const elfClass = header.match(/^\s*Class:\s*(.+)$/m)?.[1]?.trim() ?? null;
    const elfType = header.match(/^\s*Type:\s*(.+)$/m)?.[1]?.trim() ?? null;
    const machine = header.match(/^\s*Machine:\s*(.+)$/m)?.[1]?.trim() ?? null;
    const debugSections = [...new Set(sections.match(/\.debug_[A-Za-z0-9_.-]+/g) ?? [])];
    const sizeBytes = stats.size;
    const xz9 = execFileSync("xz", ["-9", "-c", "--", binaryPath], { maxBuffer: 256 * 1024 * 1024 });
    const zstd19 = execFileSync("zstd", ["-q", "-19", "-c", "--", binaryPath], {
      maxBuffer: 256 * 1024 * 1024,
    });

    Object.assign(report.binary, {
      regularFile: stats.isFile(),
      executable: (stats.mode & 0o111) !== 0,
      fileType,
      elfHeader: header.trim(),
      class: elfClass,
      type: elfType,
      machine,
      sizeBytes,
      sizeMiB: Number((sizeBytes / 1024 / 1024).toFixed(2)),
      xz9Bytes: xz9.length,
      zstd19Bytes: zstd19.length,
      debugSections,
      stripped: debugSections.length === 0,
    });

    assert.ok(stats.isFile(), "dist/excalidraw is not a regular file");
    assert.ok(report.binary.executable, "dist/excalidraw is not executable");
    assert.match(fileType, /ELF 64-bit/i, `not a 64-bit ELF: ${fileType}`);
    assert.match(fileType, /x86-64/i, `not an x86-64 executable: ${fileType}`);
    assert.match(fileType, /executable/i, `not an executable: ${fileType}`);
    assert.equal(elfClass, "ELF64", `unexpected ELF class: ${elfClass}`);
    assert.match(machine ?? "", /Advanced Micro Devices X86-64/i, `unexpected ELF machine: ${machine}`);
    assert.match(elfType ?? "", /^(DYN|EXEC)\b/, `not an executable ELF: ${elfType}`);
    assert.equal(debugSections.length, 0, `debug sections remain: ${debugSections.join(", ")}`);
    assert.ok(sizeBytes < sizeLimitBytes, `binary ${sizeBytes} bytes is not under ${sizeLimitBytes}`);
    return {
      fileType,
      class: elfClass,
      type: elfType,
      machine,
      sizeBytes,
      xz9Bytes: xz9.length,
      zstd19Bytes: zstd19.length,
      debugSections,
    };
  },
);

await check("system libraries and package ownership", async () => {
  if (isWindows) {
    // ldd and pacman have no Windows counterpart here; the import table is
    // not read yet, so this records that it was not checked.
    report.libraries.resolved = [];
    report.libraries.systemOnly = null;
    report.libraries.commonPackagesOnly = null;
    report.libraries.dynamicLoadInfo = { note: "Not checked on Windows: the PE import table is not read yet." };
    return { checked: false };
  }
  if (isMac) {
    const output = run("otool", ["-L", artifact.binary]);
    const [, ...libraryLines] = output.trim().split("\n");
    // Keep the report free of checkout paths: the header is the executable.
    report.libraries.otoolOutput = [
      `${repoRelative(repoRoot, artifact.binary)}:`,
      ...libraryLines,
    ].join("\n");
    const resolved = libraryLines
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => {
        const libraryPath = line.split(" (")[0];
        return { name: path.basename(libraryPath), path: libraryPath };
      });
    const systemPath = (library) =>
      library.path.startsWith("/usr/lib/") || library.path.startsWith("/System/Library/");
    const systemOnly = resolved.length > 0 && resolved.every(systemPath);
    report.libraries.resolved = resolved;
    report.libraries.systemOnly = systemOnly;
    report.libraries.commonPackagesOnly = systemOnly;
    report.libraries.dynamicLoadInfo = {
      note: "otool -L lists Mach-O load commands; dyld resolves them from /usr/lib and /System/Library.",
      candidates: [],
    };
    assert.ok(resolved.length > 0, "otool -L listed no dynamic libraries");
    assert.ok(
      systemOnly,
      `one or more libraries are outside /usr/lib and /System/Library: ` +
        resolved.filter((library) => !systemPath(library)).map((library) => library.path).join(", "),
    );
    return {
      resolved,
      systemOnly,
      commonPackagesOnly: systemOnly,
      dynamicLoadInfo: report.libraries.dynamicLoadInfo,
    };
  }

  const output = run("ldd", [binaryPath]);
  report.libraries.lddOutput = output.trim();
  const { resolved, missing } = parseLdd(output);
  const packages = resolved.map((library) => {
    const system = isSystemLibrary(library.path);
    const ownerOutput = run("pacman", ["-Qo", system.canonicalPath]).trim();
    const owner = ownerOutput.match(/ is owned by ([^ ]+)/)?.[1] ?? null;
    return {
      ...library,
      canonicalPath: system.canonicalPath,
      systemPath: system.allowed,
      owner,
      commonSystemPackage: owner !== null && commonSystemPackages.has(owner),
    };
  });
  const systemOnly = missing.length === 0 && packages.every((library) => library.systemPath);
  const commonOnly = packages.every((library) => library.commonSystemPackage);
  report.libraries.resolved = packages;
  report.libraries.systemOnly = systemOnly;
  report.libraries.commonPackagesOnly = commonOnly;
  report.libraries.dynamicLoadInfo = dynamicLoadInfo(resolved);
  assert.deepEqual(missing, [], `unresolved libraries: ${missing.join(", ")}`);
  assert.ok(systemOnly, "one or more resolved libraries are outside /usr/lib and /lib");
  assert.ok(commonOnly, `non-common or unowned libraries: ${packages.filter((lib) => !lib.commonSystemPackage).map((lib) => `${lib.name}:${lib.owner}`).join(", ")}`);
  return {
    resolved: packages.map(({ name, canonicalPath, owner }) => ({ name, path: canonicalPath, package: owner })),
    systemOnly,
    commonPackagesOnly: commonOnly,
    dynamicLoadInfo: report.libraries.dynamicLoadInfo,
  };
});

await check("isolated headless test-control smoke", runSmoke);

await check("submodule remains clean after build", async () => {
  report.submodule.statusAfter = submoduleStatus();
  assert.equal(report.submodule.statusAfter, "", `build modified vendor/excalidraw:\n${report.submodule.statusAfter}`);
  return { status: report.submodule.statusAfter };
});

report.pass = Object.values(report.checks).every((item) => item.pass);
fs.mkdirSync(path.dirname(resultPath), { recursive: true });
fs.writeFileSync(resultPath, `${JSON.stringify(report, null, 2)}\n`);

const rows = Object.entries(report.checks).map(([name, item]) => ({ check: name, result: item.pass ? "PASS" : "FAIL" }));
console.table(rows);
console.log(`Packaging report: ${path.relative(repoRoot, resultPath)} (pass=${report.pass})`);
if (!report.pass) process.exitCode = 1;
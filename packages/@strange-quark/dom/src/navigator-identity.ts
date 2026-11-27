// SPDX-License-Identifier: MIT

// The user agent and platform strings both `navigator` objects report: the
// window's Navigator and a worker's WorkerNavigator.

export type HostOS = "linux" | "macos" | "windows";
export type HostArch = "x86_64" | "aarch64";

export function navigatorIdentity(os: HostOS | undefined, arch: HostArch | undefined): { userAgent: string; platform: string } {
  if (os === "macos") {
    return {
      userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) StrangeQuark",
      platform: "MacIntel",
    };
  }
  if (os === "windows") {
    return {
      userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) StrangeQuark",
      platform: "Win32",
    };
  }
  const architecture = arch === "aarch64" ? "aarch64" : "x86_64";
  return {
    userAgent: `Mozilla/5.0 (X11; Linux ${architecture}) StrangeQuark`,
    platform: `Linux ${architecture}`,
  };
}

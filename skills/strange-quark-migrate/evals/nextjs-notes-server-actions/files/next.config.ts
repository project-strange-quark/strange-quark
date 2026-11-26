import type { NextConfig } from "next";
const config: NextConfig = { images: { remotePatterns: [{ hostname: "avatars.example.com" }] } };
export default config;

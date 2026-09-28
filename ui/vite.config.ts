// `vitest/config` rather than `vite` — it is vite's own `defineConfig` widened with the `test`
// key, so the build config and the test config stay one file and cannot drift into disagreeing
// about the React plugin or the module resolution the components are compiled under.
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { execSync } from "node:child_process";

// Which build this is, shown beside the version on the Status screen: two builds of one version
// (a phone sideloaded twice in an evening) are otherwise impossible to tell apart. The commit,
// then the minute it was built. `ZYRIS_BUILD` overrides it where there is no git checkout.
function buildId(): string {
  if (process.env.ZYRIS_BUILD) return process.env.ZYRIS_BUILD;
  let commit = "unknown";
  try {
    commit = execSync("git rev-parse --short HEAD", { stdio: ["ignore", "pipe", "ignore"] }).toString().trim();
  } catch {}
  return `${commit} ${new Date().toISOString().slice(0, 16).replace("T", " ")}Z`;
}

export default defineConfig({
  plugins: [react(), tailwindcss()],
  define: { __ZYRIS_BUILD__: JSON.stringify(buildId()) },
  // `@/…` is `src/…`, the alias the shadcn components are written against.
  resolve: { alias: { "@": "/src" } },
  // Tauri serves the dev build from this port and expects it not to move.
  server: { port: 5173, strictPort: true },
  build: { outDir: "dist", emptyOutDir: true },
  test: {
    // The components under test render, so they need a DOM. Nothing here talks to Tauri: every
    // `invoke` is mocked in the test that needs one, because a test that reached the real bridge
    // would be testing a webview that is not running.
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    // What jsdom lacks and Radix reaches for: pointer capture, scrolling an option into view,
    // and ResizeObserver.
    setupFiles: ["src/test/setup.ts"],
  },
});

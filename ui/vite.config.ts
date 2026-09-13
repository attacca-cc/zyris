// `vitest/config` rather than `vite` — it is vite's own `defineConfig` widened with the `test`
// key, so the build config and the test config stay one file and cannot drift into disagreeing
// about the React plugin or the module resolution the components are compiled under.
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  // Tauri serves the dev build from this port and expects it not to move.
  server: { port: 5173, strictPort: true },
  build: { outDir: "dist", emptyOutDir: true },
  test: {
    // The components under test render, so they need a DOM. Nothing here talks to Tauri: every
    // `invoke` is mocked in the test that needs one, because a test that reached the real bridge
    // would be testing a webview that is not running.
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});

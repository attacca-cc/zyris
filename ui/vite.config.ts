import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  // Tauri serves the dev build from this port and expects it not to move.
  server: { port: 5173, strictPort: true },
  build: { outDir: "dist", emptyOutDir: true },
});

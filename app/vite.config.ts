import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri drives the dev server; fail loudly rather than silently picking
// another port, or the webview will load nothing.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "chrome105",
    sourcemap: true,
  },
});

import { defineConfig } from "vite"
import react from "@vitejs/plugin-react"
import { fileURLToPath, URL } from "node:url"
import { readFileSync } from "node:fs"

/**
 * One version, from one place.
 *
 * `package.json` is the source of truth. It used to be written out by hand in
 * four files — here, Cargo.toml, tauri.conf.json and the footer — which is
 * three chances for the installer to disagree with what the app tells you it
 * is. `tauri.conf.json` reads the same file, and CI fails if Cargo.toml has
 * drifted.
 */
const { version } = JSON.parse(
  readFileSync(fileURLToPath(new URL("./package.json", import.meta.url)), "utf-8"),
) as { version: string }

// Tauri drives the dev server; fail loudly rather than silently picking a port.
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  define: {
    __APP_VERSION__: JSON.stringify(version),
  },
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "chrome110",
    sourcemap: false,
    rollupOptions: {
      input: {
        // The onboarding shell.
        main: fileURLToPath(new URL("./index.html", import.meta.url)),
        // The Arciin window's own controls, overlaid on the server's page.
        // A separate document because it lives in its own webview.
        titlebar: fileURLToPath(new URL("./titlebar.html", import.meta.url)),
      },
    },
  },
  test: {
    environment: "node",
    include: ["tests/**/*.test.ts"],
  },
})

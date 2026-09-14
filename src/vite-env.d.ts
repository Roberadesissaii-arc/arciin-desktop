/// <reference types="vite/client" />

/**
 * The application version, injected at build time from `package.json`.
 *
 * Declared rather than imported so the footer cannot drift from the installer:
 * there is one version in the repository and everything else derives from it.
 */
declare const __APP_VERSION__: string

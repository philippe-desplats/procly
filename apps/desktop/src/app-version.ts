/**
 * Set at build time from the `version` of `[workspace.package]` in the `Cargo.toml` of the workspace (see
 * `vite.config.ts`), which is the release of the app.
 */
declare const __APP_VERSION__: string;

/** The release of Procly this app ships, the one of the Daemon it embeds. */
export const APP_VERSION: string = __APP_VERSION__;

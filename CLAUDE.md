# Monocode

## Target the native app

New features and fixes go to the native Rust + GPUI app (`crates/monocode-gpui/`,
shared services in `crates/monocode-core/`), not the React/Tauri app (`src/`,
`src-tauri/`). The Tauri app does not use `monocode-core`, so Rust changes never
reach it, and React-only changes never reach the native app.

Exception: the native editor runs the TS editor code from `src/features/files/editor/`
as an embedded bundle for languages without a Tree-sitter grammar in
`crates/monocode-gpui/src/editor_syntax.rs`. After changing that TS code, rebuild
the bundle with `node scripts/native-editor/build.mjs` and commit
`crates/monocode-gpui/assets/native-editor.js`, or the native app will not see it.

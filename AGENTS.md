# Repository Guidelines

## Project Structure & Module Organization
- `src/lib.rs` holds the core generation logic; `src/main.rs` is the CLI entry when the `cli` feature is enabled.
- Key modules: `toml.rs` (config parsing), `schema.rs` (data structures), `tera_filters.rs` (template helpers), and `error.rs` (error types). Keep related tests near the module you touch.
- `about.toml` and `about.hbs` provide metadata/docs templates; `tmp/` is for local sample output; `target/` contains build artifacts and should stay untracked.

## Build, Test, and Development Commands
- `cargo build --all-features` builds the library and CLI.
- `cargo run --features cli -- <path/to/config>` runs the generator against a TOML config.
- `cargo fmt --all` formats the workspace; `cargo fmt --all -- --check` mirrors CI style checks.
- `cargo clippy --all-targets --all-features --workspace` lints the codebase.
- `cargo test --all-features --workspace` runs unit and integration tests.

## Coding Style & Naming Conventions
- Rust 2024 edition; prefer explicit error handling over unchecked `unwrap` in non-test code.
- Naming: snake_case for functions/variables, PascalCase for types, SCREAMING_SNAKE_CASE for constants; align module names with filenames.
- Document public APIs with `///` comments; keep examples in README or doc tests where helpful.
- Keep CLI parsing isolated to `main.rs` and business logic in `lib.rs` to preserve reusability.

## Testing Guidelines
- Place unit tests in the same module files; integration-style checks for CLI flows can go under `tests/` when added.
- Name tests after behavior (e.g., `parses_props_map`, `applies_replace_rule`) and cover new filters or parsing paths.
- Avoid network-bound tests; prefer deterministic fixtures and local templates.

## Commit & Pull Request Guidelines
- Follow Conventional Commits (e.g., `feat:`, `fix:`, `chore:`; scopes optional) consistent with existing history.
- Keep one logical change per PR; include a short summary, motivation, and sample config/output diff when behavior changes.
- Link issues when relevant; add screenshots or snippets if templates or generated files change.

## Configuration & Security Tips
- The CLI feature is optional; enable with `--features cli` when building or running locally.
- Do not commit generated artifacts from `tmp/` or `target/`; scrub configs of secrets before sharing or uploading.

# wincamcfg AI Guide
- **Mission**: `wincamcfg` is a Windows-only CLI that fixes webcam settings via DirectShow COM APIs. Keep command UX simple and scriptable.
- **Code layout**: Entrypoint + clap CLI live in [src/main.rs](src/main.rs); all DirectShow + registry work is isolated in [src/webcam.rs](src/webcam.rs). `build.rs` only embeds the Windows manifest.

## Architecture & Data Flow
- `main.rs` parses commands (`list`, `get`, `set`) and always logs via `tracing_subscriber` with env filters.
- Device + property data flows: `enumerate_devices()` → `DeviceInfo` structs → rendered via `DeviceOutput`/`PropertyOutput` for text or JSON.
- Property writes call `webcam::set_property()`, which sanitizes input, resolves the proper `PropertyType`, then dispatches to the DirectShow setters with COM guards.
- `webcam.rs` creates COM objects (`ICreateDevEnum`, `IMoniker`, `IAMVideoProcAmp`, `IAMCameraControl`) and queries registry/CM APIs for driver metadata. Treat every unsafe call as isolated and accompanied by context errors.
- `get_properties()` abstracts the repeated `GetRange`/`Get` pattern. Extend the `properties` slice here when adding new enums.

## Conventions & Patterns
- Inputs are aggressively validated: camera selectors max 16 chars, property names alphanumeric + ≤64 chars, values ≤32 chars. Mirror these checks when adding new flags to avoid bypasses.
- Use `IndexMap` for deterministic property ordering in JSON/text outputs.
- User-visible strings go through helpers like `format_property_value()` and `build_enum_display()` so UI stays consistent.
- COM lifetime: wrap new DirectShow calls inside `unsafe { ComGuard::new()?; ... }` blocks; never leave COM initialized globally.
- Capability flags must be translated with `format_capabilities()` so auto/manual hints stay in sync with Windows constants.
- Device metadata resolves indirect strings (via `SHLoadIndirectString`) and CM registry keys; reuse `call_with_utf16_buffer_generic()` for any new UTF-16 buffer negotiations.

## Workflows
- Build/test locally with `cargo build` or `cargo build --release`; there are no unit tests yet, so rely on manual runs of `list/get/set`.
- Enable verbose diagnostics using `RUST_LOG=wincamcfg=trace` as documented in [TROUBLESHOOTING.md](TROUBLESHOOTING.md).
- Release binaries rely on the manifest + version info produced by [build.rs](build.rs); don’t change resource identifiers without updating release docs.

## Feature Tips
- New CLI subcommands should extend `Commands` in [src/main.rs](src/main.rs) and plug into the match in `main()`; keep output format parity (text + JSON) with the helpers already in place.
- When supporting another property, update the relevant enum in [src/webcam.rs](src/webcam.rs), include it in the property slice passed to `get_properties()`, and ensure `parse_property_value()` understands any non-numeric inputs.
- Keep error context descriptive by wrapping Windows API calls with `with_context` and bubbling `anyhow::Result` up; users depend on that detail when running with trace logging.
- Remember this tool must run non-interactively for scripting—avoid prompts, and make sure new outputs remain parseable under both formats.

Let me know if any section is unclear or missing details so we can refine the guide.

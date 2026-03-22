# AGENTS

This file gives AI agents the minimum context needed to work safely and quickly in this repository.

## Project summary

Beam is an ephemeral, terminal-first file sharing CLI written in Rust.

- `beam send <path>` defaults to global sharing.
- Global mode uses provider `auto` by default.
- `auto` prefers `cloudflared` when available, then falls back to Beam's native relay client.
- `--local` is explicit LAN mode and serves the same session over HTTP and HTTPS.

The receiver only needs a browser. The sender runs Beam in a terminal.

## Current release

- Current version: `0.1.0`
- Main binary: `beam`
- Reference relay binary: `beam-relay`

## Important product behavior

- Global mode is the primary product path.
- Local mode uses HTTP as the primary link and HTTPS as a secondary encrypted link with a temporary self-signed certificate.
- QR output in local mode should point to the HTTP link.
- Regular files support HTTP `Range` and resumable downloads.
- Directory transfers are streamed as ZIPs and do not support resume.
- `--once`, TTL, PIN, counters, and session state must behave consistently across all transports.

## Key modules

- `src/cli.rs`: CLI parsing, top-level flow, mode selection, startup and shutdown.
- `src/session.rs`: shared session state, HTTP handlers, TTL, PIN, `--once`, and range handling.
- `src/content.rs`: file and directory content sources, ZIP streaming, directory filtering.
- `src/provider.rs`: global provider selection, `cloudflared`, native relay client, `auto`.
- `src/relay.rs`: reference Beam relay service.
- `src/relay_protocol.rs`: relay protocol types and frame helpers.
- `src/ui.rs`: terminal rendering, QR output, session status.
- `src/doctor.rs`: environment diagnostics.
- `src/tls.rs`: local HTTPS certificate generation.

## Commands agents should use

- Run tests: `cargo test`
- Check the CLI: `cargo run -- --help`
- Check environment readiness: `cargo run -- doctor`
- Run Beam locally: `cargo run -- send README.md --local -t 30s`
- Run the reference relay: `cargo run --bin beam-relay`
- Test native relay explicitly:
  `BEAM_RELAY_URL=http://127.0.0.1:8787 cargo run -- send README.md --provider native -t 30s`

## Guardrails

- Do not silently change the meaning of `auto`, `--global`, or `--local`.
- Do not claim Beam ships with a hosted public native relay unless one is actually deployed.
- Keep native relay behavior honest in docs: the client is embedded, the relay service is separate.
- Preserve the "single sender binary" philosophy. Avoid adding heavyweight runtime dependencies unless clearly justified.
- Keep ZIP generation streaming. Do not switch to buffering whole archives in memory.
- Keep `Range` support limited to regular files unless ZIP resume is intentionally designed.

## Release workflow

When shipping a new version:

- Update `Cargo.toml`
- Update `Cargo.lock`
- Update `CHANGELOG.md`
- Update `README.md` if behavior or install flow changed
- Run `cargo test`
- Create a release commit
- Tag the release as `vX.Y.Z`
- Publish the GitHub release

Publishing a GitHub release triggers the Homebrew tap update workflow for `lopezlean/homebrew-beam`.


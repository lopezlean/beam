# Beam

Beam is an ephemeral, terminal-first file sharing CLI.

You run one command, Beam opens a short-lived download server, prints a big QR code in your terminal, copies the link to your clipboard, and destroys the session when the timer ends.

The sender uses the terminal. The receiver only needs a browser.

See [CHANGELOG.md](CHANGELOG.md) for release-by-release notes.

## What Beam does today

- Share a single file through a temporary public HTTPS link by default.
- Share a single file over your local network with `--local`.
- Share a directory as a ZIP archive generated on the fly.
- Set an explicit TTL for every session.
- Use burn-after-reading mode with `--once`.
- Protect a session with an optional PIN.
- Print a QR code directly in the terminal.
- Prefer `cloudflared` automatically when it is available.
- Fall back to Pinggy over SSH when `cloudflared` is unavailable or fails to start.
- Use the native Beam relay client last, when a relay endpoint is configured or clearly reachable.
- Support resumable downloads with HTTP `Range` for regular files.

## Current status

Beam is an early v1 CLI.

- Sender support: macOS and Linux.
- Receiver support: any device with a browser.
- Global mode is the default path and uses provider `auto`.
- `auto` tries `cloudflared`, then Pinggy over SSH, then the native relay client when available.
- Local mode exposes both HTTP and HTTPS, with HTTP as the primary LAN link.
- Directory ZIP streaming stays chunked and does not support resume.

## Why Beam

- One binary.
- No accounts.
- No background daemon.
- No permanent uploads.
- No dashboard.
- No forgotten cloud files living forever on someone else's server.

## Install

Beam currently runs from source.

```bash
cargo build --release
./target/release/beam version
```

Homebrew install:

```bash
brew tap lopezlean/beam
brew install beam
```

The Homebrew formula installs `cloudflared` automatically, so `auto` usually tries the Cloudflare path first on Homebrew systems and falls back to Pinggy over your system `ssh` when needed.

Or run it directly during development:

```bash
cargo run -- version
```

## Requirements

- Rust toolchain to build Beam.
- `cloudflared` if you want Beam to prefer the Cloudflare path outside Homebrew.
- OpenSSH (`ssh`) if you want Beam to use the no-account Pinggy fallback outside typical macOS/Linux defaults.
- Nothing extra for the native relay client itself, but you need a reachable Beam relay endpoint. For local testing and self-hosting, the repo includes `beam-relay`.
- A terminal with ANSI/Unicode support for the best QR experience.

Check your machine with:

```bash
beam doctor
```

Or from source:

```bash
cargo run -- doctor
```

## Homebrew

Beam is distributed through the dedicated tap repository `lopezlean/homebrew-beam`.

Install it with:

```bash
brew tap lopezlean/beam
brew install beam
```

This installs both `beam` and `cloudflared`. Pinggy support uses your system `ssh`.

You can also use the fully-qualified formula name:

```bash
brew install lopezlean/beam/beam
```

The tap is updated automatically from GitHub releases published in `lopezlean/beam`.

## Release automation

Publishing a GitHub release in `lopezlean/beam` updates `lopezlean/homebrew-beam` automatically through `.github/workflows/publish-homebrew-tap.yml`.

This workflow requires one repository secret in `lopezlean/beam`:

- `HOMEBREW_TAP_TOKEN`: a GitHub token with `contents: write` access to `lopezlean/homebrew-beam`

## Usage

```bash
beam send [OPTIONS] <PATH>
```

All examples below assume `beam` is available on your `PATH`. If you are running directly from the repo, use `cargo run -- ...` instead.

Main options:

- `-t, --ttl <TTL>`: session lifetime, default `30m`
- `--once`: destroy the session after the first successful download
- `--global`: explicit alias for the default public tunnel mode
- `--local`: serve over your LAN with HTTP primary and HTTPS secondary links
- `--provider <PROVIDER>`: `auto`, `cloudflared`, `pinggy`, or `native`
- `--pin[=<PIN>]`: require a PIN; if no value is provided, Beam generates one
- `--archive <ARCHIVE>`: archive format for directories, currently `zip`
- `--port <PORT>`: fixed global port, or the base HTTP port in `--local`

## Examples

Share a file through the default public tunnel:

```bash
beam send video.mp4
```

Share a file for 15 minutes:

```bash
beam send design.fig -t 15m
```

Burn after reading:

```bash
beam send secrets.env --once
```

Burn after reading with a generated PIN:

```bash
beam send secrets.env --once --pin
```

Send a folder as a ZIP:

```bash
beam send ./my-folder
```

Force the Cloudflare tunnel explicitly:

```bash
beam send backup.sql --provider cloudflared -t 2h
```

Force the Pinggy SSH tunnel explicitly:

```bash
beam send backup.sql --provider pinggy -t 2h
```

Force the native relay explicitly:

```bash
beam send backup.sql --provider native -t 2h
```

Share only on your local network:

```bash
beam send photo.jpg --local
```

## Local mode vs global mode

### Global mode (default, provider `auto`)

Beam starts a local origin server and chooses a public provider automatically.

- The public URL is HTTPS.
- The link is easy to open on phones and remote devices.
- This is the recommended path when you want the least browser friction.
- If `cloudflared` is available on `PATH`, Beam tries it first.
- If Cloudflare startup fails or `cloudflared` is unavailable, Beam falls back to Pinggy over SSH when `ssh` is available.
- Beam only tries the native relay automatically when `BEAM_RELAY_URL` is configured or the default local relay endpoint is already reachable.
- The native path still needs a reachable Beam relay service. The repo ships a reference relay for development and self-hosting, but Beam does not bundle a public hosted relay in this release.
- Pinggy's free unauthenticated path uses random public domains and may expire after 60 minutes even if Beam's TTL is longer.

For local relay development or self-hosting, you can run the reference relay server shipped in this repo:

```bash
cargo run --bin beam-relay
```

Then point Beam at it:

```bash
BEAM_RELAY_URL=http://127.0.0.1:8787 beam send file.txt --provider native
```

`auto` will only select that native path when `BEAM_RELAY_URL` is configured or the default local relay is already reachable.

### Local mode

Beam serves the same session on your LAN with two links:

- Primary: `http://...` for the least browser friction.
- Secondary: `https://...` with a temporary self-signed certificate.

Important:

- Beam prints the QR code for the HTTP LAN link.
- The HTTPS LAN link is encrypted, but browsers such as Brave may show a certificate warning like `ERR_CERT_AUTHORITY_INVALID`.
- If you pass `--port 8080`, Beam uses HTTP on `8080` and tries HTTPS on `8081..8090` before picking the next free port above that range.

## How expiration works

Every Beam session is ephemeral.

- When the TTL expires, Beam shuts down the server and exits.
- With `--once`, Beam destroys the session immediately after the first successful download.
- Beam does not keep a background daemon alive after the session ends.

## Security notes

- Global mode uses HTTPS public URLs through the selected provider.
- Pinggy global links are public HTTPS URLs backed by a no-account SSH tunnel.
- The native relay forwards requests through a Beam relay endpoint and does not store the payload on disk.
- Local mode uses HTTP as the primary convenience link and HTTPS as a secondary encrypted link with an untrusted temporary certificate.
- `--pin` adds an application-level gate before download.
- `--once` is useful for sensitive one-time transfers.
- Regular files support HTTP `Range` so interrupted downloads can resume.
- Directory ZIP streaming stays chunked and does not currently support resume.
- Beam does not implement end-to-end encryption in the application layer yet.

## Development

Run tests:

```bash
cargo test
```

Show CLI help:

```bash
beam --help
beam send --help
```

Generate shell completions:

```bash
beam completion zsh
beam completion bash
```

## Roadmap ideas

- Trusted local certificates for personal devices.
- Additional global providers beyond `cloudflared`.
- Prebuilt binaries.
- Resume support for interrupted downloads.
- Better browser trust flow for LAN sharing.

## License

MIT

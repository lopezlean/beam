# Beam

Beam is an ephemeral, terminal-first file sharing CLI.

You run one command, Beam opens a short-lived download server, prints a big QR code in your terminal, copies the link to your clipboard, and destroys the session when the timer ends.

The sender uses the terminal. The receiver only needs a browser.

## What Beam does today

- Share a single file over your local network.
- Share a directory as a ZIP archive generated on the fly.
- Set an explicit TTL for every session.
- Use burn-after-reading mode with `--once`.
- Protect a session with an optional PIN.
- Print a QR code directly in the terminal.
- Expose a temporary public URL with `--global` via `cloudflared`.

## Current status

Beam is an early v1 CLI.

- Sender support: macOS and Linux.
- Receiver support: any device with a browser.
- Local mode uses HTTPS with a temporary self-signed certificate.
- Global mode is beta and currently uses `cloudflared`.

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

The Homebrew formula installs `cloudflared` automatically as a runtime dependency for `beam --global`.

Or run it directly during development:

```bash
cargo run -- version
```

## Requirements

- Rust toolchain to build Beam.
- `cloudflared` if you want `--global` and you are not installing Beam through Homebrew.
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

This installs both `beam` and `cloudflared`.

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
- `--global`: expose the session through a public tunnel
- `--provider <PROVIDER>`: tunnel backend, currently `cloudflared`
- `--pin[=<PIN>]`: require a PIN; if no value is provided, Beam generates one
- `--archive <ARCHIVE>`: archive format for directories, currently `zip`
- `--port <PORT>`: fixed port instead of a random free port

## Examples

Share a file on your local network:

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

Expose a public link through a tunnel:

```bash
beam send backup.sql --global -t 2h
```

## Local mode vs global mode

### Local mode

Beam serves the download over your LAN and prints an `https://` link with a QR code.

Important:

- Beam generates a temporary self-signed certificate for each local session.
- Browsers such as Brave may show a certificate warning like `ERR_CERT_AUTHORITY_INVALID`.
- This happens because the certificate is not signed by a trusted public CA.
- The warning is expected in pure LAN mode.

If you want a browser flow without that warning for the receiver, use `--global`.

### Global mode

Beam starts a local server and exposes it through `cloudflared`.

- The public URL is HTTPS.
- The link is easy to open on phones and remote devices.
- This is the recommended path when you want the least browser friction.
- This mode is still beta and depends on `cloudflared` being installed and available on `PATH`.

## How expiration works

Every Beam session is ephemeral.

- When the TTL expires, Beam shuts down the server and exits.
- With `--once`, Beam destroys the session immediately after the first successful download.
- Beam does not keep a background daemon alive after the session ends.

## Security notes

- Local mode uses HTTPS, but the certificate is temporary and untrusted by default.
- `--pin` adds an application-level gate before download.
- `--once` is useful for sensitive one-time transfers.
- `--global` currently relies on a tunnel provider instead of a Beam-hosted relay.
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

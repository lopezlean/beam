# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-03-23

### Added
- Pinggy-over-SSH as a no-account global provider and as the fallback after `cloudflared` in provider `auto`.
- Serveo-over-SSH as an explicit global provider via `--provider serveo`.
- Path-based session entry links under `/s/<token>` to keep public links working through providers that do not preserve query strings.

### Changed
- Global provider startup now reports ordered fallback attempts and exposes the actual provider chosen by `auto`.
- The download page and generated public links now use path-based session routes, while preserving compatibility with the legacy `?token=` page format.
- Provider code is now split into dedicated modules for `cloudflared`, `pinggy`, `serveo`, `native`, and shared SSH helpers.
- `beam doctor` now reports SSH-backed provider readiness more clearly, including Serveo availability.

### Fixed
- Improved `cloudflared` startup diagnostics to surface rate-limit and startup errors instead of generic timeouts.
- Serveo now uses the correct anonymous SSH flow without prompting for local key passphrases.
- Serveo public URL parsing now handles ANSI-colored output and the real `*.serveousercontent.com` hostnames.

## [0.1.1] - 2026-03-23

### Added
- Astro-powered project site for GitHub Pages.
- `AGENTS.md` with repository-specific guidance for AI agents.

### Changed
- Extracted the download page into a dedicated renderer and HTML template instead of building it inline inside `session.rs`.
- Refined the mobile download page so the primary action sits at the top and better matches Beam's current visual language.

### Fixed
- Local HTTPS startup now installs Rustls' `ring` crypto provider explicitly, avoiding runtime failures when serving the secondary TLS endpoint.
- Cleaned up the download page action layout to remove redundant nested containers and duplicate calls to action.

## [0.1.0] - 2026-03-22

### Added
- Provider `auto`, which prefers `cloudflared` and falls back to Beam's native relay client.
- Native relay client embedded in the Beam binary, plus a reference `beam-relay` server for local relay development and self-hosting.
- HTTP `Range` support for regular file downloads so interrupted transfers can resume.
- Local dual transport mode with HTTP as the primary LAN link and HTTPS as the secondary encrypted link.
- Stronger `beam doctor` output for provider selection, streaming readiness, and range support.

### Changed
- Global sharing is now the default for `beam send`; `--local` is the explicit LAN mode.
- Local mode now shares one session across both HTTP and HTTPS listeners instead of treating them as separate flows.
- Terminal output now distinguishes the active global provider and the primary vs secondary local links more clearly.

### Fixed
- Reduced unnecessary terminal repainting while showing the QR and session status.
- Improved local HTTPS behavior for browsers that refuse plain HTTP by default.

## [0.0.2] - 2026-03-22

### Added
- Automated Homebrew tap publishing from GitHub releases.

### Changed
- Homebrew installation now pulls `cloudflared` as a dependency.

## [0.0.1] - 2026-03-22

### Added
- Initial Beam release with ephemeral file sharing, QR output, TTL, `--once`, PIN protection, folder ZIP streaming, and local/global send flows.

[Unreleased]: https://github.com/lopezlean/beam/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/lopezlean/beam/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/lopezlean/beam/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/lopezlean/beam/compare/v0.0.2...v0.1.0
[0.0.2]: https://github.com/lopezlean/beam/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/lopezlean/beam/releases/tag/v0.0.1

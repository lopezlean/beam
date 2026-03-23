export const siteMeta = {
  name: "Beam",
  version: "0.1.0",
  title: "BEAM | Ephemeral Terminal-First File Sharing",
  description:
    "Beam is an ephemeral, terminal-first file sharing CLI for public and local transfers with QR codes, TTLs, PINs, and zero dashboard bloat.",
  repository: "https://github.com/lopezlean/beam",
  releases: "https://github.com/lopezlean/beam/releases",
  changelog: "https://github.com/lopezlean/beam/blob/main/CHANGELOG.md",
  tap: "https://github.com/lopezlean/homebrew-beam"
} as const;

export const navLinks = [
  { href: "#features", label: "Features" },
  { href: "#install", label: "Install" },
  { href: "#usage", label: "Usage" },
  { href: "#examples", label: "Examples" },
  { href: "#faq", label: "FAQ" }
] as const;

export const heroSessionDocument = {
  title: "Beam ⚡️",
  subtitle: "Global default",
  entries: [
    { label: "Payload", value: "Vector.png (file)" },
    { label: "Download", value: "Vector.png" },
    { label: "Size", value: "26.84 KiB" },
    { label: "Transport", value: "HTTPS tunnel via cloudflared" },
    { label: "Security", value: "token URL" },
    {
      label: "Public HTTPS",
      value:
        "https://beam.link/?token=token",
      accent: true
    }
  ],
  qr: `<svg xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:cc="http://creativecommons.org/ns#" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:svg="http://www.w3.org/2000/svg" xmlns="http://www.w3.org/2000/svg" version="1.1" width="368" height="368" id="svg8822">
  <metadata id="metadata9308">
    <rdf:RDF>
      <cc:Work rdf:about="">
        <dc:format>image/svg+xml</dc:format>
        <dc:type rdf:resource="http://purl.org/dc/dcmitype/StillImage"/>
        <dc:title/>
      </cc:Work>
    </rdf:RDF>
  </metadata>
  <defs id="defs9306"/>
  <path d="m 16,16 0,16 0,16 0,16 0,16 0,16 0,16 0,16 16,0 16,0 16,0 16,0 16,0 16,0 16,0 0,-16 0,-16 0,-16 0,-16 0,-16 0,-16 0,-16 -16,0 -16,0 -16,0 -16,0 -16,0 -16,0 -16,0 z m 128,0 0,16 0,16 16,0 0,-16 16,0 0,-16 -16,0 -16,0 z m 32,16 0,16 16,0 0,-16 -16,0 z m 16,16 0,16 16,0 16,0 0,-16 0,-16 0,-16 -16,0 0,16 0,16 -16,0 z m 0,16 -16,0 -16,0 -16,0 0,16 16,0 16,0 0,16 -16,0 0,16 16,0 0,16 16,0 0,-16 16,0 0,16 -16,0 0,16 -16,0 0,16 16,0 16,0 0,16 16,0 0,-16 0,-16 0,-16 0,-16 0,-16 -16,0 0,-16 -16,0 0,-16 z m 16,112 -16,0 0,16 -16,0 0,16 0,16 16,0 0,16 0,16 -16,0 -16,0 0,-16 16,0 0,-16 -16,0 0,-16 0,-16 -16,0 0,-16 16,0 0,16 16,0 0,-16 0,-16 -16,0 -16,0 0,-16 -16,0 -16,0 -16,0 0,16 -16,0 0,16 -16,0 0,-16 16,0 0,-16 -16,0 -16,0 0,16 -16,0 0,16 0,16 0,16 16,0 0,-16 16,0 16,0 16,0 0,-16 16,0 0,-16 16,0 0,16 -16,0 0,16 16,0 0,16 -16,0 0,16 16,0 16,0 0,16 0,16 0,16 16,0 0,16 16,0 16,0 16,0 0,16 -16,0 -16,0 -16,0 0,-16 -16,0 0,16 0,16 16,0 0,16 -16,0 0,16 16,0 16,0 0,-16 16,0 0,16 16,0 16,0 16,0 16,0 0,-16 16,0 0,16 16,0 16,0 16,0 0,-16 -16,0 -16,0 0,-16 -16,0 0,-16 -16,0 0,16 -16,0 -16,0 0,16 -16,0 0,-16 16,0 0,-16 0,-16 0,-16 16,0 0,-16 -16,0 -16,0 0,-16 16,0 0,-16 0,-16 0,-16 -16,0 0,-16 z m 48,128 0,-16 -16,0 0,16 16,0 z m 32,16 16,0 16,0 0,-16 -16,0 -16,0 0,16 z m 32,-16 16,0 0,-16 0,-16 0,-16 0,-16 -16,0 -16,0 -16,0 0,-16 -16,0 0,16 -16,0 0,16 0,16 16,0 0,-16 16,0 0,16 0,16 16,0 16,0 0,16 z m -48,-80 0,-16 -16,0 -16,0 0,16 16,0 16,0 z m 16,0 16,0 0,-16 0,-16 0,-16 16,0 0,16 16,0 0,16 16,0 0,-16 0,-16 -16,0 0,-16 16,0 0,-16 -16,0 -16,0 0,16 -16,0 0,-16 -16,0 0,16 -16,0 0,16 16,0 0,16 0,16 0,16 z m -16,-48 -16,0 0,16 16,0 0,-16 z m 64,32 -16,0 0,16 16,0 0,-16 z m -224,0 0,-16 -16,0 0,16 16,0 z m -16,0 -16,0 -16,0 -16,0 0,16 16,0 16,0 16,0 0,-16 z m -64,0 -16,0 0,16 16,0 0,-16 z m 0,-48 0,-16 -16,0 0,16 16,0 z m 112,-16 16,0 0,-16 0,-16 -16,0 0,16 0,16 z m 96,-128 0,16 0,16 0,16 0,16 0,16 0,16 0,16 16,0 16,0 16,0 16,0 16,0 16,0 16,0 0,-16 0,-16 0,-16 0,-16 0,-16 0,-16 0,-16 -16,0 -16,0 -16,0 -16,0 -16,0 -16,0 -16,0 z m -208,16 16,0 16,0 16,0 16,0 16,0 0,16 0,16 0,16 0,16 0,16 -16,0 -16,0 -16,0 -16,0 -16,0 0,-16 0,-16 0,-16 0,-16 0,-16 z m 224,0 16,0 16,0 16,0 16,0 16,0 0,16 0,16 0,16 0,16 0,16 -16,0 -16,0 -16,0 -16,0 -16,0 0,-16 0,-16 0,-16 0,-16 0,-16 z m -208,16 0,16 0,16 0,16 16,0 16,0 16,0 0,-16 0,-16 0,-16 -16,0 -16,0 -16,0 z m 224,0 0,16 0,16 0,16 16,0 16,0 16,0 0,-16 0,-16 0,-16 -16,0 -16,0 -16,0 z m -32,96 0,16 16,0 0,-16 -16,0 z m -224,96 0,16 0,16 0,16 0,16 0,16 0,16 0,16 16,0 16,0 16,0 16,0 16,0 16,0 16,0 0,-16 0,-16 0,-16 0,-16 0,-16 0,-16 0,-16 -16,0 -16,0 -16,0 -16,0 -16,0 -16,0 -16,0 z m 16,16 16,0 16,0 16,0 16,0 16,0 0,16 0,16 0,16 0,16 0,16 -16,0 -16,0 -16,0 -16,0 -16,0 0,-16 0,-16 0,-16 0,-16 0,-16 z m 16,16 0,16 0,16 0,16 16,0 16,0 16,0 0,-16 0,-16 0,-16 -16,0 -16,0 -16,0 z m 288,48 0,16 16,0 0,-16 -16,0 z" id="path3093" style="fill:currentColor;stroke:none"/>
</svg>`.trim()
} as const;

export const workflowSteps = [
  {
    step: "01 // SENDER",
    title: "Terminal Native",
    body: "Share from your shell with one command. Beam stays out of the way and fits naturally into development workflows."
  },
  {
    step: "02 // RECEIVER",
    title: "Universal Browser",
    body: "The receiver only needs a browser. No Beam install, no dashboard, and no account creation on the other end."
  }
] as const;

export const featureCards = [
  {
    icon: "timer",
    title: "Configurable TTL",
    body: "Every session expires automatically. Keep links alive for minutes or hours, then let them disappear without cleanup."
  },
  {
    icon: "local_fire_department",
    title: "Burn-after-reading",
    body: "Use --once to destroy the session immediately after the first successful download."
  },
  {
    icon: "folder_zip",
    title: "Directory ZIP Streaming",
    body: "Send whole folders as ZIP streams on the fly without writing temporary archives back to disk."
  },
  {
    icon: "sync_saved_locally",
    title: "Resumable file downloads",
    body: "Regular files support HTTP Range so interrupted transfers can resume cleanly."
  },
  {
    icon: "public",
    title: "Auto provider routing",
    body: "Global mode defaults to provider auto, preferring cloudflared and then the native relay client when a relay endpoint is reachable."
  },
  {
    icon: "lan",
    title: "Dual local transport",
    body: "Local mode shares a single session over HTTP and HTTPS, keeping HTTP as the primary friction-free LAN path."
  }
] as const;

export const installCards = [
  {
    label: "macOS / Linux (Homebrew)",
    command: "brew tap lopezlean/beam\nbrew install beam",
    note: "The Homebrew formula also installs cloudflared, so provider auto usually picks the Cloudflare path out of the box."
  },
  {
    label: "Rust source build",
    command: "cargo build --release\n./target/release/beam version",
    note: "Use this path when hacking on Beam itself or when you want full control over the runtime."
  }
] as const;

export const exampleCommands = [
  {
    title: "Basic send",
    command: "beam send video.mp4",
    note: "Share a file through the default public HTTPS path."
  },
  {
    title: "Timeboxed link",
    command: "beam send design.fig -t 15m",
    note: "Keep the session alive for exactly fifteen minutes."
  },
  {
    title: "Burn after reading",
    command: "beam send secrets.env --once --pin",
    note: "One download, then the session is gone."
  },
  {
    title: "Folder transfer",
    command: "beam send ./project-assets",
    note: "Stream a directory as a ZIP archive without temp clutter."
  }
] as const;

export const manualEntries = [
  {
    title: "Send",
    description:
      "The primary command. Share local files or folders over a short-lived public link or over your LAN.",
    flags: ["--ttl 10m", "--once", "--pin", "--local", "--provider native"],
    command: "beam send report.pdf --ttl 30m --once",
    detail: "Sends report.pdf, expires in 30 minutes, and destroys the session after the first successful download."
  },
  {
    title: "Local mode",
    description:
      "Use local mode when you want LAN-only sharing. Beam keeps HTTP primary for the smoothest browser flow and also exposes HTTPS as a secondary link.",
    flags: ["--local", "--port 8080", "--ttl 5m"],
    command: "beam send photo.jpg --local --port 8080",
    detail: "Serves HTTP on 8080 and searches for the nearest available HTTPS port starting at 8081."
  },
  {
    title: "Native relay",
    description:
      "Beam embeds a native relay client. For development or self-hosting, point it to a reachable Beam relay endpoint.",
    flags: ["--provider native", "BEAM_RELAY_URL=http://127.0.0.1:8787"],
    command: "BEAM_RELAY_URL=http://127.0.0.1:8787 beam send build.zip --provider native",
    detail: "Useful for testing the native path without cloudflared."
  }
] as const;

export const faqItems = [
  {
    question: "Global vs local mode?",
    answer:
      "Global mode is the default and is the best path for phones and remote devices. Local mode keeps sharing on your LAN and serves the same session over HTTP and HTTPS."
  },
  {
    question: "Does Beam store my file permanently?",
    answer:
      "Beam keeps the session in the sender process and does not create permanent hosted uploads as part of the Beam app. The reference native relay forwards traffic and does not persist payloads to disk."
  },
  {
    question: "Does Beam support resumed downloads?",
    answer:
      "Yes for regular files through HTTP Range. Folder ZIP streams are chunked and do not currently support resume."
  },
  {
    question: "Is the native relay public by default?",
    answer:
      "No. Beam embeds the native relay client, but a hosted public relay is not bundled in this release. This repo ships beam-relay for local relay development and self-hosting."
  }
] as const;

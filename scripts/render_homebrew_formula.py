#!/usr/bin/env python3

from __future__ import annotations

import argparse
from pathlib import Path


TEMPLATE = """class Beam < Formula
  desc "Ephemeral terminal-first file sharing"
  homepage "https://github.com/lopezlean/beam"
  url "https://github.com/lopezlean/beam/archive/refs/tags/v{version}.tar.gz"
  sha256 "{sha256}"
  license "MIT"

  depends_on "rust" => :build
  depends_on "cloudflared"

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/beam version")
  end
end
"""


def main() -> None:
    parser = argparse.ArgumentParser(description="Render the Beam Homebrew formula.")
    parser.add_argument("--version", required=True, help="Release version without the leading v.")
    parser.add_argument("--sha256", required=True, help="SHA256 for the release tarball.")
    parser.add_argument("--output", required=True, help="Where to write the formula.")
    args = parser.parse_args()

    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        TEMPLATE.format(version=args.version, sha256=args.sha256),
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()

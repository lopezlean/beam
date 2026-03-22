class Beam < Formula
  desc "Ephemeral terminal-first file sharing"
  homepage "https://github.com/lopezlean/beam"
  url "https://github.com/lopezlean/beam/archive/refs/tags/v0.0.1.tar.gz"
  sha256 "b69822817c35d1a2269c19cb41f86b8cef6a719b49663d0b09b7e7e36746222b"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/beam version")
  end
end

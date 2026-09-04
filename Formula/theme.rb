# The tap lives in the product repository, so `brew tap snaraj/theme
# https://github.com/snaraj/theme` needs no second repository kept in step.
# Homebrew 6 will not use a third-party tap until it is trusted, so the
# install is: tap, `brew trust --formula snaraj/theme/theme` (the narrowest
# grant — never the whole tap), then `brew install snaraj/theme/theme`.
#
# Bumping this after a release is a HAND edit, on purpose — a workflow that
# opens its own pull request would sidestep the review contract every other
# change obeys. Take the four digests from that release's SHA256SUMS, set
# the four sha256 lines (the trailing `# <target>` comments say which is
# which), point the four URLs at the new tag, set `version`, and open a
# normal Draft PR under the usual contract.
#
# The version is STATED, not left to Homebrew's URL scan: that scan reads
# "64-unknown-linux-gnu" out of the x86_64 asset name and puts the keg
# under it (seen with `brew test`).
class Theme < Formula
  desc "Wallpaper and terminal palette driven by one command"
  homepage "https://github.com/snaraj/theme"
  version "0.3.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.0/theme-aarch64-apple-darwin.tar.gz"
      sha256 "cdd6aef3f3e9cd479e68ee9e05537a78078f5cf5738f77023e798dbfe7137ce2" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.0/theme-x86_64-apple-darwin.tar.gz"
      sha256 "23869698516dbd75f4f2834dd48d0e2f67d7d5290d99f5dd7e2182d3f051c9a6" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.0/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "28b7fc3100e190fb9b73e883a625e7931d4ffd91618520c30798851d9d91230c" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.0/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "bee4dbe9f33074710e588168fa7696eacb94acac6a62395c43ae5262275693cd" # x86_64-unknown-linux-gnu
    end
  end

  def install
    bin.install "theme"
  end

  # No network out of a sandboxed build; the version line is the whole test.
  test do
    ENV["THEME_NO_UPDATE_CHECK"] = "1"
    assert_match "v#{version}", shell_output("#{bin}/theme version")
  end
end

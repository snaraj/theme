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
  version "0.3.5"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.5/theme-aarch64-apple-darwin.tar.gz"
      sha256 "231b6f66b104976fe3476292852efa5b434c9c101f18d3394662f12bb6453581" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.5/theme-x86_64-apple-darwin.tar.gz"
      sha256 "94c8323186e2d72973ca94bd5c558fb3d0e4003915f74386b959a78ee81f679f" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.5/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "7d42dcf7de0b5d838c65530028d99a0dcc5b8c53f5c9728c2aff8d83ed2a8197" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.5/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "93c25462eaee82957a72ad36e134a317b0fd602a0c0a28eafe97074510ba8717" # x86_64-unknown-linux-gnu
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

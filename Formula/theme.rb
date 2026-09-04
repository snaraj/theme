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
  version "0.2.2"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.2.2/theme-aarch64-apple-darwin.tar.gz"
      sha256 "6b0ff6a3eb4e313a8e97a8151fa84759d2661efd541239a370aec8c42df6de2c" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.2.2/theme-x86_64-apple-darwin.tar.gz"
      sha256 "3271a59e013f8269c45fe0d02debab5f52c868ee2b25dc7ab81959b3a4baf695" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.2.2/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "955695f6428b8dc0880d25e399a3ab2b79c636cbee56c79349e5464ba4bd4268" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.2.2/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b12954f2e16f4575b2ba8a7526a960e11d26987a83571fd04b44e72c473cb002" # x86_64-unknown-linux-gnu
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

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
  version "0.3.4"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.4/theme-aarch64-apple-darwin.tar.gz"
      sha256 "fe649a3234a044c0c1f6fe60db4d3313762f1cfb17d7d9cf43412f92e98b7576" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.4/theme-x86_64-apple-darwin.tar.gz"
      sha256 "1a3024627a76d45da2194587741ed59692b7e5cf3e7cbb763aa96e749712243c" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.4/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "fe65971adf753a12411e30b2c2126f001cbbc73b186b3701859748877bd44971" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.4/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "ea21325fd9c332a7c67402ec796566431ff8773279d2956f3ad6a3414d77bb71" # x86_64-unknown-linux-gnu
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

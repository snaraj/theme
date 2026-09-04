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
  version "0.3.1"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.1/theme-aarch64-apple-darwin.tar.gz"
      sha256 "1aee2a42f7ca66565f76167a4fb7f1ecb9198cc55392598306bba87405d5e704" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.1/theme-x86_64-apple-darwin.tar.gz"
      sha256 "d664d30caffd406171443d99a5714d7069f647d38324134360bd590f7d0beaaa" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.1/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0b5429ae7662b6cac07a578d69edb0ac9e9b9c0e8fe2dce20c25fea087aae6a2" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.1/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e2c7346d9c6343eb2329c6fabf054e9ba45f6557d5cd8390041fefdeb138c209" # x86_64-unknown-linux-gnu
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

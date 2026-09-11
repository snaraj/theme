# The tap lives in the product repository, so `brew tap snaraj/theme
# https://github.com/snaraj/theme` needs no second repository kept in step.
# Homebrew 6 will not use a third-party tap until it is trusted, so the
# install is: `brew trust --formula snaraj/theme/theme`, tap, then install.
# Trust only this formula.
#
# Keep all four URLs and digests bound to a published release. CI downloads
# every package, verifies SHA256SUMS, and installs/tests this formula.
# The release workflow stays red until the matching formula change merges.
class Theme < Formula
  desc "Wallpaper and terminal palette driven by one command"
  homepage "https://github.com/snaraj/theme"
  version "0.3.9"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.9/theme-aarch64-apple-darwin.tar.gz"
      sha256 "921cde045de96a37c5aa84e4965bad7dea006fcaeb60b772d08fc5fc44c13cdc" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.9/theme-x86_64-apple-darwin.tar.gz"
      sha256 "95451dafea02754e43a10350573dbe35146730dae6b2ce8ffb194bac6afc8da1" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.9/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "ed1a346d415aef624d7bfd3844bee7dfdf521f09c5ebba70a142637b80cdf03a" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.9/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "54bbf7509a49ecb3409c401ba09a491ecce8158d23a91044edc679b160712911" # x86_64-unknown-linux-gnu
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

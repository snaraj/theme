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
  version "0.3.10"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.10/theme-aarch64-apple-darwin.tar.gz"
      sha256 "136633d5b098a38948ccf8247486d4e6a987fab12a2201e693ba511c0897eab8" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.10/theme-x86_64-apple-darwin.tar.gz"
      sha256 "1b50be16edf0d8d6bf3b896270a193a14d112e2a7b07992a732c3038a1fb431c" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.10/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "9ba90849be84528004f7c43b8d2c47cf94b06108793599b39e340b96180d361d" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.10/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "c210820769e7ce266bdd75675191a3b81b507adf56bcad6527efd30acf6b6ad0" # x86_64-unknown-linux-gnu
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

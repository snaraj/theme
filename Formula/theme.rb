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
  version "0.3.7"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.7/theme-aarch64-apple-darwin.tar.gz"
      sha256 "af2cf21486750fb8b40fec2361b31db4bd9c073a308898a6e91937de73f5cd33" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.7/theme-x86_64-apple-darwin.tar.gz"
      sha256 "fb1f4a442d24a5e43de3b13b35d85936275caa996e5cdb73efb7a73e10842141" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.7/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "89bf84258469411f785ed88391edd6b0fad1ab303943c19224e9e42c8c895a3e" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.7/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "371c9cde2e804db6d402541a54404ab744ed2e6f16630cddb914088127acb40c" # x86_64-unknown-linux-gnu
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

# The tap lives in the product repository, so `brew tap snaraj/theme
# https://github.com/snaraj/theme` needs no second repository kept in step.
# Homebrew 6 will not use a third-party tap until it is trusted, so the
# install is: `brew trust --formula snaraj/theme/theme`, tap, then install.
# Trust only this formula.
#
# Keep all four URLs and digests bound to reviewed preparation artifacts.
# Main publishes those same bytes after CI passes, then verifies installation.
class Theme < Formula
  desc "Wallpaper and terminal palette driven by one command"
  homepage "https://github.com/snaraj/theme"
  version "0.3.13"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.13/theme-aarch64-apple-darwin.tar.gz"
      sha256 "07c250f67b6da2cf914ea7488b1a2c09d0fa57ac47a37a3060f0e6f220ed1e8e" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.13/theme-x86_64-apple-darwin.tar.gz"
      sha256 "b3f50af96916b71536df413562d0352d08aa2aa73d5fecc7596e83ed9d8978cc" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.13/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "d43465fd72c38a475da532cd76273d5eb32d9e78f1ee7a4d7f171bd02c2cf99d" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.13/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "7596deb832674ce8dee9830042f0d77980fd00aa9360817d6ab090b0f01b8fb3" # x86_64-unknown-linux-gnu
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

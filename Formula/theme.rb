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
  version "0.3.12"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.12/theme-aarch64-apple-darwin.tar.gz"
      sha256 "f9bba956f484fc42d2616e9ea4e8e79262cbeb62d6af580ec4e527c73c25edaf" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.12/theme-x86_64-apple-darwin.tar.gz"
      sha256 "848a47f35f781ded06ba59730a00e32b0e68eff7e4f190748c70ecbfacd3f102" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.12/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "6fdf435190d13fc6a6ecd7efd46c197e2ec9add5e343e30691fb2e4273c1e68a" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.12/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e24e16440fd1fbd393d5c700bde2211f5ac5deac8306d7bab406623cb940e8bb" # x86_64-unknown-linux-gnu
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

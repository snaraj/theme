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
      sha256 "285cdb86904a874edb092df04c5c9b73a68d7c41d89fa114ca45b509f7084c68" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.13/theme-x86_64-apple-darwin.tar.gz"
      sha256 "5f3ef6dfe2e1499aa24865988a5f9b4ca3577774ea2aa6b61be7416335a2272e" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.13/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "1614ab313af09d54a25cd5c0b500ba45716f144c842aa1b91f7b9b01c55d9494" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.13/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "9348ee14314b53a3e24d57ca78eb27031ed339c51c41e9933970b9a63dbbf1e0" # x86_64-unknown-linux-gnu
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

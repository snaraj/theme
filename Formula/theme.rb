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
  version "0.3.11"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.11/theme-aarch64-apple-darwin.tar.gz"
      sha256 "4bb805c055f2424322abfc2dbded9e42e7f6d6a34789f9e19d8a418adb162dfc" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.11/theme-x86_64-apple-darwin.tar.gz"
      sha256 "8ba3b86dca8b81cca465e82a3931104d029f146503daeec99d82b27a90481ca2" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.11/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "21c679ee6f3f5a07d1efb774a354cf22cf0ad30c4fc52749b151ac06f6892a48" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.11/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "6969e305c2ef786fc820e9ea00dc3cd471df6eff2bb776713f8b1adab768d330" # x86_64-unknown-linux-gnu
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

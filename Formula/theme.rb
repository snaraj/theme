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
  version "0.3.8"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.8/theme-aarch64-apple-darwin.tar.gz"
      sha256 "66d62fa3ee6ffdad796ac88848fdb89075f2e40c231213bd079fa1c76aa8dc4e" # aarch64-apple-darwin
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.8/theme-x86_64-apple-darwin.tar.gz"
      sha256 "52b616462cb23df074d6c1f7e80ac95b271abe332fdfe45cda8f949f4649c5f9" # x86_64-apple-darwin
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/snaraj/theme/releases/download/v0.3.8/theme-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "d122569fbaf62467d949f47f657b915e1c366d5841325dea4cea8ddb15109cc1" # aarch64-unknown-linux-gnu
    end
    on_intel do
      url "https://github.com/snaraj/theme/releases/download/v0.3.8/theme-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "f6436e85e0d11f68ebabc9b90dcfcba6aebfdded76ba738c4aa6bb77f94c23c1" # x86_64-unknown-linux-gnu
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

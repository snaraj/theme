#!/usr/bin/env bash
# Repoint Formula/theme.rb at the release just published for $1, then open a
# PR. Nothing here lands on main by itself: the owner's merge is the control
# on what `brew install snaraj/theme/theme` resolves to. Every value comes
# from that release's own SHA256SUMS, and the `# <target>` comments in the
# formula are the anchors — no template, so a hand edit elsewhere survives.
set -eu
tag=$1
branch=formula/$tag
gh release download "$tag" --repo "$GITHUB_REPOSITORY" --pattern SHA256SUMS --output sums
for t in aarch64-apple-darwin x86_64-apple-darwin \
    aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  sha=$(awk -v n="theme-$t.tar.gz" '{sub(/^[*]/, "", $2)} $2 == n {print $1}' sums)
  [ ${#sha} -eq 64 ] || { echo "no sha256 for $t in $tag SHA256SUMS" >&2; exit 1; }
  sed -i "s|^\( *sha256 \"\)[0-9a-f]*\(\" # $t\)\$|\1$sha\2|" Formula/theme.rb
done
sed -i -e "s|/releases/download/v[0-9][0-9.]*/|/releases/download/$tag/|g" \
  -e "s|^  version \".*\"\$|  version \"${tag#v}\"|" Formula/theme.rb
rm -f sums
if git diff --quiet -- Formula/theme.rb; then echo "formula already at $tag"; exit 0; fi
gh api "repos/$GITHUB_REPOSITORY/git/refs" -f ref="refs/heads/$branch" \
  -f sha="$GITHUB_SHA" >/dev/null
gh api "repos/$GITHUB_REPOSITORY/contents/Formula/theme.rb" -X PUT \
  -f message="Point the Homebrew formula at $tag" -f branch="$branch" \
  -f sha="$(git rev-parse "HEAD:Formula/theme.rb")" \
  -f content="$(base64 -w0 Formula/theme.rb)" >/dev/null
gh api "repos/$GITHUB_REPOSITORY/pulls" -f base=main -f head="$branch" \
  -f title="Point the Homebrew formula at $tag" \
  -f body="Regenerated from the \`$tag\` SHA256SUMS by the release chain.
A PR opened with \`GITHUB_TOKEN\` cannot start CI: close and reopen it to run
the formula checks, or read the four sha256 lines against
https://github.com/$GITHUB_REPOSITORY/releases/download/$tag/SHA256SUMS." >/dev/null

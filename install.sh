#!/usr/bin/env bash
# Installs the spendyard CLI from its GitHub releases:
#
#   curl -fsSL https://spendyard.com/install.sh | bash
#
# SPENDYARD_VERSION picks a release, e.g. v0.1.0 (default: the latest), and
# SPENDYARD_INSTALL_DIR where it goes (default: ~/.local/bin). It checks the
# download against the release's checksums before installing. macOS and
# Linux, on amd64 and arm64.
#
# Everything is in main, called on the last line: a download cut short runs
# nothing.

set -euo pipefail

# The download's folder, removed on exit however it ends.
tmp=""
trap 'if [ -n "$tmp" ]; then rm -rf "$tmp"; fi' EXIT

say() { printf '%s\n' "$*" >&2; }
fail() { say "spendyard install: $*"; exit 1; }

main() {
  local repo="spendyard/spendyard"
  local releases="${SPENDYARD_RELEASES_URL:-https://github.com/$repo/releases}"
  local version="${SPENDYARD_VERSION:-latest}"
  local dir="${SPENDYARD_INSTALL_DIR:-$HOME/.local/bin}"

  command -v curl >/dev/null || fail "needs curl"
  command -v tar >/dev/null || fail "needs tar"

  local os arch
  case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) fail "no build for $(uname -s): macOS and Linux only; or: cargo install --git https://github.com/$repo" ;;
  esac

  case "$(uname -m)" in
    x86_64 | amd64) arch=amd64 ;;
    arm64 | aarch64) arch=arm64 ;;
    *) fail "no build for $(uname -m): amd64 and arm64 only" ;;
  esac

  local base
  if [ "$version" = latest ]; then
    base="$releases/latest/download"
  else
    base="$releases/download/$version"
  fi

  local asset="spendyard_${os}_${arch}.tar.gz"
  tmp="$(mktemp -d)"

  say "Downloading spendyard ($version, $os/$arch)…"
  curl -fsSL "$base/$asset" -o "$tmp/$asset" || fail "couldn't download $base/$asset"
  curl -fsSL "$base/checksums.txt" -o "$tmp/checksums.txt" || fail "couldn't download $base/checksums.txt"

  # The archive's checksum, from the release's list.
  local want got
  want="$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/checksums.txt")"
  [ -n "$want" ] || fail "$asset isn't in the release's checksums"

  if command -v sha256sum >/dev/null; then
    got="$(sha256sum "$tmp/$asset" | awk '{ print $1 }')"
  else
    got="$(shasum -a 256 "$tmp/$asset" | awk '{ print $1 }')"
  fi

  [ "$got" = "$want" ] || fail "the download doesn't match its checksum: not installing it"

  tar -xzf "$tmp/$asset" -C "$tmp" spendyard
  mkdir -p "$dir"
  install -m 0755 "$tmp/spendyard" "$dir/spendyard"

  # --version prints the schema on a second line too: the first is enough here.
  local installed
  installed="$("$dir/spendyard" --version 2>/dev/null | head -n 1 || true)"
  say "Installed ${installed:-spendyard} to $dir/spendyard"

  case ":$PATH:" in
    *":$dir:"*) say "Next: spendyard --help" ;;
    *)
      say ""
      say "$dir isn't on your PATH. Add it, e.g. in ~/.zshrc or ~/.bashrc:"
      say ""
      say "  export PATH=\"$dir:\$PATH\""
      say ""
      say "then: spendyard --help"
      ;;
  esac
}

main "$@"

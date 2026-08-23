#!/bin/bash
# Provision the one ShellCheck version this project's checks are defined
# against, and own the version number so nothing else has to repeat it.
#
# ShellCheck's rule set moves between releases, and not only by adding rules:
# 0.9.0 reports SC2015 on an `A && B || C` whose C is an error handler, 0.11.0
# recognizes the handler and stays quiet. So "whatever shellcheck is on PATH"
# makes this check say different things on two machines, and the disagreement
# does not surface until the gate passes locally and the same tree fails in CI
# — the one moment it costs the most.
#
# A pin cannot be satisfied by a package manager. Ubuntu 24.04's archive ships
# 0.9.0, two majors behind, and Homebrew tracks latest, so neither can be asked
# for a specific version. The upstream release binaries can, which is why this
# downloads rather than installs.
#
# Local machines are not asked to run this. A developer's ShellCheck comes from
# their own package manager, and the check that uses it compares that version
# against --print-version and says so when the two have drifted apart. The
# download is for CI, where the runner image's version is not ours to choose.
#
# This file is mirrored verbatim into every spore that runs the check in its own
# CI, and the copies are compared byte for byte, so nothing in it may depend on
# where it sits.

set -euo pipefail

SHELLCHECK_VERSION=0.11.0

usage() {
  cat >&2 <<'EOF'
Usage: install-shellcheck.sh [--print-version | --print-path | --install | --check [ROOT]]

  --print-version  Print the pinned version and exit
  --print-path     Print where --install puts the binary, and exit
  --install        Download the pinned version if it is not already there,
                   then print the binary's path
  --check [ROOT]   ShellCheck every *.sh under ROOT (default: the current
                   directory) with the pinned version
EOF
}

# A user-level cache rather than one inside the checkout. That keeps this file
# byte-identical wherever it sits — it never has to work out which project it
# belongs to — so the copies can be verified by comparison instead of by
# reading them. It also survives the `target/` wipes releases do to reclaim
# disk, and is shared by every project on the machine.
cache_dir() {
  local root="${SHELLCHECK_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/agentfirstkit}"
  printf '%s\n' "$root/shellcheck-$SHELLCHECK_VERSION"
}

# Name the upstream asset for this host. Windows is deliberately absent: see
# the refusal in `install` below.
asset_name() {
  local kernel machine
  kernel="$(uname -s)"
  machine="$(uname -m)"
  case "$kernel" in
    Linux) kernel=linux ;;
    Darwin) kernel=darwin ;;
    *)
      printf 'unsupported\n'
      return 0
      ;;
  esac
  case "$machine" in
    x86_64 | amd64) machine=x86_64 ;;
    arm64 | aarch64) machine=aarch64 ;;
    *)
      printf 'unsupported\n'
      return 0
      ;;
  esac
  printf 'shellcheck-v%s.%s.%s.tar.gz\n' "$SHELLCHECK_VERSION" "$kernel" "$machine"
}

install() {
  local dir binary asset url
  dir="$(cache_dir)"
  binary="$dir/shellcheck"
  if [ -x "$binary" ]; then
    printf '%s\n' "$binary"
    return 0
  fi

  asset="$(asset_name)"
  if [ "$asset" = unsupported ]; then
    # Shell scripts are platform-independent text, so the Linux and macOS legs
    # already check every byte of them. A third run on a platform upstream
    # publishes no tarball for would add no coverage, so this is a refusal
    # rather than a gap — and `scripts/test.sh` says out loud which platforms
    # carry the check.
    echo "install-shellcheck.sh: no pinned ShellCheck build for $(uname -s)/$(uname -m);" >&2
    echo "  the Linux and macOS legs check the same files." >&2
    return 1
  fi

  url="https://github.com/koalaman/shellcheck/releases/download/v$SHELLCHECK_VERSION/$asset"
  echo "Provisioning ShellCheck $SHELLCHECK_VERSION from $url..." >&2
  mkdir -p "$dir"
  # Extracted straight into its final directory, so there is no temporary tree
  # to clean up and therefore no `rm -rf` in a script that runs in CI.
  # --strip-components drops the archive's own `shellcheck-v<version>/` prefix;
  # both GNU and BSD tar accept it.
  # --retry-all-errors because the default --retry ignores a connection reset
  # part-way through the transfer, which is the failure this download actually
  # sees; without it a flaky network reads as a broken pin.
  curl -fsSL --retry 3 --retry-delay 2 --retry-all-errors "$url" -o "$dir/$asset"
  tar -xzf "$dir/$asset" -C "$dir" --strip-components=1
  rm -f "$dir/$asset"
  if [ ! -x "$binary" ]; then
    echo "install-shellcheck.sh: $asset did not contain a shellcheck binary" >&2
    return 1
  fi
  # Prove the download is the version it was asked for before anything trusts
  # it: a redirected or cached archive that is silently a different release
  # would reintroduce exactly the drift this pin exists to remove.
  local got
  got="$("$binary" --version | awk '/^version:/ { print $2 }')"
  if [ "$got" != "$SHELLCHECK_VERSION" ]; then
    echo "install-shellcheck.sh: downloaded ShellCheck reports $got, expected $SHELLCHECK_VERSION" >&2
    return 1
  fi
  printf '%s\n' "$binary"
}

# Resolve the binary to check with: the provisioned pin if CI put one there,
# otherwise the machine's own, which is then held to the same version. Prints
# the path, or the word `skip` on a platform upstream publishes no build for.
resolve_for_check() {
  local binary found
  binary="$(cache_dir)/shellcheck"
  if [ ! -x "$binary" ]; then
    if command -v shellcheck >/dev/null 2>&1; then
      binary=shellcheck
    else
      case "$(uname -s)" in
        Linux | Darwin)
          echo "ShellCheck $SHELLCHECK_VERSION is required and was not found." >&2
          echo "  install it (e.g. brew install shellcheck), or run" >&2
          echo "  $0 --install to fetch the pinned build." >&2
          return 1
          ;;
        *)
          printf 'skip\n'
          return 0
          ;;
      esac
    fi
  fi
  found="$("$binary" --version | awk '/^version:/ { print $2 }')"
  if [ "$found" != "$SHELLCHECK_VERSION" ]; then
    echo "ShellCheck $found is installed, but these checks are defined against $SHELLCHECK_VERSION." >&2
    echo "  Its rules differ between releases, so a tree that passes here would not" >&2
    echo "  necessarily pass CI. Upgrade to $SHELLCHECK_VERSION, or, if $found is the" >&2
    echo "  newer one, move the pin in this file and re-run." >&2
    return 1
  fi
  printf '%s\n' "$binary"
}

check() {
  local root="${1:-.}"
  local binary files script
  binary="$(resolve_for_check)" || return 1
  if [ "$binary" = skip ]; then
    # Shell scripts are platform-independent text and the Linux and macOS runs
    # read every byte of them, so coverage is complete without this platform.
    # Said out loud, because a skip that prints nothing looks like a pass.
    echo "ShellCheck skipped on $(uname -s): the Linux and macOS runs check the same files"
    return 0
  fi

  # One argument list, not a loop: shellcheck reads stdin, so a `while read`
  # loop feeding it filenames has the rest of its input swallowed by the first
  # call and silently checks a single file.
  files=()
  while IFS= read -r script; do
    [ -n "$script" ] || continue
    files+=("$script")
  done < <(find "$root" \
    \( -name .git -o -name target -o -name node_modules -o -name .venv \
      -o -name dist -o -name build -o -name __pycache__ \) -prune -o \
    -name '*.sh' -type f -print 2>/dev/null | LC_ALL=C sort)

  if [ "${#files[@]}" -eq 0 ]; then
    echo "ShellCheck: no shell scripts under $root"
    return 0
  fi
  # A checked script may use `source=/dev/stdin` for generated runtime code.
  # Under `-x`, inheriting an interactive terminal makes ShellCheck wait for
  # input forever; CI happens to provide EOF, so close stdin explicitly and
  # keep local and CI behavior identical.
  "$binary" -x "${files[@]}" </dev/null || return 1
  echo "ShellCheck $SHELLCHECK_VERSION clean: ${#files[@]} script(s)"
}

case "${1:---install}" in
  --print-version)
    printf '%s\n' "$SHELLCHECK_VERSION"
    ;;
  --print-path)
    printf '%s\n' "$(cache_dir)/shellcheck"
    ;;
  --install)
    install
    ;;
  --check)
    shift
    check "$@"
    ;;
  -h | --help)
    usage
    ;;
  *)
    usage
    exit 2
    ;;
esac

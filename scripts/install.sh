#!/bin/sh
# Merry installer for Linux and macOS.
# Usage:
#   curl -fsSL https://github.com/locez/merry/releases/latest/download/install.sh | sh
# Environment overrides:
#   MERRY_VERSION      Release tag to install (e.g. v0.1.0); defaults to latest
#   MERRY_INSTALL_DIR  Destination directory; defaults to ${XDG_BIN_HOME:-$HOME/.local/bin}

set -eu

REPO="locez/merry"

say() {
    printf '%s\n' "$*"
}

err() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        err "required command not found: $1"
    fi
}

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)
            case "$arch" in
                x86_64 | amd64) echo "x86_64-unknown-linux-musl" ;;
                aarch64 | arm64) echo "aarch64-unknown-linux-musl" ;;
                *) err "unsupported Linux architecture: $arch" ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                arm64 | aarch64) echo "aarch64-apple-darwin" ;;
                x86_64) echo "x86_64-apple-darwin" ;;
                *) err "unsupported macOS architecture: $arch" ;;
            esac
            ;;
        *)
            err "unsupported operating system: $os (on Windows, use install.ps1)"
            ;;
    esac
}

resolve_version() {
    if [ -n "${MERRY_VERSION:-}" ]; then
        case "$MERRY_VERSION" in
            v*) printf '%s\n' "$MERRY_VERSION" ;;
            *) printf 'v%s\n' "$MERRY_VERSION" ;;
        esac
        return
    fi

    # Follow /releases/latest redirect to avoid unauthenticated GitHub API rate limits.
    final_url="$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest")"
    tag="${final_url##*/}"
    case "$tag" in
        v*) printf '%s\n' "$tag" ;;
        *) err "failed to resolve latest Merry release tag from ${final_url}" ;;
    esac
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        err "neither sha256sum nor shasum found to verify download integrity"
    fi
}

main() {
    need_cmd curl
    need_cmd tar
    need_cmd awk
    need_cmd mktemp

    target="$(detect_target)"
    version="$(resolve_version)"
    install_dir="${MERRY_INSTALL_DIR:-${XDG_BIN_HOME:-$HOME/.local/bin}}"
    archive="merry-${version}-${target}.tar.gz"
    base_url="https://github.com/${REPO}/releases/download/${version}"

    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT INT TERM

    say "Downloading Merry ${version} (${target})..."
    curl -fsSL "${base_url}/${archive}" -o "${tmpdir}/${archive}"
    curl -fsSL "${base_url}/SHA256SUMS.txt" -o "${tmpdir}/SHA256SUMS.txt"

    expected="$(awk -v file="$archive" '$2 == file { print $1; exit }' "${tmpdir}/SHA256SUMS.txt")"
    if [ -z "$expected" ]; then
        err "checksum entry for ${archive} not found in SHA256SUMS.txt"
    fi

    actual="$(sha256_file "${tmpdir}/${archive}")"
    if [ "$actual" != "$expected" ]; then
        err "SHA256 mismatch for ${archive} (expected ${expected}, got ${actual})"
    fi

    tar -xzf "${tmpdir}/${archive}" -C "$tmpdir"
    src_bin="${tmpdir}/merry-${version}-${target}/merry"
    if [ ! -f "$src_bin" ]; then
        err "archive did not contain expected binary: ${src_bin}"
    fi

    mkdir -p "$install_dir"
    tmp_dest="${install_dir}/.merry.tmp.$$"
    cp "$src_bin" "$tmp_dest"
    chmod 755 "$tmp_dest"
    mv "$tmp_dest" "${install_dir}/merry"

    say "Installed merry ${version} to ${install_dir}/merry"

    case ":${PATH}:" in
        *":${install_dir}:"*) ;;
        *)
            say ""
            say "Note: ${install_dir} is not in your PATH. Add it with:"
            say "  export PATH=\"${install_dir}:\$PATH\""
            ;;
    esac
}

main "$@"

#!/bin/sh
# Install one official Claude Code linux-arm64 (glibc) build under /out.
#
#   install-claude.sh <version> <sha512-integrity> <sha256-of-the-executable>
#
# Both digests are checked BEFORE the executable is put where a tool can run
# it. The sha512 is npm's own `dist.integrity` for
# `@anthropic-ai/claude-code-linux-arm64`, in the `sha512-<base64>` spelling
# `npm view` prints, so the pinned value can be compared with the registry
# without re-encoding it. The sha256 is of `package/claude` itself, which is
# the byte string the lane's README and the promotion receipts quote: the
# tarball digest says the download was not tampered with, and the executable
# digest says which binary the numbers were measured against.
#
# No node runtime is involved. `package/claude` is a native ELF.
set -eu

version="$1"
want_sha512="$2"
want_sha256="$3"

url="https://registry.npmjs.org/@anthropic-ai/claude-code-linux-arm64/-/claude-code-linux-arm64-${version}.tgz"
tarball="/tmp/claude-code-linux-arm64-${version}.tgz"

curl -fsSL --retry 3 -o "$tarball" "$url"

got_sha512="sha512-$(openssl dgst -sha512 -binary "$tarball" | openssl base64 -A)"
if [ "$got_sha512" != "$want_sha512" ]; then
    echo "integrity mismatch for ${version}" >&2
    echo "  want ${want_sha512}" >&2
    echo "  got  ${got_sha512}" >&2
    exit 1
fi

work="/tmp/unpack-${version}"
rm -rf "$work"
mkdir -p "$work"
tar -xzf "$tarball" -C "$work" package/claude
rm -f "$tarball"

got_sha256="$(sha256sum "$work/package/claude" | cut -d' ' -f1)"
if [ "$got_sha256" != "$want_sha256" ]; then
    echo "executable sha256 mismatch for ${version}" >&2
    echo "  want ${want_sha256}" >&2
    echo "  got  ${got_sha256}" >&2
    exit 1
fi

mkdir -p "/out/${version}"
mv "$work/package/claude" "/out/${version}/claude"
chmod 0755 "/out/${version}/claude"
rm -rf "$work"

printf '%s  %s\n' "$got_sha256" "claude-code-linux-arm64 ${version}" >> /out/SHA256SUMS

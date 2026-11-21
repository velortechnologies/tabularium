#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml)
test -n "$version"
remote_tag=$(git ls-remote --tags origin "refs/tags/v$version")
if test -n "$remote_tag"; then
    echo "Release v$version already exists; refusing to overwrite it" >&2
    exit 1
fi
for command in cargo cross docker reprepro gpg just; do command -v "$command" >/dev/null; done
cargo audit --version
docker info >/dev/null

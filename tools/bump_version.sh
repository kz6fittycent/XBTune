#!/usr/bin/env bash
# Bump the workspace version for the next release. Releases count in threes:
# 0.1.0 -> 0.1.1 -> 0.1.2 -> 0.2.0 -> 0.2.1 ... (the owner's scheme,
# 2026-10-22). The snap's version is exactly this version, and
# security-rebuild.yml finds a release's commit by it, so bump once per
# release, in the commit that is pushed to main.
#   tools/bump_version.sh          # from the repo root; prints old -> new
#   tools/bump_version.sh --check  # only print what the next version would be
set -euo pipefail

current="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
IFS=. read -r major minor patch <<<"$current"
if (( patch >= 2 )); then
    next="${major}.$((minor + 1)).0"
else
    next="${major}.${minor}.$((patch + 1))"
fi

if [[ "${1:-}" == "--check" ]]; then
    echo "$current -> $next"
    exit 0
fi

sed -i "0,/^version = \"${current}\"/s//version = \"${next}\"/" Cargo.toml
cargo update --workspace --quiet
echo "$current -> $next"

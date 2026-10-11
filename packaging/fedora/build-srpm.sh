#!/usr/bin/env bash
# Prepare an offline-buildable source RPM for Fedora COPR.
set -euo pipefail
if [[ $# -gt 1 ]]; then
    printf 'Usage: %s [OUTPUT_DIRECTORY]\n' "$0" >&2
    exit 2
fi
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
for tool in cargo curl rpmbuild rpmspec sha256sum tar xz; do
    command -v "$tool" >/dev/null || { printf 'Missing tool: %s\n' "$tool" >&2; exit 1; }
done
version=$(rpmspec -q --srpm --qf '%{version}\n' "$repo/packaging/fedora/grava.spec")
name=Grava-$version.tar.gz
expected=$(awk -v name="$name" '$2 == name {print $1}' "$repo/packaging/fedora/sources")
if [[ ! $expected =~ ^[0-9a-f]{64}$ ]]; then
    printf 'Add the verified %s checksum to packaging/fedora/sources first.\n' "$name" >&2
    exit 1
fi
output=${1:-$repo/dist}
mkdir -p "$output"
output=$(cd -- "$output" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/grava-srpm.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work"/{SOURCES,SPECS,SRPMS,BUILD,BUILDROOT,RPMS}
curl --fail --location --retry 3 \
    "https://github.com/QinZinn/Grava/archive/refs/tags/v$version.tar.gz" \
    --output "$work/SOURCES/$name"
(cd "$work/SOURCES"; printf '%s  %s\n' "$expected" "$name" | sha256sum -c -)
tar -xf "$work/SOURCES/$name" -C "$work"
(
    cd "$work/Grava-$version"
    cargo vendor --locked vendor > "$work/vendor-config.toml"
    tar -cJf "$work/SOURCES/grava-$version-vendor.tar.xz" vendor
)
cp "$repo/packaging/fedora/grava.spec" "$work/SPECS/"
rpmbuild -bs --define "_topdir $work" "$work/SPECS/grava.spec"
cp "$work/SRPMS/"*.src.rpm "$output/"
printf 'Source RPM ready for COPR: %s\n' "$output"

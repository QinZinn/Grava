#!/usr/bin/env bash
# Wrap a staged static musl release for a local Void repository.
set -euo pipefail
if [[ $# != 4 ]]; then
    printf 'Usage: %s STAGE VERSION VOID_ARCH OUTPUT_DIRECTORY\n' "$0" >&2
    exit 2
fi
stage=$(realpath "$1")
version=$2
arch=$3
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'Unsupported release version: %s\n' "$version" >&2
    exit 2
fi
case "$arch" in
    x86_64 | x86_64-musl) machine='Advanced Micro Devices X86-64' ;;
    aarch64 | aarch64-musl) machine=AArch64 ;;
    *) printf 'Unsupported Void architecture: %s\n' "$arch" >&2; exit 2 ;;
esac
for tool in xbps-create readelf; do
    command -v "$tool" >/dev/null || { printf 'Missing tool: %s\n' "$tool" >&2; exit 1; }
done
for path in usr/bin/grava usr/share/licenses/grava/LICENSE usr/share/licenses/grava/THIRD-PARTY-LICENSES.txt; do
    test -f "$stage/$path" || { printf 'Missing staged file: %s\n' "$path" >&2; exit 1; }
done
header=$(LC_ALL=C readelf -h "$stage/usr/bin/grava")
program=$(LC_ALL=C readelf -l "$stage/usr/bin/grava")
dynamic=$(LC_ALL=C readelf -d "$stage/usr/bin/grava")
if [[ $header != *"Machine:"*"$machine"* || $program == *INTERP* || $dynamic == *NEEDED* ]]; then
    printf 'Stage must contain a static binary matching %s.\n' "$arch" >&2
    exit 2
fi
mkdir -p "$4"
output=$(realpath "$4")
if [[ $output == "$stage" || $output == "$stage/"* ]]; then
    printf 'Output must be outside the stage.\n' >&2
    exit 2
fi
cd "$output"
xbps-create --architecture "$arch" --pkgver "grava-${version}_1" \
    --desc 'Terminal audio visualizer with Chladni sand figures' \
    --maintainer 'Zinnn <139782520+QinZinn@users.noreply.github.com>' \
    --homepage https://github.com/QinZinn/Grava \
    --license 'MIT AND (Apache-2.0 OR MIT) AND (Unlicense OR MIT) AND (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT) AND Unicode-3.0 AND BSD-2-Clause AND BSD-3-Clause AND Zlib' \
    --compression gzip "$stage"
printf 'Void package ready: %s/grava-%s_1.%s.xbps\n' "$output" "$version" "$arch"

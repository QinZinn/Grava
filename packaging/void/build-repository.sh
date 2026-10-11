#!/usr/bin/env bash
# Repackage verified static release archives and sign a remote XBPS repository.
set -euo pipefail
if [[ $# != 4 ]]; then
    printf 'Usage: %s RELEASE_DIRECTORY VERSION OUTPUT_DIRECTORY RSA_PRIVATE_KEY\n' "$0" >&2
    exit 2
fi
input=$(realpath "$1")
version=$2
output=$(realpath -m "$3")
key=$(realpath "$4")
helper=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/build-binary.sh
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'Unsupported release version: %s\n' "$version" >&2
    exit 2
fi
if [[ $output == "$input" || $output == "$input/"* ]]; then
    printf 'Output must be outside the release directory.\n' >&2
    exit 2
fi
for tool in xbps-rindex openssl tar sha256sum; do
    command -v "$tool" >/dev/null || { printf 'Missing tool: %s\n' "$tool" >&2; exit 1; }
done
test -f "$key"
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$output"
# Refuse stale metadata: each deployment represents one immutable release.
if [[ -n $(find "$output" -mindepth 1 -maxdepth 1 -print -quit) ]]; then
    printf 'Output directory must be empty.\n' >&2
    exit 2
fi
for machine in x86_64 aarch64; do
    name=grava-v$version-$machine-unknown-linux-musl
    archive=$name.tar.gz
    checksum=$(awk -v file="$archive" '$2 == file { print }' "$input/SHA256SUMS")
    if [[ $(printf '%s\n' "$checksum" | wc -l) != 1 || -z $checksum ]]; then
        printf 'Missing or duplicate checksum for %s\n' "$archive" >&2
        exit 1
    fi
    (cd "$input" && printf '%s\n' "$checksum" | sha256sum -c -)
    tar --no-same-owner --no-same-permissions -xzf "$input/$archive" -C "$work"
    source=$work/$name
    stage=$work/stage-$machine
    install -Dm0755 "$source/grava" "$stage/usr/bin/grava"
    for license in LICENSE THIRD-PARTY-LICENSES.txt; do
        install -Dm0644 "$source/$license" "$stage/usr/share/licenses/grava/$license"
    done
    for doc in README.md config.toml; do
        install -Dm0644 "$source/$doc" "$stage/usr/share/doc/grava/$doc"
    done
    install -Dm0644 "$source/grava.1" "$stage/usr/share/man/man1/grava.1"
    install -Dm0644 "$source/completions/grava.bash" "$stage/usr/share/bash-completion/completions/grava"
    install -Dm0644 "$source/completions/grava.zsh" "$stage/usr/share/zsh/site-functions/_grava"
    install -Dm0644 "$source/completions/grava.fish" "$stage/usr/share/fish/vendor_completions.d/grava.fish"
    for arch in "$machine" "$machine-musl"; do
        "$helper" "$stage" "$version" "$arch" "$output"
        package=$output/grava-${version}_1.$arch.xbps
        XBPS_ARCH=$arch xbps-rindex -a "$package"
        XBPS_ARCH=$arch xbps-rindex --privkey "$key" --sign --signedby 'Zinnn (grava)' "$output"
        XBPS_ARCH=$arch xbps-rindex --privkey "$key" --sign-pkg "$package"
    done
done
openssl rsa -in "$key" -pubout -out "$output/grava-repository-key.pem" 2>/dev/null
(
    cd "$output"
    sha256sum ./*.xbps > SHA256SUMS
)
printf 'Signed Void repository: %s\n' "$output"

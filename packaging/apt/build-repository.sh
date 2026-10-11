#!/usr/bin/env bash
# Generate a signed APT repository from released amd64/arm64 DEBs.
set -euo pipefail
if [[ $# != 3 ]]; then
    printf 'Usage: %s DEB_DIRECTORY OUTPUT_DIRECTORY GPG_SIGNING_KEY\n' "$0" >&2
    exit 2
fi
for tool in apt-ftparchive dpkg-deb gpg gzip sha256sum; do
    command -v "$tool" >/dev/null || { printf 'Missing tool: %s\n' "$tool" >&2; exit 1; }
done
input=$(realpath "$1")
output=$(realpath -m "$2")
key=$3
if [[ $output == "$input" || $output == "$input/"* ]]; then
    printf 'Output must be separate from the input package directory.\n' >&2
    exit 2
fi
gpg --batch --list-secret-keys "$key" >/dev/null
mkdir -p "$output/pool/main/g/grava"
shopt -s nullglob
packages=("$input"/*.deb)
if [[ ${#packages[@]} == 0 ]]; then
    printf 'No DEB packages found in %s\n' "$input" >&2
    exit 1
fi
for package in "${packages[@]}"; do
    name=$(dpkg-deb --field "$package" Package)
    arch=$(dpkg-deb --field "$package" Architecture)
    if [[ $name != grava || ! $arch =~ ^(amd64|arm64)$ ]]; then
        printf 'Unexpected package: %s (%s, %s)\n' "$package" "$name" "$arch" >&2
        exit 1
    fi
    cp -- "$package" "$output/pool/main/g/grava/"
done
cd "$output"
for arch in amd64 arm64; do
    directory=dists/stable/main/binary-$arch
    mkdir -p "$directory"
    apt-ftparchive --arch "$arch" packages pool > "$directory/Packages"
    gzip -n -9 -c "$directory/Packages" > "$directory/Packages.gz"
done
apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=grava \
    -o APT::FTPArchive::Release::Label=grava \
    -o APT::FTPArchive::Release::Suite=stable \
    -o APT::FTPArchive::Release::Codename=stable \
    -o 'APT::FTPArchive::Release::Architectures=amd64 arm64' \
    -o APT::FTPArchive::Release::Components=main \
    -o 'APT::FTPArchive::Release::Description=grava upstream static packages' \
    release dists/stable > dists/stable/Release.new
mv dists/stable/Release.new dists/stable/Release
gpg --batch --yes --local-user "$key" --digest-algo SHA256 \
    --clearsign --output dists/stable/InRelease dists/stable/Release
gpg --batch --yes --local-user "$key" --digest-algo SHA256 --armor \
    --detach-sign --output dists/stable/Release.gpg dists/stable/Release
gpg --batch --export "$key" > grava-archive-keyring.gpg
touch .nojekyll
printf 'Signed APT repository: %s\n' "$output"

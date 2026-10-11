#!/usr/bin/env bash
# Package a prepared, statically linked release tree without changing it.
set -euo pipefail

if [[ $# -ne 4 ]]; then
    printf 'Usage: %s STAGE VERSION ARCH OUTPUT_DIR\n' "$0" >&2
    exit 2
fi

stage=$(realpath "$1")
version=$2
architecture=$3
output_dir=$4

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([~+.-][0-9A-Za-z.+~-]+)?$ ]]; then
    printf 'Unsupported release version: %s\n' "$version" >&2
    exit 2
fi
case "$architecture" in
    amd64) machine='Advanced Micro Devices X86-64' ;;
    arm64) machine='AArch64' ;;
    *) printf 'Unsupported Debian architecture: %s\n' "$architecture" >&2; exit 2 ;;
esac
if ! command -v readelf >/dev/null 2>&1; then
    printf 'Missing build tool: readelf\n' >&2
    exit 2
fi
if [[ ! -f $stage/usr/bin/grava || -L $stage/usr/bin/grava ]]; then
    printf 'Stage must contain a regular usr/bin/grava binary.\n' >&2
    exit 2
fi
for path in \
    usr/share/man/man1/grava.1.gz \
    usr/share/bash-completion/completions/grava \
    usr/share/zsh/site-functions/_grava \
    usr/share/fish/vendor_completions.d/grava.fish \
    usr/share/doc/grava/README.md \
    usr/share/doc/grava/config.toml \
    usr/share/licenses/grava/LICENSE \
    usr/share/licenses/grava/THIRD-PARTY-LICENSES.txt; do
    if [[ ! -f $stage/$path ]]; then
        printf 'Missing staged file: %s\n' "$path" >&2
        exit 2
    fi
done
if [[ -e $stage/DEBIAN ]]; then
    printf 'Stage must not already contain DEBIAN package metadata.\n' >&2
    exit 2
fi
elf_header=$(LC_ALL=C readelf -h "$stage/usr/bin/grava")
program_headers=$(LC_ALL=C readelf -l "$stage/usr/bin/grava")
dynamic_entries=$(LC_ALL=C readelf -d "$stage/usr/bin/grava")
if [[ $elf_header != *"Machine:"*"$machine"* ]]; then
    printf 'Staged binary does not match Debian architecture %s.\n' "$architecture" >&2
    exit 2
fi
if [[ $program_headers == *INTERP* || $dynamic_entries == *NEEDED* ]]; then
    printf 'Debian release packaging requires a fully static binary.\n' >&2
    exit 2
fi

mkdir -p "$output_dir"
output_dir=$(realpath "$output_dir")
if [[ $output_dir == "$stage" || $output_dir == "$stage/"* ]]; then
    printf 'Output directory must be outside the staged release tree.\n' >&2
    exit 2
fi
work=$(mktemp -d "${TMPDIR:-/tmp}/grava-deb.XXXXXXXX")
trap 'rm -rf "$work"' EXIT
package=$work/package
mkdir "$package"
cp -a --no-preserve=ownership "$stage/." "$package/"
find "$package" -type d -exec chmod 0755 {} +
find "$package" -type f -exec chmod 0644 {} +
chmod 0755 "$package/usr/bin/grava"
mkdir -p "$package/DEBIAN" "$package/usr/share/doc/grava"

# Debian requires the distribution licenses in /usr/share/doc/PACKAGE/copyright.
# Retain the individual license files too, as in the release tarball and RPM.
copyright=$package/usr/share/doc/grava/copyright
cat > "$copyright" <<'EOF'
grava
Upstream: https://github.com/QinZinn/Grava
Source: https://github.com/QinZinn/Grava

This binary distribution includes statically linked third-party Rust crates.
The complete project and dependency copyright and license notices follow.
Individual copies are also installed under /usr/share/licenses/grava/.
EOF
while IFS= read -r -d '' license; do
    relative=${license#"$package/usr/share/licenses/grava/"}
    printf '\n----- %s -----\n\n' "$relative" >> "$copyright"
    cat "$license" >> "$copyright"
    printf '\n' >> "$copyright"
done < <(find "$package/usr/share/licenses/grava" -type f -print0 | LC_ALL=C sort -z)
chmod 0644 "$copyright"

installed_size=$(du -sk "$package/usr" | cut -f1)
cat > "$package/DEBIAN/control" <<EOF
Package: grava
Version: $version-1
Section: sound
Priority: optional
Architecture: $architecture
Maintainer: Zinnn <139782520+QinZinn@users.noreply.github.com>
Installed-Size: $installed_size
Recommends: pipewire-pulse | pulseaudio
Homepage: https://github.com/QinZinn/Grava
Description: terminal audio visualizer with Chladni sand figures
 Captures desktop playback through the PulseAudio protocol, including
 PipeWire's PulseAudio compatibility server, and draws animated sand figures.
 Includes deterministic demo, stdin and FIFO inputs, themes, a manual page,
 and shell completions. The executable is statically linked with musl.
EOF
chmod 0644 "$package/DEBIAN/control"
(
    cd "$package"
    find usr -type f -print0 | LC_ALL=C sort -z | xargs -0 md5sum
) > "$package/DEBIAN/md5sums"
chmod 0644 "$package/DEBIAN/md5sums"

# dpkg-deb uses this standard variable to clamp timestamps in both archives.
# A fixed default also keeps ad-hoc builds repeatable; release tooling supplies
# the source commit's timestamp.
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}
if [[ ! $SOURCE_DATE_EPOCH =~ ^[0-9]+$ ]]; then
    printf 'SOURCE_DATE_EPOCH must be a nonnegative integer.\n' >&2
    exit 2
fi

filename=grava_${version}-1_${architecture}.deb
if command -v dpkg-deb >/dev/null 2>&1; then
    dpkg-deb --root-owner-group --uniform-compression -Zgzip -z9 \
        --build "$package" "$work/$filename"
else
    engine=${GRAVA_CONTAINER_ENGINE:-podman}
    if ! command -v "$engine" >/dev/null 2>&1; then
        printf 'Install dpkg-deb or provide a container engine (default: podman).\n' >&2
        exit 2
    fi
    image=${GRAVA_DEB_IMAGE:-docker.io/library/debian:stable-slim}
    "$engine" run --rm --network=none \
        --env "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH" \
        --volume "$work:/work:Z" "$image" \
        dpkg-deb --root-owner-group --uniform-compression -Zgzip -z9 \
        --build /work/package "/work/$filename"
fi
mv -f "$work/$filename" "$output_dir/$filename"
printf 'Created %s\n' "$output_dir/$filename"

#!/usr/bin/env bash
# Build native static release artifacts without installing anything on the host.
set -euo pipefail

if [[ $# -gt 2 ]]; then
    printf 'Usage: %s [RUST_TARGET] [OUTPUT_DIR]\n' "$0" >&2
    exit 2
fi
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
host_arch=$(uname -m)
target=${1:-$host_arch-unknown-linux-musl}
case "$target" in
    x86_64-unknown-linux-musl) rpm_arch=x86_64; deb_arch=amd64 ;;
    aarch64-unknown-linux-musl) rpm_arch=aarch64; deb_arch=arm64 ;;
    *) printf 'Unsupported release target: %s\n' "$target" >&2; exit 2 ;;
esac
if [[ $host_arch != "$rpm_arch" ]]; then
    printf 'Build %s on a native %s runner; the release binary must run to generate documentation.\n' "$target" "$rpm_arch" >&2
    exit 2
fi
for command in cargo rustc python3 readelf strip tar gzip sha256sum; do
    if ! command -v "$command" >/dev/null 2>&1; then
        printf 'Missing build tool: %s\n' "$command" >&2
        exit 1
    fi
done

cd "$repo"
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'Release packages require a stable MAJOR.MINOR.PATCH version: %s\n' "$version" >&2
    exit 2
fi
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}
if [[ ! $SOURCE_DATE_EPOCH =~ ^[0-9]+$ ]]; then
    printf 'SOURCE_DATE_EPOCH must be a nonnegative integer.\n' >&2
    exit 2
fi
export TZ=UTC
output_dir=${2:-$repo/dist}
mkdir -p -- "$output_dir"
output_dir=$(cd -- "$output_dir" && pwd)
build_root=${CARGO_TARGET_DIR:-$repo/target}
mkdir -p -- "$build_root"
build_root=$(cd -- "$build_root" && pwd)
work=$build_root/packages/$target
stage=$work/root
mkdir -p -- "$work"

cargo build --locked --release --target "$target" --target-dir "$build_root"
binary=$build_root/$target/release/grava
program_headers=$(LC_ALL=C readelf -l "$binary")
dynamic_entries=$(LC_ALL=C readelf -d "$binary")
if [[ $program_headers == *INTERP* || $dynamic_entries == *NEEDED* ]]; then
    printf 'Release packaging requires a fully static musl binary.\n' >&2
    exit 1
fi
binary_version=$("$binary" --version)
if [[ ${binary_version%%$'\n'*} != "grava $version" ]]; then
    printf 'Binary version does not match Cargo.toml.\n' >&2
    exit 1
fi
"$binary" --config "$repo/assets/config.toml" --headless -i demo \
    --frames 60 --size 60x30 --seed 1 --no-color > "$work/headless.txt"

rm -rf -- "$stage"
install -Dm0755 "$binary" "$stage/usr/bin/grava"
strip --strip-unneeded "$stage/usr/bin/grava"
install -Dm0644 LICENSE "$stage/usr/share/licenses/grava/LICENSE"
install -Dm0644 README.md "$stage/usr/share/doc/grava/README.md"
install -Dm0644 assets/config.toml "$stage/usr/share/doc/grava/config.toml"
mkdir -p -- "$stage/usr/share/man/man1" \
    "$stage/usr/share/bash-completion/completions" \
    "$stage/usr/share/zsh/site-functions" \
    "$stage/usr/share/fish/vendor_completions.d"
"$binary" --man | gzip -n -9 > "$stage/usr/share/man/man1/grava.1.gz"
"$binary" --completions bash > "$stage/usr/share/bash-completion/completions/grava"
"$binary" --completions zsh > "$stage/usr/share/zsh/site-functions/_grava"
"$binary" --completions fish > "$stage/usr/share/fish/vendor_completions.d/grava.fish"
python3 packaging/licenses.py "$stage/usr/share/licenses/grava/THIRD-PARTY-LICENSES.txt" --target "$target"
find "$stage" -type d -exec chmod 0755 {} +
find "$stage" -type f ! -path "$stage/usr/bin/grava" -exec chmod 0644 {} +
find "$stage" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +

packaging/rpm/build.sh "$stage" "$version" "$rpm_arch" "$output_dir"
packaging/debian/build.sh "$stage" "$version" "$deb_arch" "$output_dir"

name=grava-v$version-$target
archive=$work/archive/$name
rm -rf -- "$work/archive"
mkdir -p -- "$archive/completions"
cp -- "$stage/usr/bin/grava" "$archive/"
cp -- "$stage/usr/share/licenses/grava/"* "$archive/"
cp -- "$stage/usr/share/doc/grava/"* "$archive/"
gzip -dc "$stage/usr/share/man/man1/grava.1.gz" > "$archive/grava.1"
cp -- "$stage/usr/share/bash-completion/completions/grava" "$archive/completions/grava.bash"
cp -- "$stage/usr/share/zsh/site-functions/_grava" "$archive/completions/grava.zsh"
cp -- "$stage/usr/share/fish/vendor_completions.d/grava.fish" "$archive/completions/grava.fish"
tar --sort=name --mtime="@$SOURCE_DATE_EPOCH" --owner=0 --group=0 --numeric-owner \
    -C "$work/archive" -cf - "$name" | gzip -n -9 > "$output_dir/$name.tar.gz"
(
    cd "$output_dir"
    for artifact in "$name.tar.gz" "grava-$version-1.$rpm_arch.rpm" "grava_$version-1_$deb_arch.deb"; do
        sha256sum "$artifact" > "$artifact.sha256"
    done
    cat -- *.sha256 > SHA256SUMS
)
printf 'Release artifacts: %s\n' "$output_dir"

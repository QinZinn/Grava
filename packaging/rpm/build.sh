#!/usr/bin/env bash
# Package an already staged static release without root privileges.
set -euo pipefail

if [[ $# != 4 ]]; then
    printf 'Usage: %s STAGE VERSION ARCH OUTPUT_DIR\n' "$0" >&2
    exit 2
fi

stage=$1
version=$2
arch=$3
output_dir=$4
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'RPM version must be a stable MAJOR.MINOR.PATCH version: %s\n' "$version" >&2
    exit 2
fi
case "$arch" in
    x86_64) machine='Advanced Micro Devices X86-64' ;;
    aarch64) machine='AArch64' ;;
    *) printf 'Unsupported RPM architecture: %s\n' "$arch" >&2; exit 2 ;;
esac

for command in rpmbuild rpm readelf tar gzip; do
    if ! command -v "$command" >/dev/null 2>&1; then
        printf 'Missing build tool: %s\n' "$command" >&2
        exit 1
    fi
done

stage=$(cd -- "$stage" && pwd)
for path in \
    usr/bin/grava \
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
        exit 1
    fi
done
if [[ ! -x $stage/usr/bin/grava ]]; then
    printf 'Staged grava binary is not executable.\n' >&2
    exit 1
fi

elf_header=$(LC_ALL=C readelf -h "$stage/usr/bin/grava")
program_headers=$(LC_ALL=C readelf -l "$stage/usr/bin/grava")
dynamic_entries=$(LC_ALL=C readelf -d "$stage/usr/bin/grava")
if [[ $elf_header != *"Machine:"*"$machine"* ]]; then
    printf 'Staged binary does not match RPM architecture %s.\n' "$arch" >&2
    exit 1
fi
if [[ $program_headers == *INTERP* || $dynamic_entries == *NEEDED* ]]; then
    printf 'RPM release packaging requires a fully static binary.\n' >&2
    exit 1
fi

mkdir -p -- "$output_dir"
output_dir=$(cd -- "$output_dir" && pwd)
build_dir=$(mktemp -d "${TMPDIR:-/tmp}/grava-rpm.XXXXXXXX")
trap 'rm -rf -- "$build_dir"' EXIT
mkdir -p -- "$build_dir"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

tar --sort=name --owner=0 --group=0 --numeric-owner \
    -C "$stage" -cf - usr | gzip -n >"$build_dir/SOURCES/grava-$version-payload.tar.gz"
cp -- "$script_dir/grava-binary.spec" "$build_dir/SPECS/"

rpmbuild -bb --target "$arch" \
    --define "_topdir $build_dir" \
    --define "grava_version $version" \
    --define '_rpmformat 4' \
    --define '_binary_payload w9.gzdio' \
    --define '_buildhost reproducible' \
    "$build_dir/SPECS/grava-binary.spec"

package="$build_dir/RPMS/$arch/grava-$version-1.$arch.rpm"
if [[ ! -f $package ]]; then
    printf 'rpmbuild did not produce the expected package: %s\n' "$package" >&2
    exit 1
fi
cp -- "$package" "$output_dir/"
printf '%s\n' "$output_dir/$(basename -- "$package")"

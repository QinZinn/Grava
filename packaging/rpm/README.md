# Upstream binary RPM

`build.sh` wraps a prebuilt static release in an RPM without root privileges:

```sh
SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) \
  packaging/rpm/build.sh STAGE 0.1.0 x86_64 OUTPUT_DIR
```

Use `x86_64` or `aarch64` for the architecture. The output is
`grava-VERSION-1.ARCH.rpm`. The top-level `packaging/build.sh` prepares the stage
and calls this helper; normally use that command instead of assembling a stage
by hand.

The stage contains `/usr/bin/grava`, a gzipped manual page, Bash/Zsh/Fish
completions, documentation under `/usr/share/doc/grava`, and both `LICENSE` and
`THIRD-PARTY-LICENSES.txt` under `/usr/share/licenses/grava`. The helper checks
the binary's ELF architecture and rejects an interpreter or shared-library
dependencies. RPM records files as `root:root`; the executable is mode `0755`,
data files are `0644`, and directories are `0755`.

Build tools are Bash, GNU coreutils, GNU tar, gzip, binutils (`readelf`), and
RPM build tools (`rpm-build` on Fedora, `rpm` on Debian/Ubuntu). Packages use
RPM v4 and gzip payloads for compatibility. Export `SOURCE_DATE_EPOCH` to keep
the build timestamp stable; identical staged files then produce identical
packages. Packaging preserves the already stripped binary and compressed
manual, and does not install files on the build host.

The package recommends `pipewire-pulseaudio` or `pulseaudio` for desktop audio
capture. Demo and raw PCM inputs also work without a sound server. This is an
unsigned upstream release package, separate from the source/COPR template in
`packaging/fedora/grava.spec`.

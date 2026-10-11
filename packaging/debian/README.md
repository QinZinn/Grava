# Debian binary packages

`build.sh` wraps a prepared release tree containing the static musl binary,
manual page, completions, documentation and full dependency licenses:

```sh
SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) \
  packaging/debian/build.sh STAGE 0.1.0 amd64 dist
```

The supported package architectures are `amd64` and `arm64`. The binary in
`STAGE/usr/bin/grava` must already match the chosen architecture. The script
checks its ELF headers with `readelf` and rejects dynamic executables. It does
not execute it. The output is `dist/grava_0.1.0-1_amd64.deb` (or `arm64`).
Use the release build script described in [the packaging guide](../README.md)
to prepare the complete tree.

The helper uses an installed `dpkg-deb` when available. Otherwise it runs the
official `docker.io/library/debian:stable-slim` image using rootless Podman;
it does not install packages on the host. `GRAVA_CONTAINER_ENGINE` selects a
different compatible container engine, and `GRAVA_DEB_IMAGE` selects another
Debian image. `SOURCE_DATE_EPOCH` controls archive timestamps and defaults to
zero when unset.

All installed files are owned by `root:root`. The static binary has no glibc
dependency. `pipewire-pulse | pulseaudio` is a recommendation for desktop
capture; demo and raw PCM inputs work without an audio server. The Debian
copyright file includes the complete licenses from the release tree.

Install a downloaded release package with:

```sh
sudo apt install ./grava_0.1.0-1_amd64.deb
grava --version
```

These are upstream binary packages for GitHub releases, not a source package
submission to the Debian archive.

References: [binary package fields](https://www.debian.org/doc/debian-policy/ch-controlfields.html#debian-binary-package-control-files-debian-control),
[copyright files](https://www.debian.org/doc/debian-policy/ch-docs.html#copyright-information),
and [`dpkg-deb`](https://manpages.debian.org/stable/dpkg/dpkg-deb.1.en.html).

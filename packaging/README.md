# Packaging templates

Templates only: nothing here is published. Each one builds the default,
pure-Rust configuration and installs the binary, the license, the README, the
default config as documentation, the man page (`grava --man`) and the completions
for bash, zsh and fish (`grava --completions SHELL`).

| Directory | For | Notes |
|---|---|---|
| [`aur/grava`](aur/grava/PKGBUILD) | AUR, tagged release | `updpkgsums`, then `makepkg --printsrcinfo > .SRCINFO` |
| [`aur/grava-git`](aur/grava-git/PKGBUILD) | AUR, main branch | the version comes from `Cargo.toml` and the commit count |
| [`void/srcpkgs/grava`](void/srcpkgs/grava/template) | Void Linux (`xbps-src`) | `xgensum -i grava` after tagging |
| [`fedora/grava.spec`](fedora/grava.spec) | Fedora COPR | needs a `cargo vendor` tarball; the steps are in the file |

## Releasing

1. Update `version` in `Cargo.toml`, run `cargo build` so that `Cargo.lock` follows,
   and commit.
2. Tag `vX.Y.Z` and push the tag. The `Release` workflow builds static musl binaries
   for `x86_64` and `aarch64` and attaches them, with `SHA256SUMS`, to a GitHub release.
3. In each template set the new version, reset the release number, and refresh the
   checksum (the tag archive is `Grava-X.Y.Z.tar.gz`).

The optional `pipewire` and `alsa` backends need system libraries at build time;
each template says which packages to add to enable them.

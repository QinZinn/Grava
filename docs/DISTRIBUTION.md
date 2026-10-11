# Distribution channels

Keep the published `v0.1.0` tag unchanged. Distribution definitions and workflows
can be updated on `main` while continuing to package that immutable release.
All packages use the default pure Rust `pulse` backend unless stated otherwise.

| Channel | Entry point | Publication status |
|---|---|---|
| AUR | `packaging/aur/grava/`, `packaging/aur/grava-git/` | Deferred until the maintainer can register an AUR account |
| Nix | `flake.nix`, `flake.lock`, `packaging/nix/` | Available directly through the GitHub flake; not in Nixpkgs |
| Snap | `snap/snapcraft.yaml`, manual `Snap` workflow | Public on `latest/edge`: amd64 revision 1, arm64 revision 2 |
| APT | `packaging/apt/`, manual `APT repository` workflow | Live at `https://qinzinn.github.io/Grava/apt` for amd64 and arm64 |
| DNF | `packaging/fedora/build-srpm.sh`, COPR `zinnn/grava` | Live for Fedora 43/44 on x86_64 and aarch64 |
| Void | source template and `packaging/void/build-binary.sh` | Build a local package or submit upstream when eligible |

## AUR

Publication is deferred: the maintainer cannot currently register an AUR
account. The prepared definitions remain available for local builds. Resume the
submission steps below when registration is available.

Create an account at <https://aur.archlinux.org/>, add your SSH public key in
account settings, and ensure SSH uses its matching private key for
`aur@aur.archlinux.org`. Verify access with `ssh -T aur@aur.archlinux.org`.
Keep the private key local; it is not a repository file.

The release PKGBUILD uses the verified SHA256 of GitHub's `v0.1.0` source
archive. Only the VCS package intentionally uses `SKIP` for its Git source.
Check the current package names and ownership before pushing. On Arch, verify
each definition and regenerate metadata after any change:

```sh
cd packaging/aur/grava
makepkg --cleanbuild --syncdeps
makepkg --printsrcinfo > .SRCINFO
```

From the grava repository root, submit the stable package:

```sh
git clone ssh://aur@aur.archlinux.org/grava.git target/aur-grava
cp packaging/aur/grava/PKGBUILD packaging/aur/grava/.SRCINFO target/aur-grava/
git -C target/aur-grava add PKGBUILD .SRCINFO
git -C target/aur-grava commit -m 'Initial import of grava 0.1.0'
git -C target/aur-grava push origin HEAD:master
```

Repeat with `grava-git` and `packaging/aur/grava-git/` for the development
package. Users can then install through their preferred AUR helper or build
with `makepkg`. This does not submit a package to Arch's official repositories.

References: [AUR submission guidelines](https://wiki.archlinux.org/title/AUR_submission_guidelines),
[`.SRCINFO`](https://wiki.archlinux.org/title/.SRCINFO).

## Nix

After pushing the flake and its lockfile to `main`, no separate Nix account or
registry submission is needed for direct GitHub installation:

```sh
nix profile install github:QinZinn/Grava#grava
# Or try the CLI without installing it:
nix run github:QinZinn/Grava -- --version
```

Enable the `nix-command` and `flakes` experimental features in Nix if needed.
The flake provides `x86_64-linux` and `aarch64-linux` packages. Native builds
install the generated manual and completions; cross builds skip executing the
foreign binary. From a clone, use `nix build .#grava` and `nix flake check`.

Being installable as a GitHub flake does not make `nixpkgs#grava` available.
For that, submit a separate Nixpkgs PR with a fixed release source, vendored
Cargo hash, maintainer entry, and build evidence. The local derivation provides
the build/install logic for that submission.

Reference: [Nixpkgs Rust packaging](https://github.com/NixOS/nixpkgs/blob/master/doc/languages-frameworks/rust.section.md).

## Snap Store

The publisher is `zinnn`, and [grava](https://snapcraft.io/grava) is public on
`latest/edge`: version `0.1.0`, amd64 revision **1** and arm64 revision **2**.
Both uploads passed Store processing. The Store listing uses the Music and Audio
category, project/source/issue links and recording-interface instructions. Its
metadata is managed through the dashboard; review it when publishing new versions.
Stable has no release yet; desktop audio capture still needs manual verification
before promotion.

Install the test release with:

```sh
sudo snap install grava --edge
sudo snap connect grava:audio-record
snap run grava -i demo
```

The **Actions → Snap → Run workflow** job has built and tested amd64 and arm64
snaps. It installs each locally and checks its version and headless demo, then
uploads workflow artifacts without publishing them to the Store. Download the
artifacts from the successful workflow run, or build locally on a supported
Ubuntu/LXD setup with `snapcraft`.

Browser sign-in does not authenticate the publishing CLI. Snapcraft 9.1 removed
the former Candid browser authentication, so do not use `--experimental-login`
or `SNAPCRAFT_STORE_AUTH=candid`. The supported Ubuntu One flow prompts for your
email, password and optional second factor directly in the terminal.

On Fedora, `packaging/snap/store.sh` runs the official Canonical Snapcraft OCI
image with rootless Podman, without installing snapd on the host. Run the login
step in your own terminal:

```sh
packaging/snap/store.sh login
```

The wrapper exports credentials scoped to the `grava` snap and `edge` channel
with a one-day expiry. It stores the export outside Git at
`${XDG_STATE_HOME:-$HOME/.local/state}/grava/snap-store/store.json` (directory mode
`0700`, file mode `0600`) and reuses it for subsequent commands. Enter account
credentials only in the terminal prompt; do not paste them into chat, commit the
export, or print its contents. This route uses the same `snapcraft export-login`
and `SNAPCRAFT_STORE_CREDENTIALS` mechanism as a native Snapcraft installation.
Run `login` again when the export expires. A failed login preserves the
previous credential file.

For future builds, upload the tested artifacts to `edge`:

```sh
packaging/snap/store.sh upload dist/snap/grava-snap-amd64/grava_0.1.0_amd64.snap
packaging/snap/store.sh upload dist/snap/grava-snap-arm64/grava_0.1.0_arm64.snap
packaging/snap/store.sh status
```

The Store reviews each upload before it becomes available. Native Snapcraft
users can instead run `snapcraft upload FILE.snap --release=edge` followed by
`snapcraft status grava` after authenticating with their own CLI.

Before the first stable publication, install a build on a real desktop and
connect its recording interface:

```sh
sudo snap install --dangerous ./grava_0.1.0_amd64.snap
sudo snap connect grava:audio-record
snap run grava --list-sources
snap run grava
```

Test capture through both PipeWire's PulseAudio server and PulseAudio where
available. The strict sandbox gives the snap its own home/config directory;
host dotfiles, theme files and arbitrary FIFO paths are not generally exposed.
`snap-launch` selects the user's real Pulse socket unless `PULSE_SERVER` was
already set. Keep strict confinement and confirm desktop capture before
promoting the package to stable.

Use the revision numbers reported by `snapcraft status` to release each tested
architecture to stable with a separately authenticated CLI that has stable
publication permission: `snapcraft release grava REVISION stable`. The wrapper's
credentials deliberately permit only `edge`.

References: [Snapcraft build action](https://github.com/snapcore/action-build),
[name registration review](https://forum.snapcraft.io/t/manual-review-of-all-new-snap-name-registrations/39440),
[Snapcraft authentication](https://snapcraft.io/docs/snapcraft-authentication/),
[Candid authentication removal](https://forum.snapcraft.io/t/candid-authentication-removal-in-snapcraft-9-1/53268),
[official OCI images and Podman](https://github.com/canonical/snapcraft/blob/main/docker/README.md),
[`audio-record` interface](https://snapcraft.io/docs/reference/interfaces/audio-record-interface/).

## Signed APT repository on GitHub Pages

The repository is live at <https://qinzinn.github.io/Grava/apt> with signed
metadata and the `v0.1.0` packages for amd64 and arm64. It publishes upstream
static DEBs for Debian/Ubuntu; it does not enter Debian's official archive.

The dedicated signing key's fingerprint is:

```text
BCE6 C88C 706F C13B D33D 0A50 7761 EDCE 7AFF 20A5
```

The [public signing key](../packaging/apt/grava-archive-key.asc) is committed
for independent verification. After downloading the repository key, check it
with `gpg --show-keys --with-fingerprint /etc/apt/keyrings/grava.gpg` and compare
the full fingerprint above.

Signing and GitHub Pages are already configured. The private key and its
private export are stored outside Git in `~/.local/share/grava-apt-gnupg/`;
keep a secure backup of that directory. GitHub Actions holds an encrypted
`APT_GPG_PRIVATE_KEY` secret and an `APT_GPG_FINGERPRINT` variable. Do not
commit the private export or paste it into chat. Renew or rotate the key before
its two-year expiry, updating the public key, fingerprint and Actions settings.

For future releases, run **Actions → APT repository → Run workflow** with the
published tag. It downloads both released DEBs, verifies their release
checksums, generates package indexes and signed Release metadata, and deploys
the repository to Pages. A build/signing failure prevents deployment.

Users install with:

```sh
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://qinzinn.github.io/Grava/apt/grava-archive-keyring.gpg \
  | sudo tee /etc/apt/keyrings/grava.gpg >/dev/null
echo 'deb [signed-by=/etc/apt/keyrings/grava.gpg] https://qinzinn.github.io/Grava/apt stable main' \
  | sudo tee /etc/apt/sources.list.d/grava.list
sudo apt update
sudo apt install grava
```

Each deployment replaces the repository with packages from the selected release.

References: [Debian third-party repositories](https://wiki.debian.org/DebianRepository/UseThirdParty),
[`apt-ftparchive`](https://manpages.debian.org/bookworm/apt-utils/apt-ftparchive.1.en.html).

## DNF through Fedora COPR

The [zinnn/grava COPR project](https://copr.fedorainfracloud.org/coprs/zinnn/grava/)
has been created with Fedora 43 and 44 targets for x86_64 and aarch64. The
[initial build 11107659](https://copr.fedorainfracloud.org/coprs/zinnn/grava/build/11107659/)
has succeeded for all four targets, including their Cargo tests. The signed
packages are available from the project repository.
Installation from the public repository has also been checked on Fedora 43 and
44 x86_64: package origin, RPM signatures, the installed example configuration,
and repeated deterministic headless demos. Fedora container images default to
`tsflags=nodocs`; clear that option when testing the packaged documentation.

The [0.1.0 source RPM](https://github.com/QinZinn/Grava/releases/download/v0.1.0/grava-0.1.0-1.fc44.src.rpm)
and its [SHA256 file](https://github.com/QinZinn/Grava/releases/download/v0.1.0/grava-0.1.0-1.fc44.src.rpm.sha256)
are published as GitHub `v0.1.0` release assets. Adding these assets leaves the
published tag unchanged. The source package contains the verified release
archive and vendored crates so the binary build needs no network. This is
separate from the prebuilt static RPM on GitHub Releases.

For future submissions, sign in to the COPR dashboard and submit a source RPM,
or configure `copr-cli` with credentials from
<https://copr.fedorainfracloud.org/api/> in `~/.config/copr`. Keep that credentials
file outside Git. Rebuild the source package locally with:

```sh
sudo dnf install copr-cli rpm-build cargo-rpm-macros gcc
packaging/fedora/build-srpm.sh
copr-cli list-chroots
```

The project already exists. Check its enabled targets against the available
chroots, then submit the actual source RPM filename printed by the helper:

```sh
copr-cli build grava dist/grava-0.1.0-1.fc44.src.rpm
```

The SRPM filename's Fedora suffix follows the machine preparing it; a `.fc44`
source RPM can still be rebuilt for the project's other enabled Fedora targets.

Users enable the repository and install grava with:

```sh
sudo dnf copr enable zinnn/grava
sudo dnf install grava
```

Reference: [COPR quick start](https://docs.copr.fedorainfracloud.org/user_documentation.html#quick-start).

## Void

The source template in `packaging/void/srcpkgs/grava/template` has the real
release checksum and maintainer. Copy it into a `void-packages` checkout and
build with the normal `xbps-src` workflow, checking glibc and musl variants.
The example config is documentation, not an automatically installed user config.

For an upstream binary package, run the helper in a Void environment against
the static release stage created by `packaging/build.sh`:

```sh
packaging/void/build-binary.sh STAGE 0.1.0 x86_64 dist/void
packaging/void/build-binary.sh STAGE 0.1.0 x86_64-musl dist/void
XBPS_ARCH=x86_64 xbps-rindex -a "$PWD"/dist/void/*.x86_64.xbps
XBPS_ARCH=x86_64-musl xbps-rindex -a "$PWD"/dist/void/*.x86_64-musl.xbps
sudo xbps-install --repository "$PWD/dist/void" grava
```

ARM64 uses `aarch64` / `aarch64-musl` and its matching static stage. A remote
custom XBPS repository needs its own RSA signing key and signed packages/index;
the helper produces local unsigned packages, not a public repository.

Void currently expects established release history and generally asks that
someone other than the software's author submit a new package. This first
release may therefore be ineligible for an upstream PR. Keep the template and
local packages ready, and seek a community maintainer once the project meets
that policy rather than presenting the package as accepted upstream.

References: [Void package contribution policy](https://github.com/void-linux/void-packages/blob/master/CONTRIBUTING.md),
[custom XBPS repositories](https://docs.voidlinux.org/xbps/repositories/custom.html).

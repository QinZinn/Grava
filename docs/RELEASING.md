# Releasing grava

The `Release` GitHub Actions workflow builds static default-feature binaries on
native x86_64 and ARM64 runners. Each runner produces an RPM, a DEB and a
portable tarball. A pushed `v*` tag creates a GitHub release with all six assets
and `SHA256SUMS`. A manual workflow run builds downloadable Actions artifacts
without publishing a release.

## Release 0.1.0

The manifest and lockfile already contain version `0.1.0`. The local `v0.1.0`
tag must point to the commit containing the packaging workflow. Before pushing,
check the current state:

```sh
git status --short
git log -1 --oneline
git show --no-patch --oneline v0.1.0
git ls-remote --tags origin v0.1.0
```

The working tree should be clean, and the local tag should identify the release
commit. The last command should be empty for a first publication. If the tag
has already been published, inspect that release rather than replacing it.

1. Push the reviewed commit to the recreated repository:

   ```sh
   git push origin main
   ```

2. Optionally test the workflow before publishing: open **Actions → Release →
   Run workflow**, choose `main`, and wait for both build jobs. Download the
   build artifacts and check the package matching your machine.

3. Publish by pushing the local tag:

   ```sh
   git push origin v0.1.0
   ```

4. Open **Actions → Release** and wait for both build jobs and **Publish the
   release**. The workflow verifies that the tag matches `Cargo.toml`, then
   creates [the release](https://github.com/QinZinn/Grava/releases/tag/v0.1.0)
   with generated notes and these assets:

   ```text
   grava-0.1.0-1.x86_64.rpm
   grava-0.1.0-1.aarch64.rpm
   grava_0.1.0-1_amd64.deb
   grava_0.1.0-1_arm64.deb
   grava-v0.1.0-x86_64-unknown-linux-musl.tar.gz
   grava-v0.1.0-aarch64-unknown-linux-musl.tar.gz
   SHA256SUMS
   ```

5. Edit the release description to describe the first release: Chladni sand
   figures, default PulseAudio/PipeWire capture, deterministic demo/PCM input,
   themes, configuration and shell completions. Download a package and its
   checksum to verify the published download.

Do not create the release manually before pushing the tag: the workflow creates
it. If a build fails, fix the reported problem before publishing. If only the
publish job fails and no release was created, rerun that failed job from Actions.

## Future versions

Update `Cargo.toml`, refresh the package version in `Cargo.lock`, update package
examples/templates, and run the checks in the project's working agreement plus
a local package build from [the packaging guide](../packaging/README.md).
Commit, create the matching new tag (`git tag vX.Y.Z`), push `main`, then push
the tag. Do not reuse an already published version.

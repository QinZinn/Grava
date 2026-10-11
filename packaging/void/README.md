# Signed Void repository draft

This work is saved on `codex/void-signed-repository` before an OS reinstall.
It has not been deployed, and there is no public Void repository to install
from yet. The APT workflow on `main` remains unchanged.

`build-repository.sh` wraps the checksum-verified static GitHub release
archives for x86_64 and aarch64 into four XBPS packages: each machine gets a
glibc and a musl package label. It indexes and signs each architecture, signs
the packages, and exports the public PEM. It does not execute foreign binaries.
The draft workflow builds APT and Void together under sibling Pages paths so
one repository cannot replace the other.

The dedicated RSA private key is outside Git at
`~/.local/share/grava-void-signing/private.pem` (mode `0600`). Back it up before
reinstalling. Only `grava-repository-key.pem`, its public half, belongs in Git.
No `VOID_RSA_PRIVATE_KEY` Actions secret has been configured yet.

## Resume

1. Restore the private signing key and check that its public half matches
   `grava-repository-key.pem`.
2. In a Void environment with Bash, binutils and OpenSSL, download both static
   archives and `SHA256SUMS` from the immutable `v0.1.0` GitHub release, then run:

   ```sh
   packaging/void/build-repository.sh RELEASE_DIRECTORY 0.1.0 OUTPUT_DIRECTORY \
     ~/.local/share/grava-void-signing/private.pem
   ```

3. Check all four indexes and detached `.xbps.sig2` signatures. Test install,
   version, repeated deterministic headless output and removal on native
   x86_64 glibc and musl environments; validate ARM64 on native runners.
4. Configure the GitHub Actions secret without printing the key:

   ```sh
   gh secret set VOID_RSA_PRIVATE_KEY --repo QinZinn/Grava \
     < ~/.local/share/grava-void-signing/private.pem
   ```

5. After the local checks pass, run the draft `APT and Void repositories`
   workflow for this branch and tag `v0.1.0`. Verify installation from the
   public HTTPS repository and verify that the existing APT repository still
   installs correctly. Merge the workflow and update the main distribution
   guide only after that validation.

Shell syntax and an independent workflow/signing review passed before saving
this draft. The full repository build and public installation checks were
interrupted and remain pending. Existing local unsigned x86_64/glibc and musl
packages were tested in earlier distribution work.

References: [Void signing repositories](https://docs.voidlinux.org/xbps/repositories/signing.html),
[`xbps-rindex`](https://man.voidlinux.org/xbps-rindex.1).

# Releasing Irori

Two workflows ship Irori:

- **`release.yml`** builds every package and tests that it installs and starts. A
  pull request that touches packaging runs it without publishing. Pushing a `vX.Y.Z`
  tag runs it and leaves a **draft** GitHub release with every package, a
  `SHA256SUMS` file and build attestations.
- **`publish.yml`** runs when you publish that draft. It sends the release to each
  package channel that is switched on.

## Cutting a release

1. Set `version` in the root `Cargo.toml` and the `<release>` in
   `packaging/linux/app.irori.Irori.metainfo.xml`. Merge that change.
2. Tag `main` and push the tag: `git tag v0.2.0 && git push origin v0.2.0`. The
   workflow fails if the tag and the version differ.
3. Download the draft's packages and run the checks in
   [PLATFORMS.md](PLATFORMS.md#verification-and-remaining-machine-tests) on real machines.
4. Edit the generated notes and publish the draft. `publish.yml` takes over.

To publish a release again (after adding a channel, say), run **Publish** by hand
with its tag.

## What each release contains

| File | Built on | Signed when |
| --- | --- | --- |
| `Irori-X.Y.Z-linux.tar.gz` | Ubuntu 22.04 | — |
| `irori_X.Y.Z-1_amd64.deb`, `irori-sign-in_X.Y.Z-1_amd64.deb` | Ubuntu 22.04 | The apt repository is signed |
| `irori-X.Y.Z-1.x86_64.rpm`, `irori-sign-in-X.Y.Z-1.x86_64.rpm` | Ubuntu 22.04 | Signed when published to the dnf repository |
| `Irori-X.Y.Z-linux.flatpak` | GNOME 50 SDK | — |
| `Irori-X.Y.Z-macos-arm64.dmg` / `-x86_64.dmg` (and `.zip`) | macOS 15 | `MACOS_*` secrets; notarized with the `APPLE_*` secrets too |
| `Irori-X.Y.Z-windows-setup.exe`, `Irori-X.Y.Z-windows.zip` | Windows Server 2022 | SignPath is configured |

Without signing secrets the workflow still builds every package, unsigned.

## One-time setup

Secrets go under *Settings → Secrets and variables → Actions → Secrets*, variables
under *Variables*. Never put their values in the repository.

### macOS signing and notarization

Needs an Apple Developer Program membership.

| Secret | Value |
| --- | --- |
| `MACOS_CERTIFICATE_P12` | Base64 of a *Developer ID Application* certificate and key exported as `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | That export's password |
| `MACOS_SIGN_IDENTITY` | The certificate's name: `Developer ID Application: Name (TEAMID)` |
| `APPLE_API_KEY_P8` | Base64 of an App Store Connect API key (`AuthKey_….p8`) with the Developer role |
| `APPLE_API_KEY_ID` | That key's ID |
| `APPLE_API_ISSUER_ID` | The issuer ID shown above the keys list |

### Windows signing

[SignPath Foundation](https://signpath.org) signs open-source projects for free.
Apply with the repository, then create a project with the slug `irori`, a signing
policy `release-signing`, and two artifact configurations:
- `executables`: signs `irori.exe` and `irori-sign-in.exe` in a zip;
- `installer`: signs the `*-setup.exe` in a zip.

| Kind | Name | Value |
| --- | --- | --- |
| Secret | `SIGNPATH_API_TOKEN` | A SignPath CI user's API token |
| Variable | `SIGNPATH_ORGANIZATION_ID` | Your SignPath organization ID |

### apt and dnf repositories

Served at `https://packages.irori.app` from this repository's GitHub Pages
(`PACKAGES_URL` in `publish.yml`).

1. Verify `irori.app` for the organization (*Organization settings → Pages →
   Add a domain*, a `TXT` record), so no other account can serve Pages on its
   subdomains.
2. DNS for `irori.app`: a `CNAME` record from `packages` to `irori-app.github.io`,
   not proxied, so GitHub can issue its certificate.
3. *Settings → Pages*: **Source** = **GitHub Actions**, **Custom domain** =
   `packages.irori.app`, then **Enforce HTTPS** once the certificate is issued.
4. *Settings → Environments → github-pages*: allow the `v*` tag pattern besides
   `main`, because `publish.yml` runs from the release's tag.
5. Make a signing key with no passphrase, used only for this. RSA, because
   older `rpm` releases (openSUSE Leap, for one) cannot verify ed25519
   signatures: `gpg --quick-gen-key "Irori packages" rsa4096 sign never`.
   Keep a backup of the private key outside GitHub.
6. Secret `PACKAGES_GPG_PRIVATE_KEY`: `gpg --armor --export-secret-keys <fingerprint>`.
7. Variable `PUBLISH_PACKAGE_REPOSITORIES` = `true`.

Users then follow the instructions on that page (`index.html`). Each release
replaces the repositories, which hold the latest version only.

### AUR (`irori-bin`, `irori-sign-in-bin`)

1. Create an AUR account and add an SSH key to it.
2. Secret `AUR_SSH_PRIVATE_KEY`: that key's private half.
3. Variable `AUR_COMMIT_EMAIL`: the address commits to the AUR use.
4. Variable `PUBLISH_AUR` = `true`.

### Homebrew and Scoop

1. Create the repositories `<owner>/homebrew-tap` and `<owner>/scoop-bucket`.
2. Give each a deploy key with write access (*Settings → Deploy keys*), and store
   the private halves as the secrets `HOMEBREW_TAP_DEPLOY_KEY` and
   `SCOOP_BUCKET_DEPLOY_KEY`. A deploy key can push to its one repository only.
3. Variables `PUBLISH_HOMEBREW` = `true` and `PUBLISH_SCOOP` = `true`.

Users install with `brew install --cask <owner>/tap/irori` and
`scoop bucket add irori https://github.com/<owner>/scoop-bucket && scoop install irori`.

### winget

winget accepts automated updates only for packages it already lists.

1. Submit the first version by hand: `wingetcreate new <installer URL>`, with the
   identifier `Irori.Irori`.
2. Fork `microsoft/winget-pkgs` to the account that owns the token below.
3. Secret `WINGET_TOKEN`: a classic token with the `public_repo` scope.
4. Variable `PUBLISH_WINGET` = `true`.

### Flathub

Flathub builds Irori itself from a repository it hosts, so `publish.yml` doesn't
publish there.

1. Submit `packaging/flatpak/app.irori.Irori.yml` following
   [Flathub's submission guide](https://docs.flathub.org/docs/for-app-authors/submission).
   Replace its `dir` source with a `git` source pinned to the release tag, and add
   the generated `cargo-sources.json` (`release.yml` shows the command).
2. Verify the app id: Flathub gives `app.irori.Irori` a token to publish at
   `https://irori.app/.well-known/org.flathub.VerifiedApps.txt`. Add it to
   `website/public/.well-known/` and deploy the site.
3. For each release, update the tag and `cargo-sources.json` in the Flathub
   repository; Flathub's bot builds and publishes it.

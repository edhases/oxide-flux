# Release process

Tags matching `v*` trigger [`.github/workflows/release.yml`](../.github/workflows/release.yml).

## What a release contains

| File | Platform |
| --- | --- |
| `oxide-flux-<tag>-windows-x86_64.zip` | `oxide-flux.exe`, `fluxd.exe`, README, LICENSE, THIRD_PARTY |
| `oxide-flux-<tag>-linux-x86_64.tar.gz` | same binaries, README, LICENSE, THIRD_PARTY |
| `SHA256SUMS` | checksums for every archive |
| `oxide-flux-checksums.cosign.bundle` | keyless cosign signature of `SHA256SUMS` |

The GitHub-generated source archives for the tag are the GPL corresponding
source; do not delete releases.

## Verify a download

```sh
sha256sum -c SHA256SUMS
cosign verify-blob SHA256SUMS \
  --bundle oxide-flux-checksums.cosign.bundle \
  --certificate-identity-regexp "https://github.com/edhases/oxide-flux/.github/workflows/release.yml@refs/tags/.*" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

## Windows Authenticode (optional)

The workflow signs `oxide-flux.exe` and `fluxd.exe` when these repository
secrets exist:

| Secret | Meaning |
| --- | --- |
| `WINDOWS_PFX_BASE64` | base64 of a code-signing `.pfx` |
| `WINDOWS_PFX_PASSWORD` | its password |

Without them the archives are still published and the cosign signature of the
checksums remains the trust anchor. Buy a certificate (OV/EV) before calling a
Windows build production-ready; SmartScreen reputation does not transfer between
unsigned builds.

## Not automated yet

- Windows MSI/NSIS installer (planned; sign the installer the same way).
- macOS builds (no CI runner configured; iced supports it, but the app has not
  been validated there).
- SBOM attachment (`anchore/sbom-action` or `cargo cyclonedx` is the intended
  integration).
- Auto-update. Decide the channel and signature model before 1.0.

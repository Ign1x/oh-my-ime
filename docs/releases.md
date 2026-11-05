# Binary releases

English | [简体中文](zh-CN/releases.md)

The Release workflow builds Linux binaries on native x86_64 and ARM64 runners,
then installs those archives with Nix and runs input, UI and Rime tests. A pushed
version tag publishes the verified assets. A manual workflow run produces the same
assets as Actions artifacts without publishing a Release.

Binary assets are available starting with
[v0.4.0](https://github.com/Ign1x/oh-my-ime/releases/tag/v0.4.0).
Earlier v0.1.0–v0.3.0 tags do not have these assets.

## NixOS and Home Manager

Use the release flake asset as the input instead of the source repository:

```nix
inputs.oh-my-ime.url =
  "https://github.com/Ign1x/oh-my-ime/releases/latest/download/oh-my-ime-nix.tar.gz";
```

The input is still locked by `flake.lock`. Updating it is explicit:

```sh
nix flake update oh-my-ime
sudo nixos-rebuild switch --flake .
```

To pin a specific release, replace `latest/download` with `download/vX.Y.Z`.
Import `inputs.oh-my-ime.nixosModules.default` or
`inputs.oh-my-ime.homeManagerModules.default` as before. Their package defaults now
download prebuilt platform and Rime archives with fixed SHA-256 hashes. Nix patches
the loader and runtime library paths, installs manifests and wraps the programs;
it does not run Cargo. Dependencies are still supplied by your selected nixpkgs.

If your configuration overrides `programs.oh-my-ime.package` or
`programs.oh-my-ime.rime.package`, remove the source-building overrides or select
the release packages explicitly:

```nix
programs.oh-my-ime = {
  enable = true;
  package = inputs.oh-my-ime.packages.${pkgs.stdenv.hostPlatform.system}.default;
  rime.enable = true;
  rime.package = inputs.oh-my-ime.packages.${pkgs.stdenv.hostPlatform.system}.rime;
};
```

For a profile installation:

```sh
nix profile install 'https://github.com/Ign1x/oh-my-ime/releases/latest/download/oh-my-ime-nix.tar.gz#full'
```

`default` contains the platform, Echo and Sparkle; `rime` contains the Rime engine;
`full` combines both. Hosted Fcitx5/IBus packages are configured through the modules
as usual. A custom `mynixpkgs` wrapper must consume the release flake or its overlay
as well; leaving it on the source package keeps source builds.

## Assets and runtime requirements

Each Release contains:

- `oh-my-ime-VERSION-SYSTEM.tar.gz`: platform binaries, manifests and desktop files.
- `oh-my-ime-rime-VERSION-SYSTEM.tar.gz`: Rime engine and manifest.
- `oh-my-ime-nix.tar.gz`: a minimal flake with both modules and binary packages.
- `release.json` and `SHA256SUMS`: version, commit, architecture, URLs and hashes.

`SYSTEM` is `x86_64-linux` or `aarch64-linux`. The archives are built on Ubuntu 24.04
and require glibc 2.39 or newer. They use distribution runtime libraries, including
Wayland, libxkbcommon, fontconfig and GPU/window-system libraries; Rime additionally
requires librime >= 1.11 (`librime.so.1`) and Rime data. The Nix package supplies
these libraries and their search paths. The archive's `README.txt` covers manual
installation on other distributions. It is not a fully static bundle.

## Publishing

The workflow uses the repository's temporary `GITHUB_TOKEN`. Only its final publish
job has `contents: write`; no personal access token or long-lived secret is needed.
Push tags using your normal SSH credentials. A tag created by another workflow's
`GITHUB_TOKEN` does not trigger a new workflow run.

1. Update `[workspace.package].version`, the three internal workspace dependency
   versions in `Cargo.toml`, and `Cargo.lock`. Finish the changelog and commit the
   tested release source.
2. Run **Actions → Release → Run workflow** on the release commit to validate both
   architectures without publishing.
3. Tag that commit `vVERSION` and push the tag. The tag must match `Cargo.toml`.
4. The workflow builds and tests both architectures, assembles and evaluates the
   release flake, uploads all assets to a draft, then publishes the draft. Tags with
   a suffix such as `-rc.1` create prereleases and do not become the stable latest.

Existing release assets are never overwritten. If uploading leaves a draft after
a failure, inspect or remove that draft before retrying. Use a new version for a
changed published binary, since Nix users lock the asset hashes.

For local packaging checks, use native ELF binaries and a matching tag:

```sh
python3 scripts/release.py package --tag vVERSION --system x86_64-linux \
  --bin-dir target/release --out /tmp/omime-release
nix build --impure --file nix/check-release.nix \
  --argstr artifacts /tmp/omime-release --out-link result-release
python3 scripts/release.py assemble --tag vVERSION --systems x86_64-linux \
  --out /tmp/omime-release
```

The local single-architecture flake is for testing. Publishing requires both native
build jobs, matching source commits and verified hashes for all four binary archives.

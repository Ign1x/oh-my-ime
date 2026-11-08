# Linux distribution packages

English | [简体中文](zh-CN/linux-packages.md)

The packaging tools build deb, RPM and AppImage files for **x86_64 and ARM64** from
the same verified platform and Rime archives. They also generate an AUR split-package
recipe with fixed source hashes. The **Linux packages** Actions workflow packages
an existing release without modifying its assets; download its `linux-packages`
artifact. Future tagged releases include these formats through the Release workflow.

All formats require a supported compositor, currently Hyprland or sway. Installing
a package does not enable a service, take over another input method, or change your
personal configuration. Configure the session explicitly after installation:

```sh
omime setup --engine rime --take-over --dry-run
omime setup --engine rime --take-over --start
```

Use these commands in your supported desktop session. See [setup](setup.md) for
manual configuration and `omime setup --undo` before removing an installation.

## deb

```sh
sudo apt install ./oh-my-ime_0.5.0-1_amd64.deb
# Optional native Rime engine, using the same platform version:
sudo apt install ./oh-my-ime-rime_0.5.0-1_amd64.deb
```

Use `arm64` packages on ARM64. The platform targets Ubuntu 24.04 and compatible
Debian systems with glibc 2.39 or newer. The optional Rime package additionally
requires **librime ≥ 1.11** and the distribution's Xiaohe/Luna Pinyin data packages.
Ubuntu 24.04's librime 1.10 does not satisfy that requirement; use a distribution
with a newer librime, or the AppImage. `apt` resolves dependencies and refuses an
incompatible installation.

## RPM

```sh
sudo dnf install ./oh-my-ime-0.5.0-1.x86_64.rpm
sudo dnf install ./oh-my-ime-rime-0.5.0-1.x86_64.rpm
```

Use `aarch64` packages on ARM64. Fedora is the RPM target. ELF symbol requirements
and explicit dependencies cover the libraries loaded by the UI; the Rime package
requires the matching platform package, librime ≥ 1.11 and Rime data. The package
does not include third-party Fcitx 5 or IBus engines.

Both native formats install programs into `/usr/bin`, engine manifests and example
configuration into `/usr/share/oh-my-ime`, the settings desktop entry into
`/usr/share/applications`, and `omimed.service` into `/usr/lib/systemd/user`.
Platform and Rime licenses occupy separate directories, so either package can be
upgraded or removed without taking ownership of the other's files. After upgrading,
restart your existing daemon; package hooks do not interrupt typing automatically.

## AppImage

The AppImage contains the platform, Rime, its standard dictionaries and non-system
runtime libraries. It requires **glibc 2.39 or newer**, system fonts and suitable
host graphics drivers. It does not bundle a compositor or GPU drivers.

```sh
chmod +x oh-my-ime-0.5.0-x86_64.AppImage
./oh-my-ime-0.5.0-x86_64.AppImage                 # settings
./oh-my-ime-0.5.0-x86_64.AppImage --help          # CLI help
./oh-my-ime-0.5.0-x86_64.AppImage --install
~/.local/bin/omime setup --engine rime --take-over --dry-run
```

Use the `aarch64` image on ARM64. Normal launches use a persistent cache so engine
and settings processes can outlive the AppImage mount. `--install` places a complete
copy under `$XDG_DATA_HOME/oh-my-ime/appimage` and creates launchers in
`~/.local/bin`. Add that directory to `PATH`. It refuses to overwrite unrelated
launchers. Run setup through the installed launcher so autostart uses a durable
path. Installation alone does not configure or start the input method.

Without FUSE, use `./IMAGE.AppImage --appimage-extract-and-run --install`.
The accompanying `.AppImage.build.json` records bundled library versions and source
provenance. See [AppImage packaging](../packaging/appimage/README.md) for cache,
upgrade and removal details.

## Arch Linux / AUR

The generated recipe builds `oh-my-ime-bin` and `oh-my-ime-rime-bin` from checksum-pinned
release archives; it supports `x86_64` and `aarch64`. Recipe generation does not submit
an AUR package. Until a maintainer publishes it, install the included recipe locally:

```sh
tar -xzf oh-my-ime-aur-0.5.0.tar.gz
cd oh-my-ime-bin
makepkg --verifysource
makepkg -si
```

The Rime package uses Arch's librime and the `rime-prelude`, `rime-essay`,
`rime-luna-pinyin` and `rime-double-pinyin` data packages. `.SRCINFO` is included for
maintainer submission; no credentials are embedded. Details are in the
[AUR packaging guide](../packaging/aur/README.md).

## Build and verify

Run the native build on each architecture with Docker:

```sh
python3 scripts/fetch-release.py --tag v0.5.0 --system x86_64-linux --out inputs
bash scripts/build-linux-packages.sh inputs/release.json inputs dist x86_64-linux
```

The driver builds in disposable Ubuntu/Fedora containers and checks real native
package installation. AppImage bundles a checksum-pinned librime source build on
Ubuntu 24.04 to preserve its glibc baseline. For one format, invoke
`scripts/package-linux.py --help` inside the appropriate build container.

After collecting both architectures' output into one directory:

```sh
python3 scripts/package-linux.py --format assemble --manifest inputs/release.json --out dist
(cd dist && sha256sum --check SHA256SUMS-linux)
```

The assembler rejects mixed source revisions, missing formats and changed payloads.
Its `linux-packages.json` and `SHA256SUMS-linux` describe the additional formats
without replacing an existing release's manifest or checksums. In a new tagged
release, the main `release.json` and `SHA256SUMS` cover all formats together.

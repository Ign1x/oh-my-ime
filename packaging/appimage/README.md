# AppImage packaging

The AppImage includes the platform, the native Rime engine, librime 1.11.2,
Xiaohe shuangpin and Luna Pinyin data, OpenCC data, and Rime's private shared
libraries with their licenses. It targets native x86-64 and ARM64 Linux with
**glibc 2.39 or newer**, the host runtimes below, a supported Wayland compositor,
system fonts, and compatible host graphics drivers. Host Fcitx/IBus engines and toolkit input-method plugins remain optional
system installations. This librime build provides the standard schemas; external
Rime plugins such as Lua are not included.

## Host runtimes

The image bundles only librime, gflags, glog, LevelDB, marisa, OpenCC, Snappy and
yaml-cpp. It uses the host's Wayland, X11/XCB, font libraries, C/C++ runtimes,
unwind, compression/XML libraries and graphics drivers. This keeps newer host
Mesa/LLVM drivers from binding to older copies of shared dependencies inside
the image. Each build records required SONAMEs and the highest GLIBC, GLIBCXX,
CXXABI and GCC symbol versions in `.AppImage.build.json`.

These direct runtime packages are sufficient on supported distributions; the
package manager supplies their transitive dependencies:

```sh
# Ubuntu 24.04+ / compatible Debian with glibc >= 2.39
sudo apt-get install libgcc-s1 libstdc++6 libunwind8 libfontconfig1 libxkbcommon0 \
  libwayland-client0 libwayland-cursor0 libwayland-egl1 libegl1 libgl1 libgles2
# Fedora 43+
sudo dnf install libgcc libstdc++ libunwind fontconfig libxkbcommon \
  libwayland-client libwayland-cursor libwayland-egl libglvnd-egl libglvnd-glx libglvnd-gles
# Arch Linux
sudo pacman -S --needed gcc-libs libunwind fontconfig libxkbcommon wayland libglvnd
```

Keep the graphics drivers supplied for your system. Mesa users need
`libgl1-mesa-dri` on Ubuntu, `mesa-dri-drivers` and `mesa-libEGL` on Fedora, or
`mesa` on Arch; proprietary GPU installations use their matching driver
packages. Install a Chinese font, such as the distribution's Noto CJK package,
for Rime's Chinese candidates. These packages are normally present in a Wayland
desktop session. Python and Weston are required only by the packaging smoke tests.

## Run and install

```sh
chmod +x oh-my-ime-0.5.0-x86_64.AppImage
./oh-my-ime-0.5.0-x86_64.AppImage                 # settings
./oh-my-ime-0.5.0-x86_64.AppImage --help          # omime CLI help
./oh-my-ime-0.5.0-x86_64.AppImage --daemon --help
./oh-my-ime-0.5.0-x86_64.AppImage --tool omime-settings --version
./oh-my-ime-0.5.0-x86_64.AppImage --package-help
./oh-my-ime-0.5.0-x86_64.AppImage --install
~/.local/bin/omime setup --dry-run
```

Ordinary launches first copy the image's files into
`$XDG_CACHE_HOME/oh-my-ime/appimage/<content-id>` (default
`~/.cache/oh-my-ime/appimage`). Executables, engines and detached settings can
therefore continue after the AppImage mount disappears. Keep this directory
while any of that image's programs are running; it can be removed after they all
exit. Nothing starts or takes over an input method unless explicitly requested.

`--install` creates a complete version under
`$XDG_DATA_HOME/oh-my-ime/appimage/<content-id>` (default `~/.local/share`),
atomically updates `current`, and creates the eight program launchers in
`~/.local/bin`. Add that directory to `PATH`. Unrelated existing launchers cause
installation to stop. The installation does not edit desktop configuration,
create an autostart entry, or start a daemon. Run the installed `omime setup`
when ready; setup through an uninstalled image is rejected. Its autostart command
uses the stable `current/usr/bin/omimed` launcher. Keep old version directories
until their processes exit. After undoing setup and stopping the daemon, an
installation can be removed by deleting its launchers and AppImage data directory.

`--appimage-extract-and-run` is available when FUSE is unavailable. The same
persistent extraction and explicit-install rules apply. Spaces in installation
paths are supported; colons and newlines are not, because these paths are also
used in `PATH` and XDG search lists.

The pinned runtime's extract-and-run cache can collide when different users run
the same image in a shared `/tmp`. Give it a private directory in that mode:

```sh
install -d -m700 "${XDG_CACHE_HOME:-$HOME/.cache}/oh-my-ime/tmp"
TMPDIR="${XDG_CACHE_HOME:-$HOME/.cache}/oh-my-ime/tmp" ./IMAGE.AppImage --appimage-extract-and-run --install
```

Each bundled ELF has its own relative `RUNPATH`. The launcher never sets
`LD_LIBRARY_PATH`, `LD_PRELOAD`, or graphics-driver search paths, so hosted
Fcitx/IBus and their tools use the host's libraries. The launcher supplies the
bundle's data to its own process tree using XDG paths, `OMIME_RIME_SHARED_DATA`
and `XKB_CONFIG_ROOT`; explicit user overrides of the latter two are retained.

## Build

Use an Ubuntu 24.04 native runner for each architecture. Building on a newer
distribution is rejected to retain the glibc baseline. Install these build
dependencies in that runner or an isolated container:

```sh
apt-get update
apt-get install -y --no-install-recommends \
  python3 ca-certificates patchelf squashfs-tools binutils \
  build-essential cmake ninja-build libboost-dev libboost-regex-dev \
  libgoogle-glog-dev libgflags-dev libleveldb-dev libmarisa-dev libopencc-dev \
  libyaml-cpp-dev libunwind-dev libx11-dev \
  librime-data rime-data-double-pinyin rime-data-luna-pinyin libopencc-data \
  xkb-data libxkbcommon0 libfontconfig1 libwayland-client0 libwayland-cursor0 libwayland-egl1
python3 packaging/appimage/prepare-rime.py --prefix "$PWD/build/appimage-rime"
export OMIME_APPIMAGE_RIME_PREFIX="$PWD/build/appimage-rime"
```

Then invoke `scripts/package-linux.py` with the verified release manifest and
both native component archives, selecting `appimage`. Ubuntu's librime 1.10 is
too old for the engine; `prepare-rime.py` compiles the checksum-pinned 1.11.2
source in a private prefix, verifies its version, and records its provenance.
It does not install into the system. `--source FILE` accepts an offline source
archive but still verifies the checked-in SHA-256.

The builder copies only its explicit allowlist of private Rime libraries from
this Ubuntu host, records other dependencies as host requirements, and includes
each bundled package's copyright notice, common license texts, package and
source versions in `build.json`. Ubuntu source packages can be obtained using
`apt-get source SOURCE_PACKAGE=SOURCE_VERSION` with the corresponding Ubuntu
source repositories enabled. The pinned librime source URL/checksum is also in
that record. Every shipped ELF is checked against the Ubuntu 24.04 ABI ceiling:
GLIBC 2.39, GLIBCXX 3.4.32, CXXABI 1.3.14 and GCC 13.0.0. The actual lower
requirements are recorded per image. Rime and keyboard data are dereferenced into the image, so no links depend
on the build host.

`runtime.json` pins the AppImage type-2 runtime release **20251108** and a SHA-256
for each architecture. An `OMIME_APPIMAGE_RUNTIME` file can supply an offline
copy; the same digest and native ELF checks still apply. `mksquashfs` is supplied
by Ubuntu. No floating or unchecked executable is downloaded or run.

The output is an executable `.AppImage` and a `.AppImage.build.json` record with
its checksum, source archive digests, runtime pin, libraries, licenses, data
package versions, and a hash of the final extracted tree. The content ID includes
the tree hash so rebuilding the same release with updated libraries never reuses
an incompatible cache. The runtime's own license notices are under
`usr/share/licenses/appimage-runtime`.

Run `packaging/appimage/prepare-runtime.sh` and then
`packaging/appimage/smoke.sh OUTPUT_DIR` in a clean native Ubuntu 24.04,
Fedora 43 or Arch container. The preparation script installs the documented
host runtimes and Python 3. The smoke uses private temporary HOME/XDG paths,
checks the real image without FUSE, removes the downloaded image and cache after
installation, and deploys both Rime schemas through the bundled library. The
conversion check includes OpenCC simplified Chinese data and verifies that the
C++/GCC/unwind runtimes were loaded from the host. No desktop bus or daemon
takeover is used by this check. `prepare-runtime.sh --graphics-test` also installs
Weston, a private D-Bus daemon, Mesa and a font for `graphics-smoke.py`.

Run `python3 packaging/appimage/graphics-smoke.py OUTPUT_DIR LOG_DIR` after that
graphics preparation, as an existing unprivileged user with a writable log
directory. It starts isolated Weston with the GL renderer and forces Mesa
llvmpipe plus `SLINT_BACKEND=winit-femtovg`. Both the default settings and Rime's
settings must submit a Wayland buffer. Their `/proc` maps must show host EGL,
Mesa, LLVM, C++, GCC and Wayland libraries, with only Rime's private libraries
inside the image. The logs and `result.json` record this evidence. The panel's
overlay renderer is software-only in the application; the Rime settings test
exercises the engine executable's graphics path and its private C++ libraries.

# AUR split package

`PKGBUILD.in` is the source template for `oh-my-ime-bin` and
`oh-my-ime-rime-bin`. Generate a recipe from an assembled release manifest:

```sh
python3 scripts/package-linux.py --format aur --manifest dist/release.json --out dist
tar -xzf dist/oh-my-ime-aur-0.5.0.tar.gz
cd oh-my-ime-bin
makepkg --verifysource
makepkg --printsrcinfo > .SRCINFO
makepkg -si
```

The generated archive contains `PKGBUILD` and `.SRCINFO` with the release's
SHA-256 hashes for x86_64 and aarch64. `makepkg` selects the current architecture.
Install `oh-my-ime-rime-bin` alongside the matching platform package for Rime;
its dependencies include the shared defaults, dictionaries, and Xiaohe and
Luna Pinyin schemas in `/usr/share/rime-data`.

Packages install a user service and a settings launcher. Run `omime setup`
explicitly to configure a desktop session. Package installation does not enable
the service or modify personal configuration. The generated recipe can be
submitted to the AUR by a maintainer; generation does not publish it.

The CI smoke test runs as root inside a disposable Arch Linux container:

```sh
bash packaging/aur/smoke.sh dist/oh-my-ime-aur-0.5.0.tar.gz dist
```

It uses all four local release archives as makepkg's source cache, so it also
works before their URLs are published. It verifies sources and `.SRCINFO`, builds
both architectures, installs the x86_64 packages, and checks CLI execution, engine
paths, libraries, and Rime schema deployment. AArch64 binaries are inspected but
not executed. It also reinstalls both packages and removes Rime while checking
that platform files and user configuration remain intact. Set
`OMIME_AUR_SMOKE_OUT` to choose the output directory.

If the container host rejects pacman's Landlock filesystem sandbox, the smoke
test logs that specific failure and retries with `DisableSandboxFilesystem` in
a temporary config. Package signature verification and the syscall sandbox stay
enabled; the container's original pacman config and generated packages are unchanged.

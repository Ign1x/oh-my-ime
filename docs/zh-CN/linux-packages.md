# Linux 发行版安装包

[English](../linux-packages.md) | 简体中文

打包工具从同一组已校验的平台与 Rime 发布归档生成 **x86_64 和 ARM64** 的 deb、RPM、
AppImage，并生成固定源文件哈希的 AUR 拆分包配方。Actions 中的 **Linux packages**
工作流可为已有版本打包，下载其 `linux-packages` 产物即可；它不会修改已有 Release 的附件。
后续标签发布通过 Release 工作流包含这些格式。

所有格式都需要受支持的合成器，目前为 Hyprland 或 sway。安装包不会启用服务、接管其他输入法，
或修改个人配置。安装后，在受支持的桌面会话中显式配置：

```sh
omime setup --engine rime --take-over --dry-run
omime setup --engine rime --take-over --start
```

手动配置见[配置指南](setup.md)。卸载前可用 `omime setup --undo` 撤销会话配置。

## deb

```sh
sudo apt install ./oh-my-ime_0.5.0-1_amd64.deb
# 可选的原生 Rime 引擎，需要相同版本的平台包：
sudo apt install ./oh-my-ime-rime_0.5.0-1_amd64.deb
```

ARM64 使用 `arm64` 包。平台以 Ubuntu 24.04 及 glibc 2.39 或更新版本的兼容 Debian 系统为基线。
可选的 Rime 包还要求 **librime ≥ 1.11** 以及发行版提供的小鹤双拼、朙月拼音数据包。
Ubuntu 24.04 的 librime 1.10 不满足要求；可使用提供新版 librime 的发行版，或选择 AppImage。
`apt` 会解析依赖并拒绝不兼容的安装。

## RPM

```sh
sudo dnf install ./oh-my-ime-0.5.0-1.x86_64.rpm
sudo dnf install ./oh-my-ime-rime-0.5.0-1.x86_64.rpm
```

ARM64 使用 `aarch64` 包。RPM 以 Fedora 为目标，ELF 符号依赖和显式依赖覆盖界面运行时加载的库。
Rime 包要求相同版本的平台、librime ≥ 1.11 和 Rime 数据。安装包不包含第三方 Fcitx 5、IBus 引擎。

两种原生格式均把程序安装到 `/usr/bin`，引擎清单和配置示例放在 `/usr/share/oh-my-ime`，
设置程序的桌面入口放在 `/usr/share/applications`，用户服务放在 `/usr/lib/systemd/user`。
平台与 Rime 的许可证使用不同目录，可以分别升级或移除，不会互相占用文件。
升级后请重启已有守护进程；安装脚本不会自动中断输入。

## AppImage

AppImage 包含平台、Rime、标准词库和非系统运行库，要求 **glibc 2.39 或更新版本**、系统字体
和合适的宿主图形驱动；不内置合成器或 GPU 驱动。

```sh
chmod +x oh-my-ime-0.5.0-x86_64.AppImage
./oh-my-ime-0.5.0-x86_64.AppImage                 # 设置界面
./oh-my-ime-0.5.0-x86_64.AppImage --help          # 命令行帮助
./oh-my-ime-0.5.0-x86_64.AppImage --install
~/.local/bin/omime setup --engine rime --take-over --dry-run
```

ARM64 使用 `aarch64` 镜像。普通启动会使用持久缓存，让引擎和设置进程在 AppImage 卸载后仍能运行。
`--install` 会把完整副本放在 `$XDG_DATA_HOME/oh-my-ime/appimage` 下，并在 `~/.local/bin`
创建启动器；请将该目录加入 `PATH`。遇到无关的已有启动器会拒绝覆盖。
通过安装后的启动器执行 setup，确保自启动指向持久路径。安装本身不会配置或启动输入法。

没有 FUSE 时，可用 `./IMAGE.AppImage --appimage-extract-and-run --install`。
随附的 `.AppImage.build.json` 记录打包库的版本和来源。缓存、升级和移除细节见
[AppImage 打包说明](../../packaging/appimage/README.md)。

## Arch Linux / AUR

生成的配方使用固定哈希的发布归档构建 `oh-my-ime-bin` 和 `oh-my-ime-rime-bin`，支持
`x86_64`、`aarch64`。生成配方不等于提交 AUR；维护者发布前，可先在本地安装随附配方：

```sh
tar -xzf oh-my-ime-aur-0.5.0.tar.gz
cd oh-my-ime-bin
makepkg --verifysource
makepkg -si
```

Rime 包使用 Arch 的 librime，以及 `rime-prelude`、`rime-essay`、`rime-luna-pinyin`、
`rime-double-pinyin` 数据包。已提供供维护者提交的 `.SRCINFO`，不含任何凭据。
详见 [AUR 打包说明](../../packaging/aur/README.md)。

## 构建与校验

在对应架构的原生机器上，使用 Docker 构建：

```sh
python3 scripts/fetch-release.py --tag v0.5.0 --system x86_64-linux --out inputs
bash scripts/build-linux-packages.sh inputs/release.json inputs dist x86_64-linux
```

脚本在临时 Ubuntu/Fedora 容器内构建并检查真实的原生包安装。AppImage 在 Ubuntu 24.04 内
构建固定源码哈希的 librime，以保持 glibc 基线。单独打包某种格式时，在对应构建容器内运行
`scripts/package-linux.py --help` 查看参数。

收集两个架构的输出到同一目录后：

```sh
python3 scripts/package-linux.py --format assemble --manifest inputs/release.json --out dist
(cd dist && sha256sum --check SHA256SUMS-linux)
```

组装器会拒绝混用源代码版本、缺失格式或内容被修改的包。
`linux-packages.json` 和 `SHA256SUMS-linux` 描述新增格式，不会替换已发布版本的清单或校验和。
新标签发布时，主 `release.json` 和 `SHA256SUMS` 会统一覆盖全部格式。

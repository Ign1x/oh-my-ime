# 开发

[English](../development.md) | 简体中文

## 环境

```sh
nix develop            # 全部内容，包括用于手动测试的 Hyprland
nix develop .#e2e      # 只装端到端测试需要的东西（CI 使用）
```

这些开发环境提供原生库、可无头运行的 sway、wtype 与 dbus-daemon。Rust 来自 rustup 和 `rust-toolchain.toml`。

## 检查

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```

## 端到端测试

`tests/e2e` 会为每个测试启动一个私有的无头 sway、一个私有会话总线以及 omimed，并通过虚拟键盘向 `omime-ti-client` 输入。`omime-ti-client` 是一个小型编辑器，会打印每一个 text-input 事件。整个过程不碰你的桌面会话。

```sh
cargo build --workspace
cargo test -p omime-e2e -- --ignored
```

测试失败时会打印 `omimed.log` 与 `sway.log` 的末尾。设置 `OMIME_E2E_KEEP=1` 可以保留每个测试的运行时目录及完整日志。

把 `OMIME_E2E_INSTALL_DIR` 指向已安装版本的 `bin` 目录时，同一批测试会针对该安装运行。此时 omimed 不在开发 shell 的 `LD_LIBRARY_PATH` 下启动，因此构建找不到的库会让测试失败。CI 用 Nix 包做这件事：

```sh
nix build
cargo build -p omime-e2e
OMIME_E2E_INSTALL_DIR=$(readlink -f result)/bin cargo test -p omime-e2e -- --ignored
```

## 手动运行 omimed

omimed 需要一个支持 `zwp_input_method_v2` 和 `zwp_virtual_keyboard_v1` 的合成器（Hyprland、sway 等），并且上面没有其他输入法在运行。

```sh
cargo build --workspace
PATH=$PWD/target/debug:$PATH OMIME_ENGINE_PATH=$PWD/engines \
  OMIME_LOG=debug omimed --engine echo
```

常用的环境变量：

| 变量 | 作用 |
|---|---|
| `OMIME_LOG` | 日志过滤，例如 `debug`、`omimed::core=trace` |
| `OMIME_WL_DEBUG` | 记录每一条 Wayland 消息（包括你输入的内容！） |
| `OMIME_ENGINE_PATH` | 额外的引擎目录，会优先搜索 |

### 嵌套 Hyprland

想在不离开当前桌面的情况下试用 Hyprland，可以嵌套启动它，并把客户端指向它的 socket：

```sh
nix develop
Hyprland &                      # 打开一个窗口；创建 $XDG_RUNTIME_DIR/wayland-N
export WAYLAND_DISPLAY=wayland-N
unset GTK_IM_MODULE QT_IM_MODULE XMODIFIERS
omimed --engine echo &
foot                            # 在其中输入
```

`grim` 可以截取嵌套会话的屏幕。由于嵌套的 Hyprland 只有在其窗口可见时才绘制，要无人值守截图请创建无头输出：`hyprctl output create headless`。

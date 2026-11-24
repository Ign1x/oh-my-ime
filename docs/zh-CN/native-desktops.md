# KDE 与 GNOME 原生输入

[English](../native-desktops.md) | 简体中文

本开发分支提供原生适配；已发布的 0.5.0 二进制尚未包含这些修改。安装包含适配器和 Rime
的新构建后，按桌面选择下面的入口。[应用验收](compatibility.md)记录实际测试范围。

## KDE Plasma Wayland

```sh
omime setup --desktop kde --engine rime --dry-run
omime setup --desktop kde --engine rime
```

重新登录，在**系统设置 → 键盘 → 虚拟键盘**中选择 **oh-my-ime**（不同 Plasma 版本的
分类名称可能略有不同）。KWin 启动 omimed，并传入特权输入连接。终端单独启动的 omimed
无法取得这条连接。

适配器通过 `zwp_input_method_v1` 处理按键和文本，通过同一连接上的
`zwp_input_panel_v1` 承载候选窗。保留 Wayland 引擎自绘 UI 与托管框架入口。
v1 协议没有应用 ID；按应用记忆依赖其他受支持的身份来源。

setup 写入用户入口 `$XDG_DATA_HOME/applications/io.github.ign1x.OhMyIme.InputMethod.desktop`，
并配置所选引擎，Plasma 的虚拟键盘选择由用户在系统设置中完成。从独立 omimed 用户服务
迁移时，先禁用旧服务，再选择 KWin 入口，避免同一会话启动两个 daemon。

## GNOME Wayland

```sh
omime setup --desktop gnome --engine rime --dry-run
omime setup --desktop gnome --engine rime
```

重新登录，在**设置 → 键盘 → 输入源**的中文分类下添加 **oh-my-ime**，再用 GNOME
输入源切换器选中它。系统需要安装并运行 IBus；平台通过 `omimed --ibus` 连接。

setup 写入 `$XDG_DATA_HOME/ibus/component/oh-my-ime.xml` 和 GNOME 自启动入口。
适配器也会动态注册组件，以兼容不扫描用户组件目录的发行版。IBus 继续负责应用和工具包
接入，其他 IBus 输入源仍可正常选择。保留发行版原有的 IBus 环境设置。

Rime 把行内预编辑和候选语义交给 IBus，GNOME Shell 绘制候选窗并处理翻页、点击。
此模式不创建引擎 Wayland 窗口，因此 Rime 的 Slint 候选外观设置不影响 Shell 候选窗。
当前支持 Rime、Echo；托管 Fcitx5/IBus 框架和依赖自绘 Wayland 弹窗的引擎需使用其他入口。

`omime settings` 和命令行使用相同的 daemon 控制接口。应用身份取决于客户端提供的
IBus FocusInId；经 GNOME 中转的上下文可能只有通用身份。GNOME 输入源选择与平台内
Rime 方案选择分别管理。

## 生命周期与撤销

KDE/GNOME 的启动和输入源选择由桌面管理，setup 因此拒绝 `--start`、`--take-over`。
`--config` 仅用于 Hyprland/sway。`omime setup --undo` 根据 journal 恢复托管文件；
移除正在使用的注册前，先切换到其他桌面输入源。切换 setup 桌面类型前先撤销旧 journal。

`omime doctor` 区分 IBus 是否连接与输入源是否选中；注册成功不等于已启用输入。
失焦会使待发送文本和候选失效。应用报告密码、PIN、隐藏或私密字段时绕过引擎；
应用未提供这些元数据时，适配器无法据此识别敏感字段。

NixOS/Home Manager 使用本分支模块的 `programs.oh-my-ime.backend = "kde"` 或
`"gnome"`。KDE 由 KWin 启动；GNOME 运行 `--ibus`。NixOS 的 GNOME 选项启用系统 IBus，
Home Manager 则依赖发行版提供 IBus。原生模式不写入 Wayland 后端的全局 Fcitx 环境。
包和引擎选项见 [Nix 指南](nix.md)。

## 复现验收

先运行 `nix develop .#e2e --command cargo build --workspace --locked`，再按需执行：

```sh
nix develop .#e2e --command python3 tests/desktop/native-kwin.py \
  --kwin /path/to/kwin_wayland --wire-log --out /tmp/omime-kwin-native
nix develop .#e2e --command python3 tests/desktop/native-ibus.py \
  --monitor --out /tmp/omime-ibus-native
nix develop .#e2e --command python3 tests/desktop/native-gnome.py \
  --shell /path/to/gnome-shell --out /tmp/omime-gnome-native
nix develop .#e2e --command python3 tests/desktop/native-gnome.py \
  --shell /path/to/gnome-shell --gtk3 --gtk4 --qt6 --out /tmp/omime-gnome-toolkits-native
```

每轮创建独立 XDG 目录、总线和进程，不改变现有桌面。KWin 使用虚拟显示器，GNOME
需要支持 headless 的 Shell。探针强制使用虚拟软件渲染，避免宿主 GPU 状态导致清理不确定。
KWin 6.8.3 虚拟渲染器在截图时发生崩溃，因此该轮不能证明候选窗视觉位置正确。可选的
`--gtk3`、`--gtk4` 和 `--qt6` 会在隔离 Shell 会话中启动真实 GTK3/GTK4 `Gtk.Entry` 及 Qt6
Quick `TextField`，分别检查一次 Rime 上屏；运行环境需要 GTK 3/4、Qt6 Declarative 和 PyGObject。
物理显示器、混合 DPI、全屏，以及浏览器、Electron 和办公软件仍需分别验收。

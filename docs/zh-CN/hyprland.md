# 在 Hyprland 上使用 oh-my-ime

[English](../hyprland.md) | 简体中文

对于普通可写的 Hyprland 配置文件，按下文安装平台后，可用 [`omime setup`](setup.md) 配置应用输入环境与自启动：

```sh
omime setup --engine sparkle --take-over --dry-run
omime setup --engine sparkle --take-over --start
```

安装 Fcitx5 拼音插件后可改用 `--engine fcitx5/pinyin`；安装 IBus 与 libpinyin 后可用 `--engine ibus/libpinyin`。配置后注销并重新登录，再运行 `omime doctor`。下文的手动配置也适用于 Nix/Home Manager 声明式文件和 systemd 管理的会话。

## 1. 安装

### 使用 Nix

在 NixOS 或任何带 Nix 的其他发行版上：

```sh
nix profile install github:Ign1x/oh-my-ime
```

在 NixOS 上，也可以改为把它加入系统配置。在 flake 中写上 `inputs.oh-my-ime.url = "github:Ign1x/oh-my-ime"`：

```nix
{ inputs, pkgs, ... }:
let
  oh-my-ime = inputs.oh-my-ime.packages.${pkgs.stdenv.hostPlatform.system}.default;
in
{
  environment.systemPackages = [ oh-my-ime ];
  # systemd 用户单元；它随经由 systemd 启动的会话（例如 UWSM）启动 omimed。
  # 否则请在 Hyprland 中启动 omimed，见第 3 步。
  systemd.packages = [ oh-my-ime ];
  systemd.user.services.omimed.wantedBy = [ "graphical-session.target" ];
}
```

### 从源码安装

```sh
git clone https://github.com/Ign1x/oh-my-ime
cd oh-my-ime
scripts/install.sh # 安装到 ~/.local；系统级安装请设 PREFIX=/usr/local
```

这需要 Rust 工具链，以及 wayland、libxkbcommon、dbus 和 fontconfig 的开发文件；并且 `~/.local/bin` 必须在 Hyprland 启动程序所用的 `PATH` 中。在 NixOS 上请使用 Nix 包：以这种方式构建的二进制在那里找不到图形库。

安装脚本也会把配置示例与 Hyprland 片段安装到 `~/.local/share/oh-my-ime/`，把用户单元安装到 `~/.local/share/systemd/user/omimed.service`。单元和自带引擎使用绝对路径，自定义 `PREFIX` 同样适用。打包时可设置 `OMIME_INSTALL_BIN_DIR=/预构建程序的绝对目录`，直接安装已有二进制。

## 2. 停止其他输入法

一个 Wayland seat 只能容纳一个输入法。如果 fcitx5 或 IBus 随会话启动，请把它从自启动中移除（或执行 `pkill fcitx5`、`ibus exit`）。seat 被占用时 omimed 会提示你：`another input method is active on this seat`。

## 3. 让 omimed 随 Hyprland 启动

Lua 配置（`~/.config/hypr/hyprland.lua`，Hyprland 0.55 及更高版本）：

```lua
hl.on("hyprland.start", function()
    hl.exec_cmd("omimed")
end)

-- 让各类应用找到 omimed（见下文）。
hl.env("GTK_IM_MODULE", "")
hl.env("QT_IM_MODULE", "fcitx")
hl.env("QT_IM_MODULES", "wayland;fcitx")
hl.env("XMODIFIERS", "@im=fcitx")

-- 可选：在任意位置循环切换引擎。
hl.bind("SUPER + space", hl.dsp.exec_cmd("omime next"))
```

旧版 `hyprland.conf`：

```ini
exec-once = omimed
env = GTK_IM_MODULE,
env = QT_IM_MODULE,fcitx
env = QT_IM_MODULES,wayland;fcitx
env = XMODIFIERS,@im=fcitx
bind = SUPER, space, exec, omime next
```

这些变量选择应用接入 omimed 的途径。GTK 和 Qt 6.8 及更高版本（`QT_IM_MODULES`）使用合成器的 text-input；Qt 5 以及 X11 上的 Qt 应用（例如微信、WPS）使用 Fcitx5 模块（`QT_IM_MODULE=fcitx`），由 omimed 提供服务；其他 X11 应用使用 XIM（`XMODIFIERS`）。系统中的 Qt 应用需要 fcitx5-qt 插件才能使用该模块（在 NixOS 上为 `qt6Packages.fcitx5-qt` 和 `libsForQt5.fcitx5-qt`）；自带 Qt 的应用还需确认其打包版本包含对应插件。`omime doctor` 会检查这些变量。`bind` 让你在任何地方都能切换输入法，包括那些没有在输入、omimed 自己的热键够不到的应用。

这两段配置都在 [`data/hyprland`](../../data/hyprland) 和安装后的 `share/oh-my-ime/hyprland/` 中。如果会话使用 systemd（例如通过 UWSM），可以使用已安装的用户单元自启动：

```sh
systemctl --user daemon-reload
systemctl --user import-environment WAYLAND_DISPLAY DISPLAY PATH
systemctl --user enable --now omimed
```

保留上面的应用环境设置，自启动方式选择用户单元或 Hyprland 的 `exec-once`/`hl.exec_cmd` 其中一种。自定义前缀不在 systemd 搜索路径内时，先运行 `systemctl --user link /你的前缀/share/systemd/user/omimed.service`。Nix 包也包含单元，NixOS 通过 `systemd.packages` 提供它。

## 4. 使用

| 操作 | 方式 |
|---|---|
| 在直接输入与上次使用的引擎之间切换 | `Ctrl+Space`（在文本框获得焦点时） |
| 下一个输入法 | `Ctrl+Shift+Space`，或 `omime next` |
| 指定输入法 | `omime switch fcitx5/shuangpin`、`omime switch sparkle`、`omime switch direct`，或使用托盘菜单 |
| 查看可选项 | `omime list`（`*` 标记正在使用的输入法）；`omime list --all` 还会列出可以启用的（`-`） |
| 查看已安装的内容 | `omime engines`（`*` 标记已启用的引擎） |
| 排查问题 | `omime doctor`（发现问题时退出状态为 1） |
| 启用 / 禁用引擎 | `omime enable echo`、`omime disable echo` |
| 配置引擎 | `omime settings fcitx5`，或托盘菜单里的“配置 …” |
| 安装 / 移除引擎 | `omime install ~/src/my-engine`、`omime uninstall my-engine` |
| 重新加载配置 | `omime reload`、`systemctl --user reload omimed` 或 SIGHUP |
| 查看或修改选项 | `omime config`、`omime config apps.remember true` |
| 各个应用会拿回的输入法 | `omime apps`、`omime apps forget <应用>` |
| 引擎的菜单（标点等） | `omime properties`、`omime activate <id>` |
| 持续监视状态 | `omime status --watch --json`（每次变化输出一行，供状态栏使用） |

Echo 和 Sparkle 已随平台安装，但与 Fcitx 5、IBus 一样默认仅供选择，不启用。运行 `omime enable echo` 或 `omime enable sparkle`，或在设置应用中选择“添加输入法…”，即可将演示引擎加入热键切换顺序。`omime switch sparkle` 可直接试用尚未启用的引擎；托盘菜单列出已启用的输入法，其中的“设置…”可打开设置应用。`omime setup --engine sparkle` 会启用 Sparkle，并将其设为首次登录的输入法。没有启用任何引擎时，`omime doctor` 会列出可用引擎及启用方式。

首次启动采用 `omime setup --engine ID` 设置的 `initial_input_method`，未设置时使用直接输入；之后会恢复上次选择，包括直接输入。即时切换可用 `omime switch sparkle`。热键和按键超时位于 `~/.config/oh-my-ime/config.toml`；[`data/config.toml`](../../data/config.toml) 说明了每一个选项。文件写错时，`omime reload` 会指出错在哪里，omimed 继续使用原来的配置；它不认识的选项（拼错了，或者属于更新的版本）会被略过，`omime doctor` 会把它们列出来。

### 每个应用一个引擎

默认情况下，所有应用共用一个引擎。要让每个应用使用你上次在其中用过的引擎，在 `config.toml` 中加入以下内容，然后运行 `omime reload`：

```toml
[apps]
remember = true
```

之后，在某个应用获得焦点时切换引擎，只会影响这个应用；每次回到它时，它都会换回自己的引擎，重启之后也一样。还没有切换过的应用沿用当前引擎。启动器等 layer-shell 界面不是窗口：在其中切换，算作下方窗口的选择。

要让同一 Rime 方案在聊天软件中使用中文、在终端中使用英文，可在设置的“应用”页开启
输入模式记忆，或运行：

```sh
omime config apps.remember true
omime config apps.remember_mode true
```

此功能从 v0.5.0 起可用，默认关闭。平台按应用及 Rime 方案保存模式，在处理
新按键前恢复。普通托盘图标也会显示“中”或“A”；`omime status --json` 的 `input_mode`
字段分别为稳定的 `native`、`latin`，设置页显示相同的已确认状态。关闭选项后恢复引擎
原有的共享模式行为，并保留已保存偏好；忘记某个应用会同时删除它的模式记录。
托管 Fcitx5、IBus 及未声明此可选协议能力的引擎仍自行管理模式。
恢复失败时，设置和 `omime doctor` 会显示原因，状态展示以引擎实际确认的模式为准。

### 托盘

omimed 在状态栏的托盘中显示正在使用的输入法（StatusNotifierItem）：waybar 的 `tray` 模块、KDE 的系统托盘等等。悬停提示写出它的名字，Fcitx 5 内的输入法会写成“双拼 (Fcitx 5)”这样；用 Fcitx 5 与 IBus 时，图标也来自它们。单击在直接输入与上一个引擎之间切换，中键单击切到下一个输入法，菜单列出输入法与直接输入，之后是正在使用的引擎的开关（例如中文标点）和它的设置。要隐藏它，在 `~/.config/oh-my-ime/config.toml` 中这样设置，然后运行 `omime reload`：

```toml
[tray]
enabled = false
```

### 设置程序

运行 `omime settings`，或点托盘菜单里的“设置…”、启动器里的“oh-my-ime 设置”，都能打开 oh-my-ime 的设置。在这里可以：
- 管理你的输入法及其顺序，并从 Fcitx 5、IBus 和已安装引擎提供的输入法中添加更多；
- 设置热键；
- 设置每个应用的输入法；
- 开关托盘、切换浮层和通知，调整按键超时；
- 查看 omimed 报告的会话状态。

它所做的修改都经 omimed 写入 `config.toml`，你的注释会保留。它的应用 id 是 `io.github.ign1x.OhMyIme.Settings`，可用于窗口规则；`omime-settings hotkeys`（或 `applications`、`general`、`about`）会直接打开对应的页面。

### 通知

出现问题时，omimed 会用桌面通知告诉你：引擎停止了、另一个输入法（例如桌面自动启动的 Fcitx 5）占用了键盘，或者 `config.toml` 有错误。通知由你的通知守护进程（mako、dunst、swaync 等）显示，点击通知会打开设置。要关掉通知：

```toml
[notifications]
enabled = false
```

### 切换浮层

从 v0.5.0 起可用 `omime choose` 打开交互式选择器：方向键或 Tab/Shift+Tab
浏览，Enter/空格或鼠标点击确认，Esc 取消。长列表支持滚轮、PageUp/PageDown 和
Home/End。浏览时不改变输入法；开启 `[apps] remember` 后，确认会记到打开选择器前的
应用。失去焦点或守护进程退出时自动关闭，重复运行命令不会打开多个选择器。

可绑定合成器快捷键，在未输入文字时也能打开，例如 Hyprland 的 `.conf` 配置：

```ini
bind = SUPER, Space, exec, omime choose
```

选择器使用名为 `omime-chooser`、可接收键盘焦点的图层窗口，由合成器选择显示器；
即使 `[panel] enabled = false` 也可使用。打开时会离开原输入框，尚未上屏的组合内容
按引擎原有的失焦规则处理。现有切换快捷键仍然立即生效，并使用下面的被动提示。

用热键切换，或者用 `omime switch`、`next` 和 `toggle`（比如来自你自己的按键绑定）切换时，屏幕下方会短暂显示输入法，并高亮刚切换到的那个：这就是切换浮层 `omime-panel`，由 omimed 启动。从托盘菜单切换，以及启用 `[apps] remember` 后随应用切换时，不会显示它。它是命名空间为 `omime-switcher` 的图层表面，可用于合成器规则；要关掉它：

```toml
[panel]
enabled = false
```

### Waybar

`omime status --watch --json` 先输出一次状态，之后每次变化输出一行，格式与 waybar 一致（`text`、`tooltip` 和 `class`），waybar 因此无需轮询：

```json
"custom/ime": {
    "exec": "omime status --watch --json",
    "return-type": "json",
    "on-click": "omime toggle",
    "on-click-right": "omime next"
}
```

### Fcitx 5 作为引擎

Fcitx 5 无需修改即可作为 oh-my-ime 内部的一个引擎运行，连同它的全部输入法和插件：omimed 启动它，为它提供一个私有会话总线和属于它自己的输入法，并像对待任何引擎一样，把它的候选窗口显示在文本光标处。必须安装 `fcitx5`，并且它不能单独运行（见第 2 步）。oh-my-ime 自带它的清单（Nix 包和 `scripts/install.sh` 会安装），所以只要装了 `fcitx5`，`omime engines` 就会把它列为可用。启用它，它就加入 `Ctrl+Shift+Space` 的轮换；也可以直接切换过去：

```sh
omime enable fcitx5
omime switch fcitx5
```

没有自带清单时，可以从仓库的检出目录安装：`omime install /path/to/oh-my-ime/data/engines/fcitx5`；也可以自己编写 `~/.local/share/oh-my-ime/engines/fcitx5/engine.toml`：

```toml
[engine]
id = "fcitx5"
name = "Fcitx 5"
exec = ["fcitx5"]
protocol = "input-method-v2"
```

Fcitx 5 照常把配置保存在 `~/.config/fcitx5`。要修改配置，用 `omime settings fcitx5` 打开它的配置工具，或在使用 Fcitx 5 时点托盘菜单里的“配置 Fcitx 5…”：omimed 会让配置工具连到 Fcitx 5 所在的私有总线上。Fcitx 5 为正在使用的输入法提供的开关，例如中文标点、全角和简繁转换，也在托盘菜单里，以及 `omime properties` 中（用 `omime activate <id>` 切换）。

你在 Fcitx 5 中设置的输入法就是可供选择的对象，每个都单独成项：`omime list` 会显示 `fcitx5/shuangpin`、`fcitx5/pinyin` 等等（它的键盘布局不在其中；直接输入属于 omimed），`omime switch fcitx5/shuangpin`、`Ctrl+Shift+Space` 与托盘菜单都能选中它们。omimed 会向 Fcitx 5 询问它有哪些输入法并记住，因此即使 Fcitx 5 尚未启动，它们也会被列出；Fcitx 5 再次启动时，omimed 会把正在使用的输入法还给它。

`omime enable fcitx5` 启用 Fcitx 5 分组里的全部输入法。要逐个挑选，就按 id 启用：`omime enable fcitx5/rime` 即使 Rime 不在 Fcitx 5 的分组里也能启用它（切换过去时 omimed 会把它加进分组），`omime disable fcitx5/pinyin` 则只禁用拼音，Fcitx 5 的其它输入法仍然启用。写在 `config.toml` 里就是 `engines = ["fcitx5/shuangpin", "fcitx5/rime", "sparkle"]` 这样的列表，顺序即切换顺序。

请用 omimed 的热键、`omime switch` 和托盘切换，而不要用 Fcitx 5 的触发键：omimed 会在任何引擎看到 `Ctrl+Space` 之前先处理它。由 Fcitx 5 自己处理的热键（例如它内部输入法之间的 `Ctrl+Shift`）仍然有效，omimed 也会跟随它们。Fcitx 5 自己打开的窗口（例如它的配置工具）会作为普通窗口出现在桌面上。

### IBus 作为引擎

IBus（1.5.32 或更高版本）以同样的方式在 oh-my-ime 内部运行，连同它的全部输入法（ibus-libpinyin、ibus-rime、ibus-hangul 等）。oh-my-ime 提供的 `omime-ibus` 启动 `ibus-daemon`，并把 IBus 的 GTK 面板作为 Wayland 输入法启动（`ibus-ui-gtk3 --enable-wayland-im`），因此候选窗口由 IBus 自己绘制，再由 omimed 放到文本光标处。必须安装 IBus，并且它不能单独运行。和 Fcitx 5 一样，装好之后它就会被列为可用（没有自带清单时，用 `omime install /path/to/oh-my-ime/data/engines/ibus` 安装）：

```sh
omime enable ibus
omime switch ibus
```

你在 IBus 中选定的引擎（`ibus-setup`，或 dconf 中的 `preload-engines`）就是可供选择的对象，每个都单独成项，和 Fcitx 5 的一样：`omime switch ibus/libpinyin`、`Ctrl+Shift+Space` 与托盘菜单都能选中它们。没有选定任何引擎时，omimed 会列出 IBus 正在使用的引擎，以及你用 IBus 自己的工具切换到的每一个引擎。

被托管的守护进程把它的地址写到 IBus 工具查找的位置，因此这些工具照常工作：`ibus engine libpinyin` 也能切换，`ibus-setup` 则用来配置引擎，它会启动各引擎自己的设置工具。这些工具在你的会话里运行，把设置保存到 dconf。被托管的 IBus 本身运行在 omimed 的私有会话总线上，那里无法访问 dconf 的服务：它在启动时读取 dconf 中保存的设置，但无法保存自己的更改。用 `omime settings ibus` 或托盘菜单里的“配置 IBus…”打开 `ibus-setup`，关闭它时 omimed 会重启 IBus，让 IBus 读到你做的更改。请用 omimed 的热键和 `omime switch` 切换；omimed 会在 IBus 看到 `Ctrl+Space` 之前先处理它。正在使用的 IBus 引擎的开关，例如 libpinyin 的中英文模式和全角半角，在托盘菜单和 `omime properties` 中。

## 哪些应用可用

oh-my-ime 通过合成器的 text-input 协议连接 Wayland 应用，通过 XIM 或 Fcitx5 的 GTK 与 Qt 模块连接 X11 应用：

下表说明接入途径。自动化测试覆盖协议客户端和托管的 Fcitx5/IBus，尚未完成整张真实应用矩阵的验收。应用验收需记录版本、Wayland/XWayland 模式、模块或启动参数和测试日期，覆盖组字、候选选择、快捷键、焦点、定位与重启；微信/WPS 以及 Flatpak/Snap 打包版本需要分别验证。

已知应用问题（2026-11-04 实测）：WPS Office 12.1.2.29183 在 NixOS/XWayland 下，
使用 Fcitx 模块后文档能输入中文，但首页输入框失败，并可能暂时影响随后文档中的输入。
用户在相同会话中换用独立运行的 Fcitx 5.1.23 也复现了同样的问题。
具体原因尚未确定，可能涉及 WPS、自带的 Qt/CEF 或打包方式。

| 工具包 / 应用 | 接入途径 / 条件 |
|---|---|
| GTK 3/4、较新的 Qt 6 和 Wayland 终端 | 所安装版本支持的 text-input-v3 |
| Firefox（Wayland） | GTK Wayland 后端的 text-input |
| Chromium 与 Electron（Wayland） | 部分版本需加 `--enable-wayland-ime --wayland-text-input-version=3` |
| XWayland 下的 X11 应用 | XIM，设置 `XMODIFIERS=@im=omime` |
| 使用 Fcitx5 模块的 GTK/Qt 应用 | 安装对应插件，设置 `GTK_IM_MODULE=fcitx` / `QT_IM_MODULE=fcitx` |

### X11 应用

XWayland 应用收不到 `text-input`，因此通过另外的途径连接 omimed。两个前端都由 omimed 自己提供；fcitx5 不能运行。

- **XIM。** 启动时若设置了 `DISPLAY`，omimed 就是 `$DISPLAY` 对应 X 显示上的 XIM 服务端 `omime`。以 `XMODIFIERS=@im=omime` 启动的应用会用它：xterm、urxvt、Emacs（X11）、Java（AWT/Swing）、Wine，以及设置了 `GTK_IM_MODULE=xim` 的 GTK 应用。只有 X11 应用会读取 `XMODIFIERS`，因此可以为整个会话设置它。只要该显示上没有运行 Fcitx 或 IBus，omimed 也会以它们的名字提供服务，所以以 `XMODIFIERS=@im=fcitx` 或 `@im=ibus` 启动的应用（这些输入法的配置往往会留下这样的设置）同样能找到它。
- **Fcitx5 模块。** 以 `GTK_IM_MODULE=fcitx` 或 `QT_IM_MODULE=fcitx` 启动的应用通过 Fcitx5 模块协议与 omimed 通信。需要安装对应的模块（`fcitx5-gtk`、`fcitx5-qt`）。

使用 systemd 用户单元时，`DISPLAY` 必须在用户管理器的环境中，否则 omimed 启动时不会提供 XIM。在 Hyprland 中把它加入 `hyprland.conf`（Lua 配置中把同样的命令交给 `hl.exec_cmd`）：

```ini
exec-once = dbus-update-activation-environment --systemd DISPLAY WAYLAND_DISPLAY
```

如果 omimed 已经在运行，请执行 `systemctl --user import-environment DISPLAY WAYLAND_DISPLAY`，然后重启它一次。

[第 3 步](#3-让-omimed-随-hyprland-启动)中的会话环境已经能覆盖大多数应用。使用合成器 text-input 的 Wayland 应用不需要这些变量；但有些应用做不到：比如微信，它只会通过自带的 Fcitx5 模块连接输入法。设置了 `QT_IM_MODULE=fcitx` 或 `GTK_IM_MODULE=fcitx` 的 Wayland 应用会通过 Fcitx5 模块连到 omimed；在 Hyprland 和 sway 上，omimed 会向合成器询问它的窗口位置，因此引擎窗口同样会出现在它的文本光标处（在其他合成器上则无法放置，因为 Wayland 不会告诉客户端自己的窗口在哪里）。设为 `ibus` 目前还连不到。`GTK_IM_MODULE` 最好不要为整个会话设置，只为需要它的 X11 GTK 应用设置，例如在包装脚本或 `.desktop` 文件中：

```sh
#!/bin/sh
GTK_IM_MODULE=fcitx exec "$@"
```

有些 X11 应用把预编辑的绘制交给输入法，xterm 就是其中之一。omimed 自己不绘制文字，所以在这些应用里，正在输入的内容只会显示在引擎自己的窗口中（Sparkle 有窗口；Echo 没有），直到上屏。

X11 应用的引擎窗口是命名空间为 `omime-popup` 的图层表面。Hyprland 会为图层表面播放动画。要为它们关掉动画，在 Lua 配置中写：

```lua
hl.layer_rule({
    match = { namespace = "omime-popup" },
    no_anim = true,
})
```

在旧版 `hyprland.conf` 中写：

```ini
layerrule = no_anim on, match:namespace omime-popup
```

## 故障排查

- **setup 报告不支持当前桌面。** KDE Plasma 和 GNOME 未提供必需的
  `zwp_input_method_manager_v2` 和 `zwp_virtual_keyboard_manager_v1`。
  请在 Hyprland 会话中运行 setup。工具会在写入文件或停止 Fcitx5/IBus 之前拒绝执行；
  `--dry-run` 仍会显示警告并预览。继承 `WAYLAND_SOCKET` 时也会拒绝执行，请使用未继承
  该套接字的 Hyprland 终端。两个 Wayland 变量均未设置时，可以不带 `--start`
  配置下次登录。omimed 未运行时，`omime doctor` 会指出缺少的协议。omimed 会记录
  原因并发送一次通知（除非已禁用），然后以状态码 69 退出；systemd 不会重启它。
- **接管后启动输入服务失败。** setup 的错误信息会列出已停止的框架和恢复命令
  （`fcitx5 -d`、`ibus-daemon -drx`）。setup 会先停止它启动的 omimed；请先停止其他
  omimed 实例（服务可用 `systemctl --user stop omimed` 停止）。仅应用启动环境更新失败时，
  输入服务会继续运行。重试前请查看错误中指定的日志。
- **某些功能不正常。** 先运行 `omime doctor`：它检查守护进程、seat、`config.toml`、已安装的引擎和环境，每个发现输出一行（`✓` 正常、`!` 警告、`✗` 问题）。
- **打字没有任何反应。** 检查 `omime status`。如果 omimed 没有运行，在终端里用 `OMIME_LOG=debug omimed` 启动它，看它输出什么。
- **`another input method is active on this seat`。** fcitx5、IBus 或其他输入法正在运行；见第 2 步。omimed 会持续重试，一旦 seat 空闲就接管。
- **某个应用忽略了输入法。** X11 应用需要 `XMODIFIERS=@im=omime`；或者安装 Fcitx5 模块后，使用 `GTK_IM_MODULE=fcitx`/`QT_IM_MODULE=fcitx`；后者也适用于不使用合成器 text-input 的 Wayland 应用，例如微信（见 [X11 应用](#x11-应用)）。设为 `ibus` 目前还连不到。`omime doctor` 会对环境中的输入法变量发出警告。
- **弹窗在全屏窗口上方消失，或在某些分数缩放的配置下位置略有偏移。** 这两者都是 Hyprland 的限制，影响所有输入法。

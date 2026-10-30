# 在 Hyprland 上使用 oh-my-ime

[English](../hyprland.md) | 简体中文

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

## 2. 停止其他输入法

一个 Wayland seat 只能容纳一个输入法。如果 fcitx5 或 IBus 随会话启动，请把它从自启动中移除（或执行 `pkill fcitx5`、`ibus exit`）。seat 被占用时 omimed 会提示你：`another input method is active on this seat`。

## 3. 让 omimed 随 Hyprland 启动

Lua 配置（`~/.config/hypr/hyprland.lua`，Hyprland 0.55 及更高版本）：

```lua
hl.on("hyprland.start", function()
    hl.exec_cmd("omimed")
end)

-- 让应用使用 text-input-v3，而不是 IM 模块。
hl.env("GTK_IM_MODULE", "")
hl.env("QT_IM_MODULE", "")

-- 可选：在任意位置循环切换引擎。
hl.bind("SUPER + space", hl.dsp.exec_cmd("omime next"))
```

旧版 `hyprland.conf`：

```ini
exec-once = omimed
env = GTK_IM_MODULE,
env = QT_IM_MODULE,
bind = SUPER, space, exec, omime next
```

这两段配置都在 [`data/hyprland`](../../data/hyprland) 中。如果你的会话使用 systemd（例如通过 UWSM），也可以把 [`data/omimed.service`](../../data/omimed.service) 安装到 `~/.config/systemd/user/`，然后运行 `systemctl --user enable --now omimed`。Nix 包已包含该单元。

## 4. 使用

| 操作 | 方式 |
|---|---|
| 在直接输入与上次使用的引擎之间切换 | `Ctrl+Space`（在文本框获得焦点时） |
| 下一个引擎 | `Ctrl+Shift+Space`，或 `omime next` |
| 指定引擎 | `omime switch sparkle`、`omime switch direct` |
| 查看已安装的内容 | `omime list`（`*` 标记已启用的引擎） |
| 排查问题 | `omime doctor`（发现问题时退出状态为 1） |
| 启用 / 禁用引擎 | `omime enable echo`、`omime disable echo` |
| 安装 / 移除引擎 | `omime install ~/src/my-engine`、`omime uninstall my-engine` |
| 重新加载配置 | `omime reload` |
| 持续监视状态 | `omime status --watch --json`（每次变化输出一行，供状态栏使用） |

omimed 会以你上次使用的引擎启动。热键和按键超时位于 `~/.config/oh-my-ime/config.toml`；[`data/config.toml`](../../data/config.toml) 说明了每一个选项。

### 每个应用一个引擎

默认情况下，所有应用共用一个引擎。要让每个应用使用你上次在其中用过的引擎，在 `config.toml` 中加入以下内容，然后运行 `omime reload`：

```toml
[apps]
remember = true
```

之后，在某个应用获得焦点时切换引擎，只会影响这个应用；每次回到它时，它都会换回自己的引擎，重启之后也一样。还没有切换过的应用沿用当前引擎。启动器等 layer-shell 界面不是窗口：在其中切换，算作下方窗口的选择。

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

Fcitx 5 无需修改即可作为 oh-my-ime 内部的一个引擎运行，连同它的全部输入法和插件：omimed 启动它，为它提供一个私有会话总线和属于它自己的输入法，并像对待任何引擎一样，把它的候选窗口显示在文本光标处。必须安装 `fcitx5`，并且它不能单独运行（见第 2 步）。安装仓库自带的清单（Nix 包把它放在 `share/oh-my-ime/extra-engines/fcitx5`）：

```sh
omime install /path/to/oh-my-ime/data/engines/fcitx5
omime switch fcitx5
```

也可以自己编写 `~/.local/share/oh-my-ime/engines/fcitx5/engine.toml`：

```toml
[engine]
id = "fcitx5"
name = "Fcitx 5"
exec = ["fcitx5"]
protocol = "input-method-v2"
```

Fcitx 5 照常把配置保存在 `~/.config/fcitx5`。它的配置工具通过 D-Bus 与它通信，因此要让工具连到私有总线：

```sh
DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/oh-my-ime/fcitx5/bus fcitx5-configtool
```

请用 omimed 的热键和 `omime switch` 切换，而不要用 Fcitx 5 的触发键：omimed 会在任何引擎看到 `Ctrl+Space` 之前先处理它。由 Fcitx 5 自己处理的热键（例如它内部输入法之间的 `Ctrl+Shift`）仍然有效。只显示 Fcitx 5 的弹窗，它的其他窗口无法显示。

## 哪些应用可用

oh-my-ime 通过合成器的 text-input 协议连接 Wayland 应用，通过 XIM 或 Fcitx5 的 GTK 与 Qt 模块连接 X11 应用：

| 工具包 / 应用 | 状态 |
|---|---|
| GTK 3 和 4、Qt 6、foot、kitty、alacritty、wezterm（Wayland） | ✅ text-input-v3 |
| Firefox（Wayland） | ✅ |
| Chromium 与 Electron 应用（Wayland） | ✅ 旧版本需加 `--enable-wayland-ime --wayland-text-input-version=3` |
| XWayland 下的 X11 应用，通过 XIM | ✅ `XMODIFIERS=@im=omime` |
| XWayland 下的 GTK 与 Qt X11 应用，通过 Fcitx5 模块 | ✅ `GTK_IM_MODULE=fcitx`、`QT_IM_MODULE=fcitx` |

### X11 应用

XWayland 应用收不到 `text-input`，因此通过另外的途径连接 omimed。两个前端都由 omimed 自己提供；fcitx5 不能运行。

- **XIM。** 启动时若设置了 `DISPLAY`，omimed 就是 `$DISPLAY` 对应 X 显示上的 XIM 服务端 `omime`。以 `XMODIFIERS=@im=omime` 启动的应用会用它：xterm、urxvt、Emacs（X11）、Java（AWT/Swing）、Wine，以及设置了 `GTK_IM_MODULE=xim` 的 GTK 应用。只有 X11 应用会读取 `XMODIFIERS`，因此可以为整个会话设置它。
- **Fcitx5 模块。** 以 `GTK_IM_MODULE=fcitx` 或 `QT_IM_MODULE=fcitx` 启动的应用通过 Fcitx5 模块协议与 omimed 通信。需要安装对应的模块（`fcitx5-gtk`、`fcitx5-qt`）。

使用 systemd 用户单元时，`DISPLAY` 必须在用户管理器的环境中，否则 omimed 启动时不会提供 XIM。在 Hyprland 中把它加入 `hyprland.conf`（Lua 配置中把同样的命令交给 `hl.exec_cmd`）：

```ini
exec-once = dbus-update-activation-environment --systemd DISPLAY WAYLAND_DISPLAY
```

如果 omimed 已经在运行，请执行 `systemctl --user import-environment DISPLAY WAYLAND_DISPLAY`，然后重启它一次。

不要为整个会话设置 `GTK_IM_MODULE`/`QT_IM_MODULE`。设置了 `GTK_IM_MODULE=fcitx` 的 Wayland GTK 或 Qt 应用能连到 omimed 并输入文字，但 omimed 无法为它放置引擎窗口：Wayland 不会告诉客户端自己的窗口在哪里，候选词因此始终不可见。设为 `ibus` 则什么都连不到。只在 X11 应用里设置这些变量，例如在包装脚本或 `.desktop` 文件中：

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

- **某些功能不正常。** 先运行 `omime doctor`：它检查守护进程、seat、`config.toml`、已安装的引擎和环境，每个发现输出一行（`✓` 正常、`!` 警告、`✗` 问题）。
- **打字没有任何反应。** 检查 `omime status`。如果 omimed 没有运行，在终端里用 `OMIME_LOG=debug omimed` 启动它，看它输出什么。
- **`another input method is active on this seat`。** fcitx5、IBus 或其他输入法正在运行；见第 2 步。omimed 会持续重试，一旦 seat 空闲就接管。
- **某个应用忽略了输入法。** X11 应用需要 `XMODIFIERS=@im=omime`；或者安装 Fcitx5 模块后，使用 `GTK_IM_MODULE=fcitx`/`QT_IM_MODULE=fcitx`。不要为整个会话设置模块变量：Wayland 应用虽然能经由 omimed 输入，却得不到引擎窗口（见 [X11 应用](#x11-应用)），设为 `ibus` 则什么都连不到。`omime doctor` 会对环境中的输入法变量发出警告。
- **弹窗在全屏窗口上方消失，或在某些分数缩放的配置下位置略有偏移。** 这两者都是 Hyprland 的限制，影响所有输入法。

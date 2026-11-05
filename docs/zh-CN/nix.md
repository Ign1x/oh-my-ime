# NixOS 与 Home Manager 模块

[English](../nix.md) | 简体中文

源码 flake 会编译 Rust。如需直接下载预编译程序，使用[二进制发布指南](releases.md)
中的 `oh-my-ime-nix.tar.gz` 输入；它保留相同模块接口，并按固定校验值下载平台和
Rime 包。已有源码包的 `package` 覆写也需一起切换。

flake 提供两个模块，将安装、用户服务、输入法环境和托管引擎统一声明：

- `nixosModules.default` 用于 NixOS 系统配置，并安装 Qt 输入模块。
- `homeManagerModules.default` 用于用户配置，可在 NixOS 或其他发行版上使用。

选择其中一种方式，或采用 [Hyprland 手动配置](hyprland.md)，避免同时启动多个 daemon。

## NixOS

添加 flake 输入并导入模块：

```nix
{
  inputs.oh-my-ime.url = "github:Ign1x/oh-my-ime";

  outputs = { nixpkgs, oh-my-ime, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        oh-my-ime.nixosModules.default
        ({ pkgs, ... }: {
          programs.oh-my-ime = {
            enable = true;
            rime.enable = true;
            fcitx5 = {
              enable = true;
              addons = [ pkgs.qt6Packages.fcitx5-chinese-addons ];
            };
          };
        })
      ];
    };
  };
}
```

`rime.enable` 安装原生 Rime 引擎及 nixpkgs 的 `rime-data`，默认先提供小鹤双拼。
`fcitx5.enable` 将 Fcitx 5 作为托管引擎，`addons` 指定附加输入法。
IBus 对应的选项为 `ibus.enable` 和 `ibus.engines`，例如
`[ pkgs.ibus-engines.libpinyin ]`。这几个引擎可以同时启用。

Fcitx 5 和 IBus 只放在 omimed 用户服务的 `PATH` 中，不加入系统 profile，
避免它们的桌面自启动条目抢占 Wayland 输入法 seat。引擎自己的 manifest
通过 `OMIME_ENGINE_PATH` 或系统 profile 的 `share/oh-my-ime/engines` 被发现。

`qtModules` 默认为 `true`，安装 `qt6Packages.fcitx5-qt` 和
`libsForQt5.fcitx5-qt`，供 Qt 5 和 X11 Qt 应用接入 omimed。
`sessionVariables` 默认为 `true`，统一配置：

```sh
GTK_IM_MODULE=""
QT_IM_MODULE="fcitx"
QT_IM_MODULES="wayland;fcitx"
XMODIFIERS="@im=fcitx"
```

较新的 Qt 可优先使用 Wayland text-input，其他 Qt 应用通过 Fcitx 模块，
X11 应用可通过 XIM 接入。应用无需各自修改启动器，但自带 Qt 的软件仍需
包含与其 Qt 版本匹配的输入插件。

模块与 `i18n.inputMethod.enable` 互斥：一个 Wayland seat 只能有一个输入法。
使用 oh-my-ime 时，应停用原来直接占用 seat 的输入法服务，并将其作为引擎托管。

应用系统配置后重新登录，让启动器继承会话环境，然后检查：

```sh
omime doctor
omime list --all
omime settings
```

NixOS 模块不生成 `config.toml`，可以通过设置窗口或 CLI 管理启用列表和顺序。
如果希望声明式管理该文件，可选择下面的 Home Manager 模块。

## Home Manager

导入模块并指定初始输入法：

```nix
{ inputs, pkgs, ... }:
{
  imports = [ inputs.oh-my-ime.homeManagerModules.default ];

  programs.oh-my-ime = {
    enable = true;
    rime.enable = true;
    settings = {
      engines = [ "rime/double_pinyin_flypy" "sparkle" ];
      initial_input_method = "rime/double_pinyin_flypy";
    };
  };
}
```

`settings` 的结构与 [config.toml](../../data/config.toml) 相同。`engines` 是切换顺序，
`initial_input_method` 是首次使用的输入法；之后恢复用户上次的选择，包括直接输入。
默认 `settings = { };` 不生成文件，允许设置窗口保存修改。

非空 `settings` 会把 `config.toml` 链接到 Nix store。它是声明式配置，运行时修改
不能作为持久配置保留，下一次 `home-manager switch` 会恢复 Nix 中的内容。
应修改 `settings` 后重新应用，或保持它为空，改用平台设置窗口管理。

`fcitx5`、`ibus`、`rime` 和 `sessionVariables` 的作用与 NixOS 模块相同。
Home Manager 模块不安装 Qt 输入插件；系统没有时，可在 `home.packages` 中加入
`pkgs.qt6Packages.fcitx5-qt` 与 `pkgs.libsForQt5.fcitx5-qt`。

NixOS 的 `home-manager.users.<name>.imports` 也可导入此模块，此时不要再启用
NixOS 版模块。用户服务和会话环境由 Home Manager 提供。

## 自启动与诊断

两个模块都让 omimed 跟随 `graphical-session.target` 启动。
如果合成器未启动该 target，例如从 TTY 直接运行 Hyprland 且没有 UWSM，
参照 [Hyprland 自启动](hyprland.md#3-让-omimed-随-hyprland-启动)从合成器启动 daemon。

某个软件仍不能输入时，用 `omime doctor --app PID` 检查它实际继承的环境和加载的插件。
更新会话配置不会改变已运行的进程，重启应用或重新登录后再验证。

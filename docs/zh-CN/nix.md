# NixOS 与 Home Manager 模块

[English](../nix.md) | 简体中文

flake 提供两个模块，将软件包转为声明式配置：

- `nixosModules.default` 配置系统：软件包、systemd 用户服务、输入环境、Qt 输入模块，
  以及放在服务 `PATH` 上、作为引擎使用的 Fcitx 5 或 IBus。
- `homeManagerModules.default` 配置用户：软件包、用户服务、`home.sessionVariables`，
  以及由 Nix 生成的 `~/.config/oh-my-ime/config.toml`，可用于 NixOS 或其他发行版。

源码 flake 会编译 Rust。如需预编译 Release 包，使用[二进制发布](releases.md)中介绍的
`oh-my-ime-nix.tar.gz` 输入。该 flake 提供相同模块，并按固定校验值下载平台和 Rime
压缩包。已有的源码包覆写也须切换为二进制包。

模块替代的手动步骤见 [Hyprland 指南](hyprland.md)。请选择一个模块或手动配置，
不要混用；NixOS 模块和 Home Manager 模块也不能同时启用，因为两者都提供
`omimed.service`。

## NixOS

添加 flake 并导入模块：

```nix
{
  inputs.oh-my-ime.url = "github:Ign1x/oh-my-ime";

  outputs =
    { nixpkgs, oh-my-ime, ... }:
    {
      nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          oh-my-ime.nixosModules.default
          (
            { pkgs, ... }:
            {
              programs.oh-my-ime = {
                enable = true;
                fcitx5 = {
                  enable = true;
                  # Fcitx 5 的中文输入法，包括小鹤双拼。
                  addons = [ pkgs.qt6Packages.fcitx5-chinese-addons ];
                };
              };
            }
          )
        ];
      };
    };
}
```

`fcitx5.enable` 构建 `qt6Packages.fcitx5-with-addons.override { addons = ...; }`，
并将其放到 `omimed` 服务的 `PATH` 上，供 Fcitx 5 引擎查找 `fcitx5`。IBus 使用
`ibus.enable` 和 `ibus.engines = [ pkgs.ibus-engines.libpinyin ]`，构建的是
`ibus-with-plugins`。两者可以同时启用，各自成为一个引擎。它们为 omimed 安装，
不加入系统 `PATH`：全局安装 Fcitx 5 会让其自启动条目将它作为桌面输入法启动，
取代 omimed。服务的 `PATH` 在这些框架之后保留用户 profile，因此清单引用 profile
中程序的引擎仍能找到它们。

`rime.enable` 提供 oh-my-ime 自己的 [Rime 引擎](../../engines/rime)，优先使用小鹤双拼，
并带上 librime 和 nixpkgs 的 `rime-data`；服务通过 `OMIME_ENGINE_PATH` 找到它。
其他软件包安装到 `share/oh-my-ime/engines` 的引擎通过系统 profile 查找，模块会将
该目录链接到 profile。

`qtModules` 默认为 `true`，安装 `qt6Packages.fcitx5-qt` 和 `libsForQt5.fcitx5-qt`，
供 Qt 5 和 X11 上的 Qt 应用通过 `QT_IM_MODULE=fcitx` 接入 omimed。
`sessionVariables` 默认为 `true`，设置：

```sh
GTK_IM_MODULE=""
QT_IM_MODULE="fcitx"
QT_IM_MODULES="wayland;fcitx"
XMODIFIERS="@im=fcitx"
```

之后注销并重新登录，或切换到新会话。如果会话本身不启动 `graphical-session.target`，
请启动服务，再检查结果：

```sh
systemctl --user start omimed
omime doctor
omime list --all
```

Echo 和 Sparkle 随平台安装但默认关闭，Fcitx 5 和 IBus 引擎也默认仅供选择。
运行 `omime enable echo` / `omime enable sparkle`，或在设置中选择“添加输入法…”，
即可将演示引擎加入切换顺序。

NixOS 模块不写入 `config.toml`。使用 `omime setup`、`omime enable fcitx5`
（或 `omime enable ibus`）和设置窗口选择引擎。要声明式配置，请使用 Home Manager
模块的 `settings` 选项，或自行编写配置文件。

`programs.oh-my-ime` 与 `i18n.inputMethod.enable` 冲突：一个 Wayland seat 只容纳
一个输入法。两者同时启用时，模块求值会失败。

## Home Manager

导入并启用模块：

```nix
{ inputs, pkgs, ... }:
{
  imports = [ inputs.oh-my-ime.homeManagerModules.default ];

  programs.oh-my-ime = {
    enable = true;
    fcitx5 = {
      enable = true;
      addons = [ pkgs.qt6Packages.fcitx5-chinese-addons ];
    };

    settings = {
      engines = [
        "fcitx5/shuangpin"
      ];
      initial_input_method = "fcitx5/shuangpin";
    };
  };
}
```

`settings` 是 `~/.config/oh-my-ime/config.toml` 的内容，详见[配置文件](../../data/config.toml)：
`engines` 按切换顺序列出启用的引擎和输入法，`initial_input_method` 是首次登录的选择。
`"fcitx5/shuangpin"` 是托管 Fcitx 5 内的双拼，需要 `fcitx5.enable` 和
`qt6Packages.fcitx5-chinese-addons`；在 Fcitx 5 配置中选择小鹤或其他方案。
要加入演示引擎，在 `settings.engines` 中显式添加
`"sparkle"` 或 `"echo"`，无需其他软件包。

该选项默认是 `{ }`，不写入文件，沿用 omimed 默认设置，包括关闭两个演示引擎。
设置此选项时，配置文件是指向只读 Nix store 的符号链接：`omime config`、
`omime enable` 或设置窗口的修改不能持久保留，下一次 `home-manager switch` 会恢复
生成的文件。请在 `settings` 中修改，再运行 `home-manager switch` 和 `omime reload`。

`sessionVariables` 默认为 `true`，在 `home.sessionVariables` 中设置同样四个变量。
`fcitx5.enable`、`ibus.enable` 和 `rime.enable` 与 NixOS 模块相同：框架位于服务的
`PATH` 上，不加入 `home.packages`；Rime 位于引擎搜索路径上。

在 NixOS 上，可以通过 `home-manager.users.<name>.imports` 导入模块，此时无需
NixOS 模块，但会话变量由 Home Manager 提供，只对该用户生效。`qtModules` 所提供的
Qt 输入模块也不会安装；请在 `home.packages` 中添加 `pkgs.qt6Packages.fcitx5-qt`
和 `pkgs.libsForQt5.fcitx5-qt`，供 Qt 5 和 X11 Qt 应用使用。

## 不使用 systemd 的会话

两个模块都添加随 `graphical-session.target` 启动 omimed 的用户服务。如果合成器
不启动该 target，例如从 TTY 直接运行 Hyprland 且没有 UWSM，请按
[Hyprland 指南第 3 步](hyprland.md#3-让-omimed-随-hyprland-启动)从合成器启动守护进程。
保留模块即可，它仍负责安装软件包、框架并配置环境。

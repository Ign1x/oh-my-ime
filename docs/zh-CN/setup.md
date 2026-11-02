# Hyprland 首次配置

[English](../setup.md) | 简体中文

`omime setup` 将已安装的 oh-my-ime 接入 Hyprland：配置应用输入途径和自启动，按需选择首次使用的输入法，并保存可撤销的修改记录。

先安装平台和需要的输入法。使用 Fcitx5 拼音时，需要安装 Fcitx5 及其拼音插件；Qt 应用还需要匹配的 Fcitx5 Qt 输入模块。自带的 Sparkle 表情输入法无需外部框架。依赖见[安装与应用要求](hyprland.md)。

## 配置并启动

先预览：

```sh
omime setup --engine fcitx5/pinyin --take-over --dry-run
```

应用配置，并在当前会话开始使用：

```sh
omime setup --engine fcitx5/pinyin --take-over --start
```

安装 IBus 与 libpinyin 后可用 `--engine ibus/libpinyin`；使用自带表情输入法时改为 `--engine sparkle`。省略 `--engine` 会保留已有输入法配置；省略 `--start` 则只配置下次登录，当前会话继续运行。

`--take-over` 暂停所选 Hyprland 文件中能够识别的 Fcitx5/IBus 自启动命令。与 `--start` 一起使用时，还会通过当前会话的 D-Bus 请求旧框架退出，不按进程名称批量终止程序。若其他输入法仍占用 seat，命令会明确报告输入尚未就绪。

启动时会等到框架接受输入法选择；最初的按键会等到输入上下文获得焦点再交给框架，缺少插件时会明确报错。

配置后注销并重新登录 Hyprland，让合成器和应用继承新的输入环境，再运行 `omime doctor`。已经打开的应用可能需要重启，才能使用新的 GTK/Qt/XIM 接入途径。

## 配置文件位置

存在 `$XDG_CONFIG_HOME/hypr/hyprland.lua` 时使用该文件，否则使用已有的 `hyprland.conf`；两者都不存在时，创建新版 Hyprland 使用的 Lua 配置。`XDG_CONFIG_HOME` 默认是 `~/.config`。

若 Hyprland 使用自定义配置文件，请指定同一份文件：

```sh
omime setup --config ~/.config/hypr/my-desktop.lua --engine sparkle
```

支持 Lua 和旧版 `.conf`。工具在主配置末尾加入带标记的 include，并在 `$XDG_CONFIG_HOME/oh-my-ime/` 中创建自己的配置片段，设置 `GTK_IM_MODULE`、`QT_IM_MODULE`、`QT_IM_MODULES`、`XMODIFIERS` 和守护进程自启动。其他设置、快捷键和注释保留。

自定义脚本、其他被包含文件或任意 Lua 表达式中的启动逻辑，需在原配置中调整。把输入法与其他程序合在一起的复合命令不会被自动改写。Nix/Home Manager 等工具可能将主配置生成为符号链接，应通过[手动配置](hyprland.md#3-让-omimed-随-hyprland-启动)在声明式配置中加入 include；setup 会指出符号链接，不会替换它。

## 首次选择与后续记忆

`--engine` 设置 `oh-my-ime/config.toml` 中的 `initial_input_method`，必要时启用对应引擎。守护进程只在尚无历史选择时使用这个初始值；之后每次登录都恢复用户上次的选择，包括直接输入。

`--start --engine ID` 还会在当前 daemon 中切换到该输入法。再次执行不带 `--engine` 的 `omime setup --start` 会保留当前选择。

## 撤销与恢复

```sh
omime setup --undo --dry-run
omime setup --undo
```

撤销会移除工具的 include 与配置片段，恢复它改过的选项和自启动行。注销并重新登录后，恢复原来的自启动和应用环境；当前会话暂时继续使用现有输入服务。

工具管理范围之外的修改都会保留，包括 setup 之后新增的设置。若管理范围内的内容被手动改过，撤销会先报告冲突，不改动任何文件。原内容与写入内容保存在 `$XDG_STATE_HOME/oh-my-ime/setup.toml`，默认是 `~/.local/state/oh-my-ime/setup.toml`；需要恢复时请保留这份记录。

重复执行不会重复添加自启动。写入失败会回滚已经改过的文件；进程中断后，先用 `omime setup --undo` 按记录恢复，再重新配置。

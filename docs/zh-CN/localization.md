# 本地化

[English](../localization.md) | 简体中文

平台设置、托盘、通知、切换浮层和命令行的帮助、诊断、配置流程支持英文与简体中文。
Rime 的设置和菜单、Sparkle 的操作提示使用同一份翻译目录。其他引擎和托管框架
提供各自的翻译。

## 选择语言

将桌面语言设为简体中文，重新登录后生效。程序依次读取第一个非空的 `LC_ALL`、
`LC_MESSAGES`、`LANG`。可直接预览中文帮助：

```sh
LC_ALL=zh_CN.UTF-8 omime help
LC_ALL=zh_CN.UTF-8 omimed --help
LC_ALL=zh_CN.UTF-8 omime-settings --help
```

支持 `zh_CN.UTF-8` 等 POSIX 区域设置和 `zh-Hans` 等语言标签。
`LANGUAGE=fr:zh_CN:en` 按顺序查找可用语言，目前会选择中文；遇到英文偏好时
停止回退。`C`、`C.UTF-8` 和 `POSIX` 始终显示英文，即使设置了 `LANGUAGE`。
没有译文时显示英文原文。目前还没有独立的繁体中文目录。

程序启动时选择语言。更改环境后需重新打开设置；托盘和引擎名称由 omimed 及引擎
提供，它们的服务环境也需要相同的语言设置。重新登录桌面可统一生效；只在终端中
设置语言，只影响从该终端启动的程序。界面语言与当前输入法分别选择。

JSON 字段名和状态标识（`direct`、`stopped`、引擎 ID）、D-Bus 接口、配置键和
setup 标记保持原有拼写。提示文字等供人阅读的 JSON 值可以翻译。操作系统、库和
引擎提供的错误详情可能仍使用其自身的语言。

## 添加翻译

共享目录位于 `crates/omime-proto/po/zh_CN.po`，实现位于
`crates/omime-proto/src/i18n.rs`。翻译内置于二进制，无需另外安装 gettext 目录。

复制 PO 文件，修改 `Language` 头并翻译各条 `msgstr`，再在 `CATALOGS` 中注册
语言标签和文件。`ja` 等基础标签可覆盖各地区变体，也可注册更具体的地区标签。
当前消息使用单数条目；读取器跳过 fuzzy、废弃、带上下文和复数条目。

Rust 中使用完整句子和具名变量：

```rust
omime_proto::tr!("Configure {engine}…", engine = engine_name)
```

静态标签使用 `i18n::text("…")`；常量、匹配分支等延后翻译的消息用
`i18n::message("…")` 标记。Slint 使用 `Tr.text`、`Tr.text1` 和 `Tr.text2`。
后两者的 `{}` 参数可在译文中写成 `{0}`、`{1}` 调整顺序。保留所有占位符，
按完整句子翻译。变量只插入一次，路径和引擎名称中的大括号不会被再次解释。

提交前运行：

```sh
python3 scripts/check-translations.py
cargo test -p omime-proto -p omime-cli -p omime-settings
```

检查器提取 Rust 和 Slint 中标记的消息，检查覆盖、重复项、fuzzy 和占位符。
桌面启动器的译文位于 `data/io.github.ign1x.OhMyIme.Settings.desktop`，引擎清单
使用 `[engine.i18n.<tag>]`。增加语言时也请补齐这些名称。

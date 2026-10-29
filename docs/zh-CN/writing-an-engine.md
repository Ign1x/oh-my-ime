# 编写引擎

[English](../writing-an-engine.md) | 简体中文

oh-my-ime 引擎就是一个普通的程序。守护进程（`omimed`）启动它，把在获得焦点的文本框中输入的按键发给它，并应用它的回复：上屏的文本、内联的预编辑、删除。它的用户界面完全属于自己；见[显示窗口](#显示窗口)。

本指南使用 Rust SDK（`omime-sdk`）。其他语言的引擎直接使用同一套 D-Bus 协议；见[其他语言](#其他语言)。

## 一个完整的引擎

```rust
use omime_sdk::{Context, Engine, KeyEvent, Preedit, modifiers};

#[derive(Default)]
struct Echo {
    word: String,
}

impl Engine for Echo {
    fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool {
        // 把释放事件和快捷键留给应用。
        if key.released || key.modifiers & (modifiers::CONTROL | modifiers::ALT) != 0 {
            return false;
        }
        match key.text.chars().next() {
            Some(c) if c.is_alphabetic() => {
                self.word.push(c);
                ctx.set_preedit(Preedit::with_cursor_at_end(self.word.clone()));
                true // 已消费
            }
            _ => {
                if !self.word.is_empty() {
                    ctx.commit(std::mem::take(&mut self.word).to_uppercase());
                }
                false // 上屏之后，应用也会收到这个键
            }
        }
    }
}

fn main() -> Result<(), omime_sdk::Error> {
    omime_sdk::run(Echo::default())
}
```

这段代码大致就是 [`engines/echo`](../../engines/echo/src/main.rs)。需要知道的规则：

- `process_key` 同时收到按下**和**释放。返回 `true` 表示消费该键。无论你返回什么，释放事件总是按对应的按下事件来路由，因此应用不会看到卡住的键。
- 通过 `Context` 请求的操作（`commit`、`set_preedit`、`clear_preedit`、`delete_surrounding`、`forward_key`）会**按顺序**在未被消费的按键到达应用**之前**执行。
- 文本的偏移与长度以 UTF-8 字节计，与 Wayland 的 text-input 协议一致。`Preedit::with_cursor_at_end` 会替你计算。
- 尽快回复。200 ms（可配置）之后，按键会被交给应用，迟到的回复被丢弃，随后执行 `reset`。
- `activate`、`deactivate`、`update` 和 `reset` 告知你焦点与字段的情况（`ctx.info()` 包含内容类型、光标周围文本，以及窗口显示期间的光标矩形）。密码和 PIN 字段永远不会到达引擎。

## 安装引擎

引擎是一个以其 id 命名的目录，其中包含 `engine.toml`：

```toml
[engine]
id = "echo"
name = "Echo"
description = "Commits every word in UPPERCASE."
languages = ["en"]
exec = ["omime-echo"]   # 在 $PATH 中查找；"./bin/x" 相对此目录
api = "1.0"

[engine.i18n.zh-CN]
name = "回声"
```

要为当前用户安装这个目录，运行 `omime install <dir>`：它会检查清单，把目录复制到 `~/.local/share/oh-my-ime/engines/<id>`，并让正在运行的 omimed 重新加载。`--force` 会替换之前安装的副本，`omime uninstall <id>` 则将其移除。

omimed 按以下顺序查找引擎目录：

1. `$OMIME_ENGINE_PATH` 中的每个目录（以冒号分隔；开发时很方便），
2. `~/.local/share/oh-my-ime/engines/`，
3. 每个 `$XDG_DATA_DIRS` 条目下的 `oh-my-ime/engines/`，例如 `/usr/share/oh-my-ime/engines/`。

开发期间：

```sh
cargo build
PATH=$PWD/target/debug:$PATH OMIME_ENGINE_PATH=$PWD/engines omimed --engine echo
```

然后用热键切换（`Ctrl+Space` 在直接输入与上一个引擎之间切换，`Ctrl+Shift+Space` 循环切换引擎），或使用 `omime switch <id>`。

## 显示窗口

用任意 Wayland 工具包创建一个无边框窗口，尺寸与你内容相适应。omimed 启动引擎时会把 `WAYLAND_DISPLAY` 指向其 UI Bridge 的私有 socket，因此工具包无需任何特殊代码。只要你的引擎是当前激活的引擎，你的 toplevel 就会成为文本光标旁的输入法弹窗。

- 在调用 `Host::set_popup_visible(true)` 之前，窗口一直隐藏。不要通过隐藏或关闭窗口来让它消失；调用 `set_popup_visible(false)`，由平台取消映射。
- 窗口上的点击会照常到达你的引擎。通过 `omime_sdk::spawn`（或 `ctx.host()`）得到的 `Host` 来响应它们，例如 `host.commit(ctx_id, text)`。
- 对于自己占用主线程的工具包：用 `omime_sdk::spawn` 运行引擎，它会在引擎注册完成后返回。
- 对于不应当作为弹窗的普通窗口（例如设置对话框），改为连接 `$OMIME_HOST_WAYLAND_DISPLAY` 中的 display，例如从一个独立进程连接。

[Sparkle](../../engines/sparkle) 引擎用 Slint 展示了以上全部内容。

## 其他语言

引擎协议是点对点 D-Bus，规范见 [`protocol/io.github.ign1x.OhMyIme.Engine1.xml`](../../protocol/io.github.ign1x.OhMyIme.Engine1.xml)。一个引擎需要：

1. 以点对点连接的方式连到 `$OMIME_ENGINE_ADDRESS` 中的地址，
2. 在 `/io/github/ign1x/OhMyIme/Engine` 导出 `io.github.ign1x.OhMyIme.Engine1`，
3. 在 `/io/github/ign1x/OhMyIme`（`io.github.ign1x.OhMyIme.EngineHost1`）上调用 `Register($OMIME_ENGINE_ID, 1, 0, {})`，
4. 应答调用，直到连接关闭或收到 `Shutdown`。

GLib（`g_dbus_connection_new_for_address`）、Qt（`QDBusConnection::connectToPeer`）、sd-bus 和 zbus 都支持点对点连接。

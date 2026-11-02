# 编写引擎

[English](../writing-an-engine.md) | 简体中文

oh-my-ime 引擎就是一个普通的程序。守护进程（`omimed`）启动它，把在获得焦点的文本框中输入的按键发给它，并应用它的回复：上屏的文本、内联的预编辑、删除。它的用户界面完全属于自己；见[显示窗口](#显示窗口)。

本指南使用 Rust SDK（`omime-sdk`）。其他语言的引擎直接使用同一套 D-Bus 协议；见[其他语言](#其他语言)。

## 一个完整的引擎

```rust
use std::collections::HashMap;
use omime_sdk::{Context, ContextId, Engine, KeyEvent, Preedit, modifiers};

#[derive(Default)]
struct Echo {
    words: HashMap<ContextId, String>,
}

impl Engine for Echo {
    fn activate(&mut self, ctx: &mut Context<'_>) {
        self.words.remove(&ctx.id());
    }

    fn deactivate(&mut self, ctx: &mut Context<'_>) {
        self.words.remove(&ctx.id());
    }

    fn reset(&mut self, ctx: &mut Context<'_>) {
        self.words.remove(&ctx.id());
    }

    fn process_key(&mut self, ctx: &mut Context<'_>, key: &KeyEvent) -> bool {
        // 把释放事件和快捷键留给应用。
        if key.released || key.modifiers & (modifiers::CONTROL | modifiers::ALT) != 0 {
            return false;
        }
        let word = self.words.entry(ctx.id()).or_default();
        match key.text.chars().next() {
            Some(c) if c.is_alphabetic() => {
                word.push(c);
                ctx.set_preedit(Preedit::with_cursor_at_end(word.clone()));
                true // 已消费
            }
            _ => {
                if !word.is_empty() {
                    ctx.commit(std::mem::take(word).to_uppercase());
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

- 多个上下文可能同时激活。组合状态须按 `ctx.id()` 保存；激活、失焦和重置只影响对应上下文。目前各 seat 仍共享引擎选择和候选窗定位，正式支持的 UI 使用流程是单 seat 会话。
- `process_key` 同时收到按下**和**释放。返回 `true` 表示消费该键。无论你返回什么，释放事件总是按对应的按下事件来路由，因此应用不会看到卡住的键。
- 通过 `Context` 请求的操作（`commit`、`set_preedit`、`clear_preedit`、`delete_surrounding`、`forward_key`）会**按顺序**在未被消费的按键到达应用**之前**执行。
- 文本的偏移与长度以 UTF-8 字节计，与 Wayland 的 text-input 协议一致。`Preedit::with_cursor_at_end` 会替你计算。
- 尽快回复。200 ms（可配置）之后，按键会被交给应用；后续按键绕过繁忙的上下文，直到迟到回复被丢弃、排队的 `reset` 完成。切换或失焦后被放弃的回复只会被丢弃，不能重置新会话。
- `activate`、`deactivate`、`update` 和 `reset` 告知你焦点与字段的情况（`ctx.info()` 包含内容类型、光标周围文本，以及窗口显示期间的光标矩形）。Wayland/Fcitx5 明确标记的敏感字段绕过引擎；XIM 不提供此信息。
- 有些应用不显示预编辑（xterm 等把绘制预编辑交给输入法的 XIM 应用）：这时 `ctx.info().client_preedit` 为 `false`，有窗口的引擎应在自己的窗口里显示正在组字的内容。`frontend` 说明应用经由哪条路径到达 omimed（`wayland`、`xim`、`fcitx`）。

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

omimed 不认识的键会被略过并给出警告，因此用到新版本键的清单在旧版本里也能加载。

如果清单随别的软件一起分发，而它所依赖的程序未必已安装，就在 `requires = ["fcitx5"]` 中写出这个程序（`$PATH` 中的名字，或者绝对路径）：只有装了它，omimed 才会提供这个引擎。`enabled_by_default = false` 表示在 `config.toml` 没有列出引擎时，它只被列为可用而不被启用；用户用 `omime enable <id>` 启用它。

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

引擎输出到 stdout 和 stderr 的内容会逐行写进 omimed 的日志，target 为 `engine`，并带上引擎的 id（`INFO engine: … engine="echo"`）。

## 显示窗口

用任意 Wayland 工具包创建一个无边框窗口，尺寸与你内容相适应。omimed 启动引擎时会把 `WAYLAND_DISPLAY` 指向其 UI Bridge 的私有 socket，因此工具包无需任何特殊代码。只要你的引擎是当前激活的引擎，你的 toplevel 就会成为文本光标旁的输入法弹窗。

- 在调用 `Host::set_popup_visible(true)` 之前，窗口一直隐藏。不要通过隐藏或关闭窗口来让它消失；调用 `set_popup_visible(false)`，由平台取消映射。
- 窗口上的点击会照常到达你的引擎。通过 `omime_sdk::spawn`（或 `ctx.host()`）得到的 `Host` 来响应它们，例如 `host.commit(ctx_id, text)`。
- 对于自己占用主线程的工具包：用 `omime_sdk::spawn` 运行引擎，它会在引擎注册完成后返回。
- 对于不应当作为弹窗的普通窗口（例如设置对话框），改为连接 `$OMIME_HOST_WAYLAND_DISPLAY` 中的 display，例如从一个独立进程连接。
- 清单里的设置程序就是 `omime settings <id>` 和托盘菜单打开的东西，它以普通窗口出现在桌面上：

  ```toml
  [engine.settings]
  exec = ["my-engine-settings"]   # 解析方式与 `exec` 相同
  bus = "session"                 # "engine"：被托管输入法的私有总线
  restart = false                 # true：设置程序关闭时重启引擎
  ```

[Sparkle](../../engines/sparkle) 引擎用 Slint 展示了以上全部内容。

## 多个输入法

引擎可以拥有自己的多个输入法，就像 Rime 引擎有多个方案。在 `registered` 中公布它们以及正在使用的那个：它先于其它任何调用运行，因此公布的内容是最新的。之后用户就能以 `engine/id` 选择它们（`omime switch my-engine/fast`、托盘菜单、`Ctrl+Shift+Space`、config.toml 的 `engines`），omimed 会请求你的引擎切换：

```rust
impl omime_sdk::Engine for MyEngine {
    fn registered(&mut self, host: &omime_sdk::Host) {
        let fast = omime_sdk::InputMethodInfo {
            id: "fast".into(),
            name: "Fast".into(),
            ..Default::default()
        };
        host.set_input_methods(&[fast], self.mode.as_str());
    }

    fn select_input_method(&mut self, host: &omime_sdk::Host, id: &str) {
        self.mode = id.to_owned();
        host.set_current_input_method(id);
    }
    // ...
}
```

引擎自己切换时，也要调用 `set_current_input_method`。omimed 会跨重启保留用户的选择，并在引擎启动时交还给它。

## 菜单

开关与动作（例如中文 / 英文标点）放在引擎的菜单里；你的引擎正在使用时，平台会显示它（比如在托盘菜单中）。公布菜单，其中任何内容变化时再公布一次，并在某一项被激活时执行相应操作：

```rust
use omime_sdk::{Host, Property, PropertyKind};

impl MyEngine {
    fn menu(&self) -> Vec<Property> {
        vec![
            Property {
                id: "punct".into(),
                kind: PropertyKind::Toggle,
                label: "中文标点".into(),
                checked: self.chinese_punctuation,
                ..Default::default()
            },
            Property {
                id: "deploy".into(),
                label: "重新部署".into(),
                ..Default::default()
            },
        ]
    }
}

impl omime_sdk::Engine for MyEngine {
    fn registered(&mut self, host: &Host) {
        host.set_properties(&self.menu());
    }

    fn activate_property(&mut self, host: &Host, id: &str) {
        match id {
            "punct" => self.chinese_punctuation = !self.chinese_punctuation,
            "deploy" => self.deploy(),
            _ => return,
        }
        host.set_properties(&self.menu());
    }
    // ...
}
```

菜单是一个按菜单顺序排列的扁平列表。每一项可以是动作（默认）、开关（`checked` 表示是否打开）、单选项（与 `parent` 相同的各项构成一组选择）、子菜单（其中各项的 `parent`）或分隔线。开发时，`omime properties` 显示正在使用的引擎的菜单，`omime activate <id>` 激活其中一项。

## 其他语言

引擎协议是点对点 D-Bus，规范见 [`protocol/io.github.ign1x.OhMyIme.Engine1.xml`](../../protocol/io.github.ign1x.OhMyIme.Engine1.xml)。一个引擎需要：

1. 以点对点连接的方式连到 `$OMIME_ENGINE_ADDRESS` 中的地址，
2. 在 `/io/github/ign1x/OhMyIme/Engine` 导出 `io.github.ign1x.OhMyIme.Engine1`，
3. 在 `/io/github/ign1x/OhMyIme`（`io.github.ign1x.OhMyIme.EngineHost1`）上调用 `Register($OMIME_ENGINE_ID, 1, 1, {})`；既没有自己的多个输入法、也没有菜单的引擎用次版本号 0 也可以，
4. 应答调用，直到连接关闭或收到 `Shutdown`。

GLib（`g_dbus_connection_new_for_address`）、Qt（`QDBusConnection::connectToPeer`）、sd-bus 和 zbus 都支持点对点连接。

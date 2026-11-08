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
- Wayland 字符串的上限为 **4,000 字节**。omimed 会沿 UTF-8 字符边界拆分较长的上屏文本，通过连续的提交周期完整、按序送达，并在之后才转发按键。应用可能把每一段视为独立的撤销步骤。预编辑会被截断至该上限，光标偏移会被限制在保留文本内的字符边界；`-1` 仍表示隐藏光标。每个 seat 最多保留 **64 KiB 待送达的上屏文本**。超出该额度的回复会被拒绝；超过 128 个操作，或 256 项输出队列剩余空间不足（每个操作最多预留两项）的回复也会被拒绝。每个已接受的按下事件还会为其释放事件预留一项。队列满时会丢弃后续按下事件，包括托管输入法转发的按键和未被消费的按键；已接受的文本仍按序送达，必要的释放事件会被保留，即使它们来自被拒绝的回复。被托管的输入法在一次 `commit` 之前通过 `commit_string` 累积的文本同样受 64 KiB 上限约束。拒绝时日志不会包含文本，触发回复的按键仍会完成处理。等待应用反馈可能使每段延迟最多 200 ms，因此需要及时送达后续按键时，应尽量保持回复简短。
- 尽快回复。默认 200 ms 的按键期限包含在守护进程中排队的时间；首次激活保留有界的词库加载窗口，但每次已发送的按键请求仍最多等待一个按键超时间隔。超时后按键交给应用，后续按键绕过繁忙的上下文，直到迟到回复被丢弃、排队的 `reset` 完成。切换或失焦时取消守护进程的请求，迟到回复不能重置新会话。
- 相邻字段更新可能按字段合并，保留每个按键、重置或模式恢复之前的最新值。队列饱和也会让已接收按键按顺序各直通一次；守护进程等待旧请求结束，随后重置、同步字段并恢复记忆的模式，才发送新的组合按键。无法恢复的请求由现有超时监控立即终止引擎。
- Fcitx 输出使用独立的有界队列：每个输入上下文 128 项 / 64 KiB，每条总线 512 项 / 256 KiB；单次操作事务最多 63 个操作 / 32 KiB 文本。相邻的替换预编辑会合并，上屏、删除、转发按键和回复保持顺序。超限或总线写入停滞 300 ms 时会结束受影响的组合并完成待处理按键，重新聚焦后恢复组合，修改能力不会解除退役状态。已接受按键会预留精简回复记录（每个上下文 64 个 / 每条总线 256 个），直到方法返回值实际写入才释放；批量文本仍受输出字节额度约束。记录已满时，由一个 Fcitx 分派器等待额度，不创建等待写入回复的任务；同一总线上的其他 IC 调用可能延迟，守护进程控制服务仍使用独立连接。
- 原生 `EngineHost1` 文本回调（`Commit`、`SetPreedit`、`DeleteSurrounding`、`ForwardKey`）进入核心事件队列前须取得数量和字节额度：每个上下文 64 次 / 64 KiB，每个引擎连接 256 次 / 256 KiB，全局 1,024 次 / 1 MiB，包含保留文本的分配容量和事件元数据。不会创建等待额度的任务。超出的回调整条拒绝；普通调用收到 `LimitsExceeded`，无需回复的调用没有响应。已接受的回调按下事件还为对应释放预留一项额度（相同连接、上下文和键码），文本队列满时释放仍能转用该额度。预留也计入相同数量和字节额度，上下文或连接退役时清理，不创建等待任务。事件被消费或丢弃后释放额度，后续回调可以重试。SDK 的 `Host` API 使用无需回复的调用，可能突发发送；此类文本回调因超限被拒绝时会丢失，SDK 无法收到拒绝结果。异步输出应保持简短；普通 `Context` 操作随按键回复返回。健康总线的一次 64 回调突发有足够输出条目空间容纳上屏生成的预编辑清除。
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
enabled_by_default = false

[engine.i18n.zh-CN]
name = "回声"
```

omimed 不认识的键会被略过并给出警告，因此用到新版本键的清单在旧版本里也能加载。

如果清单随别的软件一起分发，而它所依赖的程序未必已安装，就在 `requires = ["fcitx5"]` 中写出这个程序（`$PATH` 中的名字，或者绝对路径）：只有装了它，omimed 才会提供这个引擎。`enabled_by_default = false` 表示在 `config.toml` 没有列出引擎时，它只被列为可用而不被启用；用户可用 `omime enable <id>` 或设置应用中的“添加输入法…”启用它。Echo、Sparkle 以及 Fcitx 5、IBus 的清单都采用此设置。省略该键则默认启用引擎。

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

`--engine echo` 可立即试用 Echo。在另一个终端运行 `omime enable echo`，将其加入热键切换顺序（`Ctrl+Space` 在直接输入与上一个引擎之间切换，`Ctrl+Shift+Space` 循环切换已启用的引擎）。`omime switch <id>` 也可直接选择尚未启用的引擎。

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

## 浮动面板

在用户打字时一直留在桌面上的窗口，比如状态条、软键盘或调色板，不是弹窗：它属于 `$OMIME_HOST_WAYLAND_DISPLAY`（`omime_sdk::ENV_HOST_WAYLAND_DISPLAY`）中的合成器，是一个不接收键盘（`keyboard_interactivity` 为 none）的图层表面（`zwlr_layer_shell_v1`），这样键盘焦点留在文本框里。一个进程通常无法用同一个工具包连接两个 display，所以为面板启动一个辅助进程，把它的 `WAYLAND_DISPLAY` 设为那个 display，再通过管道与它通信；上屏仍由引擎来做，用 `Host::commit` 提交到它服务的文本框。

- 用 Slint 时，`omime-overlay` 把图层表面作为 Slint 的平台：调用 `omime_overlay::install(namespace, placement, Pointer::Take)`，然后显示组件，再调用 `Overlay::show(width, height)` 和 `Overlay::hide()`。它用 Slint 的软件渲染器绘制，而软件渲染器只用一种颜色绘制文字字形：彩色 emoji 要作为图片绘制。
- 其他工具包也可以：gtk4-layer-shell、layer-shell-qt。
- 只在引擎服务某个文本框时（`activate` 与 `deactivate` 之间）显示面板，并给图层表面一个你自己的命名空间，方便合成器规则使用。

Sparkle 的最近使用表情面板（在它的菜单里打开）完整演示了以上做法，见 [`engines/sparkle/src/panel.rs`](../../engines/sparkle/src/panel.rs)。

## 多个输入法

引擎可以拥有自己的多个输入法，就像 Rime 引擎有多个方案。在 `registered` 中公布它们以及正在使用的那个。守护进程可能在注册回复到达 SDK 之前恢复选择，因此应公布当时的实际状态。之后用户就能以 `engine/id` 选择它们（`omime switch my-engine/fast`、托盘菜单、`Ctrl+Shift+Space`、config.toml 的 `engines`），omimed 会请求你的引擎切换：

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

## 记忆输入模式

协议 1.2 提供可选的模式记忆能力：同一输入法在聊天中使用中文，在终端中使用英文字母。采用 `native`、`latin` 等稳定且不翻译的 ID，显示用的 `中`、`A` 等标签单独处理。用户同时开启 `apps.remember` 和 `apps.remember_mode` 后生效；后者默认关闭。

通过 `Engine::input_modes` 声明支持的 ID，实现 `set_input_mode`，按引擎原有的组合文本策略执行变更，然后通过上下文确认实际结果。对用布尔值保存模式的引擎，相关回调如下：

```rust
impl omime_sdk::Engine for MyEngine {
    fn input_modes(&self) -> Vec<String> {
        vec!["native".into(), "latin".into()]
    }

    fn set_input_mode(&mut self, ctx: &mut omime_sdk::Context<'_>, mode: &str) {
        self.latin = match mode {
            "native" => false,
            "latin" => true,
            _ => return,
        };
        ctx.report_input_mode(&self.schema, if self.latin { "latin" } else { "native" });
    }
    // ...
}
```

输入法 ID 属于引擎内部：传入完整方案 ID，例如 `double_pinyin_flypy`，不要加上 `rime/` 前缀；没有子输入法的引擎传入 `""`。激活时，以及按键或重置改变模式时，都要报告实际模式。空回复不代表恢复成功；未知模式应保持引擎不变。回调以外发生的变化用 `host.report_input_mode(ctx_id, method_id, mode)` 报告，并与回调使用同一把状态锁。`SetStatus` 标签只用于显示，不能代替模式确认。

SDK 为上下文动作和异步报告分配共用的递增序号。守护进程结合上下文、方案、连接和序号拒绝过期信息，避免较早按键的回复覆盖较新的菜单操作。直接实现 D-Bus 的引擎在 `Register` 能力字典中声明 `"input-modes"`（`as`），返回种类为 4、载荷为 `(method, mode, revision)`（`sst`）的动作，或调用 `ReportInputMode(context, method, mode, revision)`。序号从 1 开始，在观察到实际状态时分配，再发送动作或报告。完整签名见 XML 协议文件。

现有 1.0/1.1 引擎继续正常工作，只是不支持模式恢复。新 SDK 遇到低于 1.2 的守护进程时自动省略模式报告；直接实现协议的客户端也须检查 `Register` 回复中的 `api-minor`。

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

`SetProperties` 每次更新只处理通过协议发送的前 **256 项**，无效项和重复项也计入这个数量；超出时记录一次警告。在这 256 项中，每个 `id` 只保留首次出现的一项。父级链中存在环（包括引用自身）的子菜单会被丢弃。父级不存在或不是子菜单的条目不会显示，被丢弃子菜单的子项也不会移到顶层。每个属性最多生成一个托盘菜单条目。

托盘点击绑定到提供该条目的引擎；守护进程处理点击时，如果当前已经切换到另一引擎或直接输入，就会忽略它。控制 API 和 `omime activate <id>` 仍然操作当前引擎。

## 退出时保存状态

不要将按键、周围文本或上屏文本打印到 stdout 或 stderr：omimed 默认以 INFO 转发这两个输出流。
远端 D-Bus 错误正文仅在 debug 级别记录，普通错误仍保留引擎 ID、方法和错误名。
`OMIME_LOG=info,engine=warn` 可关闭引擎输出转发。

守护进程关闭、重启引擎、重新加载设置或卸载引擎时，原生引擎会收到 `Shutdown`，并有 **两秒** 保存状态和退出。如果仍未退出，omimed 会发送 SIGTERM，再等两秒后发送 SIGKILL。输入超时监控判定为卡死的引擎仍会被立即杀死。

Rust SDK 收到 `Shutdown`、主机连接关闭或 SIGTERM 后，`run` 会返回，`spawn` 创建的线程会结束。此时应完成保存并关闭界面；窗口连接失败时也必须清理状态。Rime 在每个返回路径上都持有共享状态锁完成 librime 的清理。如果 omimed 自身被杀死，原生引擎会通过 `PR_SET_PDEATHSIG` 收到 SIGKILL，避免卡死的清理过程在监督进程退出后残留。因此，重要状态也应在正常运行期间及时保存。

## 其他语言

引擎协议是点对点 D-Bus，规范见 [`protocol/io.github.ign1x.OhMyIme.Engine1.xml`](../../protocol/io.github.ign1x.OhMyIme.Engine1.xml)。一个引擎需要：

1. 以点对点连接的方式连到 `$OMIME_ENGINE_ADDRESS` 中的地址，
2. 在 `/io/github/ign1x/OhMyIme/Engine` 导出 `io.github.ign1x.OhMyIme.Engine1`，
3. 在 `/io/github/ign1x/OhMyIme`（`io.github.ign1x.OhMyIme.EngineHost1`）上调用 `Register($OMIME_ENGINE_ID, 1, 1, {})`；既没有自己的多个输入法、也没有菜单的引擎用次版本号 0 也可以，
4. 应答调用，直到连接关闭或收到 `Shutdown`。

GLib（`g_dbus_connection_new_for_address`）、Qt（`QDBusConnection::connectToPeer`）、sd-bus 和 zbus 都支持点对点连接。[`examples/python-gtk4`](../../examples/python-gtk4) 是一个用 Python 和 PyGObject 写成、不依赖 SDK 的完整引擎，它的 GTK 4 窗口和其他引擎的窗口一样成为弹窗。

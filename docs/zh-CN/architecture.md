# oh-my-ime 架构

[English](../architecture.md) | 简体中文

本文说明 oh-my-ime 由哪些部分组成，以及为什么这样设计。它是贡献者的参考文档；README 里是简版。

## 目标

- 输入法（“引擎”）是独立进程，完全掌控自己的 UI。
- 平台负责一切必须按 seat 唯一的东西（合成器协议、按键路由、焦点、切换），并负责送达文本。
- Wayland 优先、Hyprland 优先，但不把设计绑死在某一个合成器上。
- 引擎崩溃或卡死都不会打断打字。
- 通过标注清晰的兼容层，与 Fcitx5 和 IBus 生态兼容。

## 决定设计的约束

以下事实来自协议本身和合成器源码，也正是设计长成这样的原因。

1. **每个 seat 只有一个输入法。** `zwp_input_method_manager_v2.get_input_method` 只对单个客户端成功。之后绑定的客户端会收到 `unavailable`，并处于失活状态。槽位空出时不会有通知，因此客户端必须重新绑定。
2. **弹窗属于输入法自己的连接。** `get_input_popup_surface` 接受一个 `wl_surface` 参数，而 libwayland 在发送方客户端自己的对象表里解析对象参数。由其他进程创建的 surface 永远无法成为输入法弹窗。
3. **弹窗由合成器摆放。** 输入法只能得知文本光标矩形相对于其弹窗的位置（`text_input_rectangle`）。Hyprland 把弹窗放在光标下方，靠近底边时翻到上方，靠近右边时向左平移。
4. **Hyprland 不会在 `deactivate` 时隐藏弹窗。** 只要文本框持有键盘焦点，它就会绘制弹窗。输入法必须自己取消映射。
5. **虚拟键盘何时绕过抓取。** 如果某个 `zwp_virtual_keyboard_v1` 与当前键盘抓取属于同一个客户端，通过它发送的按键会同时绕过抓取和 Hyprland 的快捷键绑定。来自其他虚拟键盘的按键则会被路由**进**抓取。
6. **存在抓取时，修饰键只送给抓取。** 输入法必须把它们转发给自己的虚拟键盘，否则应用会看到过期的修饰键状态。
7. **虚拟键盘必须先收到 keymap，然后才能收到任何按键或修饰键事件。** 否则就是协议错误，而协议错误对整个连接都是致命的。
8. **`text-input-v3` 在每次 `done` 时清空预编辑**，而 Hyprland 只在输入法提交（`commit`）之后才发出 `done`。因此输入法必须在每次 `done` 之后提交一次，并重新发送当前的预编辑。
9. **XWayland 不支持 text-input。** X11 客户端需要 XIM 或 GTK/Qt 输入法模块。
10. **Hyprland 会合并所有键盘的状态**（`input:virtualkeyboard:share_states`）。只有在输入法持有抓取期间，它才把该输入法的虚拟键盘排除在外。`share_states = 1` 时，其余时间它都会合并这个虚拟键盘，于是虚拟键盘上仍按着的修饰键对所有应用都一直按着。默认值 2 时，Hyprland 一旦在输入法的抓取期间见到它的虚拟键盘，就不再合并这个虚拟键盘。

## 进程

```
合成器 ── 一条客户端连接 ── omimed ──┬── D-Bus 点对点 ── 引擎进程（逻辑）
                                     └── 私有 Wayland socket ── 引擎 UI
```

- **`omimed`**：守护进程。一个单线程 actor（tokio `current_thread` + `LocalSet`）持有 `wl-proxy` 状态，因而持有每一个 Wayland 对象。D-Bus 服务（zbus）运行在同一个 runtime 上，通过 channel 与该 actor 通信。
- **引擎**：各自一个进程，由 `omimed` 启动并监护。
- **`omime`**：命令行工具。它通过会话总线与守护进程通信。

## Wayland 前端

对每个 `wl_seat`，`omimed` 绑定一个 `zwp_input_method_v2`，并维护一个输入上下文：

- **激活。** 收到 `activate`+`done` 后，该上下文获得焦点。如果内容类型敏感（密码、PIN、`sensitive_data`、`hidden_text`），就不做键盘抓取，按键也从不经过守护进程。否则守护进程抓取键盘。
- **抓取的生命周期。** 只在上下文激活期间持有抓取。释放之前，守护进程会先释放它在虚拟键盘上仍然按下的每一个键；之后销毁虚拟键盘，虚拟键盘只在抓取存在期间存在（见上面第 10 条）。新的抓取只在旧的抓取销毁之后才创建。
- **按键。** 抓取事件用 xkbcommon 解码（keysym、UTF-8、修饰键）。热键优先处理。其余按键交给当前引擎，由引擎的回复决定该键是被消费还是通过虚拟键盘转发。按下未被消费的按键，其释放事件总会转发，包括在抓取开始之前就按下的键。
- **重复。** 应用会自己重复被转发的按键。守护进程只为引擎消费的按键合成重复事件（依据 `repeat_info`），并且在合成器发出自己的重复事件后关闭这一行为。
- **提交纪律。** 每次 `done` 之后，守护进程都会发送一次 `commit`，携带当前的预编辑，无论内容是否变化。
- **获得焦点的应用。** 输入法不会被告知文本框属于哪个应用。如果合成器提供 `zwlr_foreign_toplevel_manager_v1`（Hyprland、sway 等），守护进程会跟踪哪个窗口处于激活状态，并把它的 app id 传给引擎（`app-id`）；启用 `[apps] remember` 后，核心会为每个应用切换回上次在其中选择的引擎。守护进程从不读取窗口标题。

## UI Bridge

Bridge 是基于 [`wl-proxy`](https://crates.io/crates/wl-proxy) 构建的多路复用 Wayland 代理。所有引擎连接共享守护进程的上游连接，因此在合成器看来，它们的 surface 就是输入法自己的 surface。

- **逐引擎的 socket。** 每个引擎得到 `$XDG_RUNTIME_DIR/oh-my-ime/<engine-id>/wayland`（目录权限 `0700`）。`WAYLAND_DISPLAY` 被设为这个相对名称，`DISPLAY` 被移除，以免工具包悄悄回退到 X11。守护进程自己接受连接，因此始终知道某个客户端属于哪个引擎。
- **全局对象过滤。** 引擎看到的是一个虚拟 registry。它包含 UI 需要的全局对象（合成器、子合成器、shm、不带键盘能力的 seat、输出、xdg-output、linux-dmabuf、viewporter、分数缩放、光标形状、单像素缓冲、呈现时间），并隐藏其余一切（text input、input method、虚拟键盘、layer shell、捕获类协议）。绑定时会核对通告的版本，并在守护进程自己的 registry 上执行。守护进程同步获知全局对象，因此引擎完成第一次 roundtrip 时 registry 绝不会是残缺的。
- **虚拟协议。** `xdg_wm_base` 由 Bridge 代为应答：引擎创建一个普通的无边框 `xdg_toplevel`，Bridge 用 `configure(0, 0, [])` 应答初始 commit，并关闭 `xdg_popup`。虚拟的 `zxdg_decoration_manager_v1` 总是回答“服务器端”，于是工具包会去掉标题栏；虚拟的 `wl_data_device_manager` 为强依赖剪贴板的工具包提供空剪贴板：引擎永远看不到用户的剪贴板。
- **弹窗生命周期。** 每个引擎 surface 只在应当映射它的那次 commit 之前获得一次 input-popup 角色，并终身持有。窗口在引擎调用 `SetPopupVisible(true)` 之前一直隐藏，且只显示当前激活引擎的窗口。隐藏窗口时，Bridge 注入 `attach(NULL)` + `commit`，并吞掉合成器对最后一个 buffer 的 release，代合成器扣住该 buffer；再次显示时立即重新 attach 该 buffer，无需等待工具包重绘。隐藏期间提交的 buffer 会替换被扣住的那个。当 X11 应用拥有焦点且没有 Wayland 文本框时，显示中的窗口改为出现在镜像上；见 [X11 应用](#x11-应用)。
- **缩放。** 合成器只在输入法弹窗映射之后才报告其分数缩放；Bridge 会把最近一次已知的缩放立即交给新出现的引擎 surface。镜像的首选分数缩放会像来自引擎自己的 surface 一样送达引擎。
- **帧回调。** 引擎窗口的帧回调由 Bridge 自己应答：它向当前显示该窗口的 surface（弹窗或镜像）请求一帧，并在该 surface 的帧到达时应答引擎的回调。窗口在两者之间移动时，工具包不会卡住。
- **输入路由。** 指针的 `enter`、`motion`、`button`、`axis` 和 `frame` 只送给指针进入其 surface 的那个引擎。`wl_surface.enter(output)` 之类的事件按所属者过滤。镜像上的指针事件会作为其自身 surface 上的事件送达引擎。
- **清理。** 引擎断开或死亡时，按依赖顺序销毁它的合成器对象：先弹窗角色，再 surface 扩展、子 surface、surface、buffer 和 pool，最后是 seat 与 output 对象。若引擎自己销毁了 surface，则先销毁弹窗角色。
- **故障处理。** 合成器视为致命、且容易检查的请求（shm pool 与 buffer、绑定版本、虚拟对象）会做校验，出错的引擎被单独断开。其他任何导致合成器关闭共享连接的情况，都会让守护进程重新连接、重新绑定输入法并重启引擎；端到端测试覆盖了这一点。

## X11 应用

X11 应用不使用 `text-input`，Wayland 前端因此够不到它们。另有两个 X11 前端，两者都汇入同一个核心和同一批引擎；引擎并不知道某个按键来自哪个前端。

- **XIM。** 基于 [`xim`](https://crates.io/crates/xim) 与 [`x11rb`](https://crates.io/crates/x11rb) 两个 crate。启动时若设置了 `DISPLAY`，omimed 就成为该显示上的 XIM 服务端 `omime`；应用通过 `XMODIFIERS=@im=omime` 找到它。X 服务器尚未启动时，omimed 以递增的延迟重试（2 秒起，每次翻倍，最长 60 秒）；X 服务器消失后它会重新连接。只有按下事件经过 XIM，释放事件直接送达应用。libX11 会把上屏文本或被回送的按键插入应用事件队列的最前面，连续两个就会顺序错乱：因此 omimed 把它们作为同步请求逐个发送，等应用回复 `XIM_SYNC_REPLY`（最多 300 ms）之后才发下一个。keysym 来自 X 服务器的核心键盘映射。
- **Fcitx5 模块。** omimed 在 `org.fcitx.Fcitx5` 与 `org.freedesktop.portal.Fcitx` 这两个总线名下提供 `org.fcitx.Fcitx.InputMethod1` 和 `org.fcitx.Fcitx.InputContext1`；当别的程序（例如运行中的 fcitx5）持有这些名字时，omimed 排队等待。每个应用上下文对应一个输入上下文对象，只有它的所有者（应用的唯一总线名）可以调用。按键以 `ProcessKeyEvent` 或 `ProcessKeyEventBatch` 到达；上屏文本、预编辑、对周围文本的删除以及被回送的按键都作为信号在该按键的回复之前送达应用。具备 `KeyEventOrderFix` 能力时，引擎未消费但产生了文本的按键会以已处理作答，并在文本之后回送。密码输入框绕过引擎，与其他途径相同。
- **引擎窗口。** 合成器只为 Wayland 文本框显示输入法弹窗。当 X11 应用拥有焦点且没有 Wayland 文本框时，Bridge 把当前引擎的窗口显示在自己创建的图层表面上，即*镜像*：图层为 `overlay`，命名空间为 `omime-popup`，需要 `zwlr_layer_shell_v1`（Hyprland、sway 等 wlroots 合成器都有）。镜像以引擎 surface 的缩放、变换和 viewport 显示引擎的缓冲区；引擎自己的 surface 在此期间保持未映射。镜像开在文本光标下方，下方没有空间时开在上方，会超出输出右侧时向左平移。光标来自 Fcitx5 模块的 `SetCursorRect`，或 XIM 的 spot 位置（应用绘制文本处，omimed 假定一行从基线上方 16 像素延伸到下方 6 像素）。X11 坐标按名称匹配 XWayland 的 RandR 监视器与合成器的输出（xdg-output）来映射为合成器坐标，这也涵盖了 Hyprland 的 `xwayland:force_zero_scaling`。

## 引擎协议（v1）

引擎连接到一个私有的点对点 D-Bus socket，地址在 `OMIME_ENGINE_ADDRESS` 中。

- `io.github.ign1x.OhMyIme.Engine1` 由引擎在 `/io/github/ign1x/OhMyIme/Engine` 导出。它包含 `Activate`、`Deactivate`、`UpdateContext`、`ProcessKey`、`Reset` 和 `Shutdown`。`ProcessKey` 返回 `(handled, actions)`，其中 `actions` 是一个有序列表，包含上屏、预编辑、删除周围文本和转发按键等操作。
- `io.github.ign1x.OhMyIme.EngineHost1` 由守护进程在 `/io/github/ign1x/OhMyIme` 导出。它包含 `Register`、带外操作（例如在鼠标点击候选词之后上屏）、`SetPopupVisible`、`SetStatus` 和 `RequestSwitch`。除 `Register` 外，一切都是发后即忘。
- 按键事件默认 200 ms 超时。超时后按键被转发给应用，迟到的回复被丢弃，随后发送 `Reset`。
- 引擎不得隐藏自己的工具包窗口。它们通过 `SetPopupVisible` 请求平台处理。
- 协议按接口名区分版本（主版本号），并在 `Register` 时协商（次版本号）。可扩展参数使用 `a{sv}`，未知键被忽略。`protocol/` 中的 XML 是权威来源。

## 被托管的输入法

未修改的 Wayland 输入法（例如 Fcitx 5）在清单中声明 `protocol = "input-method-v2"` 时，就作为引擎运行。它没有引擎协议 socket：它像任何引擎的 UI 一样连接到自己的 bridge socket，而它的 registry 会多出两个全局对象，即虚拟的 `zwp_input_method_manager_v2` 和虚拟的 `zwp_virtual_keyboard_manager_v1`，两者都由 Bridge 应答。

- **激活。** 核心像合成器那样为文本框激活这个虚拟输入法：`activate`、文本框的光标周围文本与内容类型、`done`。每个按键都带着按下时的修饰键进入输入法的键盘抓取，因此一批按键会按顺序到达。抓取会拿到所在 seat 的 keymap 与重复速率（X11 文本框没有重复；它们的按键由 X 服务器重复）。输入法对文本框所做的操作（`commit_string`、`set_preedit_string`、`delete_surrounding_text`，然后 `commit`）会作为引擎动作返回；如果一次 `commit` 的 serial 早于该文本框被激活的时间，就会被丢弃。
- **它不想要的按键。** 输入法通过自己的虚拟键盘转发这些按键。Bridge 把它们变成 `ForwardKey` 动作，因此合成器看到的始终只有 omimed 自己的虚拟键盘，并由 Bridge 记录应用当前按住了哪些键。按键会为一个仍在启动、或尚未抓取键盘的被托管引擎最多等待一秒，之后便直接放行。
- **X11 文本框。** XIM 与 Fcitx5 模块应用的文本框使用任意 seat 上的输入法。XIM 只传递按下事件，因此每个送达的按键都会立刻被释放，修饰键则留给应用：否则输入法会把单独按一下 Shift 当成一次点按。
- **窗口。** 输入法的弹窗 surface 就是引擎窗口，以输入法弹窗的形式或显示在镜像上，文本光标矩形则通过它的弹窗角色送达它。输入法的其他窗口（toplevel）无法显示。
- **进程。** omimed 在引擎的运行时目录中用一个私有会话总线启动输入法，即一个不提供服务激活的 `dbus-daemon`，这样它就无法从 omimed 的 Fcitx5 前端手中夺走用户总线上的 `org.fcitx.Fcitx5`。它会拿到自己 bridge socket 的 `WAYLAND_DISPLAY`，但拿不到 `DISPLAY`。omimed 退出时引擎都会收到 SIGTERM，即使 omimed 是被杀死的也一样（`PR_SET_PDEATHSIG`）；被托管的输入法最多有两秒来保存自己的状态。每个 seat 只有一个输入法：第二次 `get_input_method` 会得到 `unavailable`。

## 兼容层（计划中）

- **逐引擎宿主：** 一个 IBus 引擎宿主（私有总线、组件发现、Factory/Engine 转换）和一个 Fcitx5 插件宿主（C++、libFcitx5Core）。

## 安全说明

- 敏感字段完全绕过守护进程（不抓取）。
- 默认不记录按键内容。
- socket 位于 `0700` 目录中，对端用 `SO_PEERCRED` 校验。
- 以同一用户身份运行的进程不构成安全边界。对引擎做沙箱隔离（例如使用 bubblewrap）属于后续工作。

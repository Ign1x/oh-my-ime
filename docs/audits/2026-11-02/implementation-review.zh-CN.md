# oh-my-ime 实现与需求核查

审查日期：2026-11-02。基线：`main` / `d9899fcfb3d385ce2b77048bc3b28df78e93c346`，开始审查时工作区干净。

本次按仓库定位理解需求：在 Linux 上提供类似 macOS/Windows 的输入平台体验，让输入法独立开发、独立绘制 UI，由平台负责系统接入，用户安装并选择后即可在支持的应用中使用。

## 判断

核心架构与目标一致：独立引擎进程、统一按键与文本路由、引擎自有 UI、兼容既有生态，这几项已经有实际实现和测试。没有理由因为这次发现的问题推翻整个项目。

当前更准确的定位是 **Hyprland/sway 上具备原生引擎和 Fcitx5/IBus 托管能力的平台原型**。距离普通用户安装后在常用软件中稳定使用，仍缺输入正确性的边界处理、安装接管闭环、真实应用验收，以及主流桌面覆盖。路线图主要按技术模块完成情况推进，对用户使用结果的验收不足。

现有测试全部通过，但额外设计的五个复现全部暴露实际问题。前三个直接涉及日常输入；第四个影响整个服务；第五个说明多 seat 的完成声明超出了现有实现。

## 已复现的问题

### F1 · P1：延迟回送的按键丢失原来的修饰键

位置：[seat.rs:422](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/wayland/seat.rs:422)、[seat.rs:514](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/wayland/seat.rs:514)。

复现：使用仓库自带 `omime-dummy-ui --slow-ms 80`，按下 Shift，20 ms 后输入 a，再过 20 ms 释放 Shift。预期输入 `A`，实际输入 `a`。80 ms 在默认 200 ms 超时范围内，不需要引擎故障就会发生。

原因：修饰键变更立即发给虚拟键盘；普通按键却等待引擎答复。回送时仅发送 keycode，没有恢复 `KeyRef` 已保存的按键发生时的 modifiers，应用因而用释放后的状态解释旧按键。Ctrl/Alt 快捷键也存在同类风险，本次具体复现的是 Shift。

修复方向：按键及其修饰键必须按同一顺序投递；回送应使用事件快照，并正确恢复后续状态。托管输入法的 ForwardKey 路径也应检查同样的时序问题。

复现测试：`delayed_forwarding_preserves_the_keys_original_shift`。

### F2 · P1：旧引擎的迟到回复清空新引擎的组合输入

位置：[core/mod.rs:1230](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/core/mod.rs:1230)、[core/mod.rs:1628](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/core/mod.rs:1628)。

复现：旧引擎延迟答复 a，切换到 Echo 并输入 bc；旧回复返回后按空格。预期 `aBC `，实际 `a `，已经出现的 `bc` 预编辑被清空。

原因：切换时将旧请求 token 标记为 stale；迟到回复到达后，却通过该 context **当前**的 engine 找到 proxy 并 Reset，同时清除当前 preedit。旧请求所属引擎、进程代次和组合输入代次没有被用于判断清理对象。

修复方向：过期请求应保留所属引擎与会话代次，旧回复不能操作新引擎或更新后的组合状态。Reset 的排序还需与正常请求队列统一。

复现测试：`late_reply_from_previous_engine_preserves_new_composition`。

### F3 · P1：失焦后，正在处理的 Fcitx 按键请求永远没有答复

位置：[core/mod.rs:895](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/core/mod.rs:895)、[fcitx/mod.rs:443](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/fcitx/mod.rs:443)。

复现：通过真实 D-Bus 接口发出 ProcessKeyEvent，让引擎延迟 450 ms；50 ms 后由同一客户端发送 FocusOut。等待 3 秒仍收不到该按键调用的回复，尽管引擎早已返回。

原因：Deactivated 删除 context，仅把 in-flight token 标为 stale；它结算了队列中的键，却没有结算 `flight.key`。Fcitx frontend 的 `pending` 中仍保留回复 sender，context 消失后也没有超时路径再处理它。

影响：异步 GTK/Qt 模块可能一直等待该键，直到客户端自身超时；反复触发还会留下 pending 状态。不能将“引擎默认 200 ms 超时”视为覆盖了失焦场景。

修复方向：失焦、销毁、切换、崩溃都应确保每个待处理按键恰好结束一次；对已经失焦的应用避免提交旧文本，同时按前端协议完成原 RPC 的回复。

复现测试：`focus_out_finishes_in_flight_fcitx_key`。

### F4 · P1：一个持续发送错误 UI 请求的引擎会拖垮整个 daemon

位置：[bridge/mod.rs:1647](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/wayland/bridge/mod.rs:1647)、[daemon.rs:327](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/daemon.rs:327)。

复现：运行自带故障引擎 `omime-dummy-ui --misbehave viewport`，不指定 `--misbehave-once`。它发送非法 viewport 尺寸，导致共享上游连接断开；约 21 秒内断开六次后，omimed 输出 `the connection keeps failing; giving up` 并退出。

原因：该非法请求未经校验就送到合成器，破坏的是所有引擎共用的连接。daemon 的全局断线阈值先于故障引擎的隔离生效。

影响：整个平台的输入法服务被一个坏引擎反复重置并最终停止。普通键盘是否还能直输取决于应用与合成器，不能把此次结果描述为操作系统键盘彻底失效。systemd 自动重启也不能代替隔离故障引擎。

现有 [robustness.rs:37](/srv/storage/WorkSpace/oh-my-ime/tests/e2e/tests/robustness.rs:37) 只验证“出错一次、重启后主动恢复正常”的引擎，未覆盖持续故障。

修复方向：补齐可验证的 Wayland 请求检查，记录错误对象所属引擎，并在重连前停止重新激活故障来源。长期应明确共享上游连接的故障边界，使“重连成功”和“单引擎故障隔离”有各自的验收标准。

复现测试：`a_repeat_protocol_fault_does_not_terminate_the_daemon`。

### F5 · P2：多 seat 会把两个输入框的组合内容混在一起

位置：[Echo:16](/srv/storage/WorkSpace/oh-my-ime/engines/echo/src/main.rs:16)、[Core:295](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/core/mod.rs:295)、[update_bridge:478](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/wayland/mod.rs:478)。

复现：seat0 在窗口 A 输入 ab，暂不上屏；seat1 在窗口 B 输入 cd 并按空格；随后 seat0 按空格。预期 A=`AB `、B=`CD `；实际 A=` `、B=`ABCD `。

直接原因是 Echo 对所有 context 共用一个 `word`。SDK 虽提供 ContextId，示例引擎却未按 context 保存组合状态；Sparkle 也只保存一个当前 ctx/composer。平台自身还只有全局 current engine，切换会遍历全部 context；UI Bridge 只选择一个有焦点的 seat 来承载窗口。因此“多个 seat 各自拥有独立输入法会话”的用户体验尚未完整成立。

现有 [seats.rs:49](/srv/storage/WorkSpace/oh-my-ime/tests/e2e/tests/seats.rs:49) 先在第一个 seat 完成上屏，再让第二个 seat 输入，刚好避开组合状态交叠。

修复方向：明确区分 context 的组合状态、seat 的输入法选择和 popup 所属上下文；示例及 SDK 文档应展示按 context 管理状态。若近期只交付单 seat，应据此收窄状态表。

复现测试：`simultaneous_seats_keep_compositions_separate`。

## 与最初需求的对照

| 用户需要的结果 | 当前实现 | 判断 |
|---|---|---|
| 输入法独立开发，UI 技术自由选择 | P2P D-Bus、Rust SDK、独立进程、Wayland UI Bridge；Sparkle 已能绘制和接收点击 | 核心方向正确，具有继续建设价值 |
| 安装平台、安装输入法、选择后开始输入 | Nix 包、源码安装脚本、manifest 目录安装、CLI 和托盘；仍需手动停旧框架、设环境变量、配置自启动、补模块依赖 | 安装功能存在，开箱使用流程尚未完成 |
| 在常用软件中保持一致的输入体验 | Wayland text-input、Fcitx 模块、XIM 三条接入路径；已测试协议客户端 | 接入基础较完整，真实应用兼容性证据不足 |
| 覆盖常见 Linux 桌面 | 硬依赖 input-method-v2 和 virtual-keyboard-v1；KWin 前端待做，Mutter 暂未规划 | Hyprland 优先可作为阶段目标，不能等同于通用 Linux 支持 |
| 新旧输入法都容易使用 | 原生示例为 Echo/Sparkle；实际中文输入通过托管 Fcitx5/IBus 已打通 | 兼容宿主有实际用途；独立原生复杂输入法的开发体验仍需验证 |
| 输入法卡住、崩溃时仍能稳定打字 | 普通崩溃重启、按键超时、敏感字段绕过均有实现 | F1–F4 表明稳定性还不能按完成项处理 |

### 安装与应用接入的具体差距

1. [scripts/install.sh:19](/srv/storage/WorkSpace/oh-my-ime/scripts/install.sh:19) 复制二进制和引擎清单，未安装或启用 systemd unit，也未完成会话配置。Nix 包包含 unit，但启用和环境导入仍依赖外部配置。用户完成“安装”不等于平台已接管输入。
2. [Hyprland 指南](/srv/storage/WorkSpace/oh-my-ime/docs/zh-CN/hyprland.md:39) 要求处理 `GTK_IM_MODULE`、`QT_IM_MODULE`、`QT_IM_MODULES`、`XMODIFIERS`，并安装对应 GTK/Qt 模块。部分应用仍需包装脚本，旧版 Chromium/Electron 可能需要启动参数。这些工作应尽量收进平台安装/设置流程。
3. [wayland/mod.rs:550](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/wayland/mod.rs:550) 在缺少必要协议时直接报错；即使 XIM/Fcitx 前端存在，也不能使当前 daemon 独立运行在纯 X11 或缺少这些协议的桌面中。
4. 托管 IBus 是引擎端兼容；它不等于实现面向应用的 IBus IM-module 前端。`GTK_IM_MODULE=ibus` / `QT_IM_MODULE=ibus` 仍是缺口，代码中的 doctor 也明确提示这一点。
5. Wayland 应用通过 Fcitx 模块输入时，候选窗定位依赖 Hyprland/sway 私有 IPC，而非通用 Wayland 能力。[compositor_ipc.rs](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/compositor_ipc.rs:1) 已承认此边界。
6. 候选窗的 `xdg_popup` 子菜单/tooltip 被直接关闭，设置窗口需要另连真实桌面。支持任意工具包创建基本候选窗，尚不意味着工具包的全部窗口行为都透明可用；见 [bridge/mod.rs:1807](/srv/storage/WorkSpace/oh-my-ime/crates/omimed/src/wayland/bridge/mod.rs:1807)。

### 文档与能力声明需要更精确

- README 的“多 seat 独立会话”和“引擎故障不影响打字”，应反映上述复现结果。
- 应用兼容表应注明版本、Wayland/XWayland 模式、所需模块/参数、实际验收日期；协议具备接入条件与某个应用已经验收是两种证据。
- Wayland/Fcitx 明确标记的密码、PIN、敏感字段有绕过逻辑，也有部分测试；XIM 的 `Ic::info()` 使用默认 ContentType，没有密码用途信息来源。不能把敏感字段保护无条件推广到所有 XIM 应用。本次没有在真实密码管理器中验证该边界。
- 首次无历史状态启动时 current engine 为 direct；快速开始中“启动后直接输入 :rocket”的描述还缺少选择 Sparkle 的步骤。

## 建议调整的下一阶段

### 先完成稳定性

将 F1–F4 及对应复现纳入正式回归测试并修复，补充失焦、切换、延迟回复、异常重启之间的组合场景。约束应是：每个按键恰好被消费或交还一次；旧会话不能改变新会话；一个引擎持续故障时健康引擎与管理接口仍可用。多 seat 若暂不做完整，应明确单 seat 支持范围。

### 再完成一条可交付的用户流程

以 Hyprland 为首个明确支持的桌面，提供安装/首次设置流程：检查依赖与协议、提示并处理旧框架冲突、配置会话环境和自启动、选择一个真实可用的中文输入法、展示可直接测试输入的入口，并能撤销接管。验收使用干净用户环境，从桌面启动应用，不能借助 `nix develop` 的库路径和临时 shell 变量。

兼容 Fcitx5/IBus 可以继续作为可用中文输入来源；下一阶段应以这条用户流程的成功率和稳定性决定工作顺序。逐插件宿主、更多切换 UI 或更复杂动画可以在明确使用需求后推进。

### 用真实应用验证“各种软件”

最低矩阵建议包含 GTK3/4、Qt5/6、Firefox、Chromium、一个 Electron 编辑器、一个 Wayland 终端、X11/XIM 应用，以及用户实际使用的微信/WPS。每项覆盖组合输入、候选选择、快捷键、切换/失焦、光标定位和重启；再补不同缩放、多屏和全屏窗口。当前仓库的自建客户端仍适合协议回归，应与这些验收并存。

然后按明确的目标桌面扩展 KWin/Mutter 的前端与 UI 承载方案。应把前端启动与 Wayland v2 的必需绑定拆开，保持核心和引擎协议可复用，逐个公布已验收的支持范围。

## 本次验证记录

环境：Rust/Cargo 1.99.1；Nix 开发环境中的 sway 1.12、Fcitx5 5.1.23、IBus 1.5.34。

| 检查 | 结果 |
|---|---|
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过 |
| `cargo test --workspace --locked` | 129 项通过，含 doctest；84 项 E2E 按原有标记跳过 |
| `cargo build --workspace --locked` | 通过 |
| 原有 E2E | 84 项全部验证通过 |
| 针对性复现 | 5 项均暴露上述问题，详见源码与日志摘录 |
| Nix 安装包 | 构建通过；针对安装产物运行原有 E2E，84/84 通过（omimed 不使用开发环境的 `LD_LIBRARY_PATH`） |

原有 E2E 首轮为 73 通过、11 失败；11 项都在 XWayland 启动前失败，因为执行沙箱中的 `/tmp/.X11-unix` 属主被映射为 65534，wlroots 拒绝使用。仅对受影响测试在沙箱外运行相同的私有会话测试，11 项均通过。没有将环境失败计为产品缺陷。

Nix 安装产物为 `/nix/store/cyrpmk0b99znrcbmw8drnfnmaf9nfpwp-oh-my-ime-0.3.0`。安装包的全部 E2E 沿用沙箱外的私有测试会话，一次运行 84 项全部通过。五个问题的合并复现脚本也已实际运行，得到预期的五个失败断言，并自动清理临时测试文件。

本次没有实测用户当前 Hyprland 会话、真实 GTK/Qt/浏览器/微信/WPS、GPU 渲染与分数缩放组合、Flatpak/Snap，以及其他桌面。没有运行 cargo-deny。这些结果不等同于全软件兼容认证。

复现材料保存在本目录：

- [regression-probes.rs](regression-probes.rs)：五个断言期望行为的测试。
- [reproduce.sh](reproduce.sh)：临时挂入 E2E 测试目录，结束后自动移除；会拒绝覆盖同名文件。
- [verification.txt](verification.txt)：基线测试汇总、复现失败摘录与运行版本。

运行方式：

```sh
nix develop .#e2e --command bash docs/audits/2026-11-02/reproduce.sh
```

这五个断言在本次审查基线上应失败；修复后的目标是全部通过。复现使用私有 headless sway、私有 D-Bus 和临时 XDG 目录。审查未改动生产源码或原有测试；新增内容仅为本目录的报告和复现材料。

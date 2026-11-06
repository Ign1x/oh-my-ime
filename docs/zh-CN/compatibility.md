# 桌面和真实应用验收

[English](../compatibility.md) | 简体中文

兼容性记录必须对应具体的应用版本、桌面、输入路径和场景。协议测试用于回归，
不能替代真实应用验收。首批机器可读记录保存在
[`tests/desktop/results/2026-11-06.json`](../../tests/desktop/results/2026-11-06.json)。

## 本轮实测

以下测试在 2026-11-06 的 x86_64 NixOS 上使用**已发布的 v0.4.0 平台与 Rime 二进制包**，
并未使用尚未发布的源码修改：

| 桌面 / 应用 | 结果 | 验证范围 |
|---|---|---|
| sway 1.12 / Firefox 159.0，原生 Wayland | 预编辑、空格选词、中文上屏、失焦清理通过 | 独立 Firefox 配置与本地双输入框页面；Rime `luna_pinyin` |
| sway 1.12 / foot 1.28.0，原生 Wayland | 空格选词、中文上屏通过 | 真实终端 stdin 精确收到 `中国` 和换行；Rime `luna_pinyin` |
| sway 1.12 / xterm 411，XWayland | 空格选词、中文上屏通过 | 真实终端 stdin 通过 XIM 精确收到 `中国` 和换行 |
| KWin 6.8.2，虚拟显示器 | 阻塞：当前平台要求 IM v2 | 已读取普通连接及 `--inputmethod` 特权连接的 registry；特权连接提供 `zwp_input_method_v1` 和 `zwp_input_panel_v1`，没有 IM v2 或 virtual-keyboard-v1 |
| GNOME | 阻塞：测试环境未安装 `gnome-shell` | 未运行 GNOME 输入流程 |

Firefox 测试读取真实 DOM 组合事件和文本内容：输入 `zhongguo` 后按空格得到 `中国`；
再开始组合 `ni`，让页面把焦点移到第二个输入框，输入 `hao` 后按空格只得到 `好`。
测试通过 DOM 改变焦点，因为 Tab 可能由 Rime 用于组合操作。这里不要求第一个输入框
失焦时一定提交或取消组合，不同应用的处理可以不同。

Firefox 的候选窗口位置、鼠标选词、快捷键、混合缩放和重启尚未验收。
foot/xterm 的预编辑外观与焦点行为仍未测试。GTK 3/4 应用、Qt 5/6 应用、Chromium、VS Code、微信和 WPS 都有独立的
待测行；Firefox 通过不代表其他 GTK 应用通过。既有 WPS 问题仍记录在
[应用指南](hyprland.md)中。

## 运行隔离探针

先构建工作区，得到虚拟键盘测试工具。桌面探针只依赖 Python 标准库，不需要 `wayland-info`：

```sh
nix develop .#e2e --command cargo build --workspace --locked
nix develop .#e2e --command python3 scripts/desktop-probe.py \
  --desktop sway --test-firefox --out /tmp/omime-sway-check \
  --bin-dir /path/to/installed/bin --require-compatible
```

Firefox 需要已在 `PATH` 上，或用 `--firefox` 指定。省略 `--bin-dir` 会检查 `target/debug`；
测试安装产物时，该目录需要包含 Rime。`--tools-dir` 可指定包含 `omime-keyboard` 的其他构建目录。
增加 `--test-foot` 可检查真实 foot 终端；不在 PATH 上时用 `--foot /path/to/foot` 指定。
`--test-xterm` 会另外启动私有 XWayland，检查 XIM；用 `--xterm /path/to/xterm` 指定程序。
三种应用可以在同一个私有会话中检查。XIM 测试通过 `omime-keyboard` 注入正常 US 键盘映射，
因为 wtype 的合成键盘映射不适合 XIM 的 keycode 查询。

KWin 和 GNOME 分别运行能力及启动探针：

```sh
python3 scripts/desktop-probe.py --desktop kwin \
  --out /tmp/omime-kwin-check --bin-dir /path/to/installed/bin
python3 scripts/desktop-probe.py --desktop gnome \
  --out /tmp/omime-gnome-check --bin-dir /path/to/installed/bin
```

`--compositor /path/to/executable` 可固定桌面版本。GNOME 需要支持 `--headless` 和
`--wayland-display` 的构建。缺少依赖会记录为 `blocked`，脚本不会下载桌面或安装系统包。

每轮必须使用**新的输出目录**。脚本创建独立 XDG 目录、不提供服务自动激活的私有会话总线、
无头桌面和明确指定的 Firefox 配置目录，不运行 setup、不向用户管理器导入环境、
不重启当前会话服务。清理时只结束自身启动的进程组。日志、合成文本的浏览器事件与
`report.json` 留在输出目录中。报告记录仓库 revision、dirty 状态、探针脚本 SHA-256，
以及 daemon/Rime 二进制 SHA-256，避免相同版本号混淆不同开发构建。
读取 registry 不会绑定输入法；KWin 辅助程序会使用
`--inputmethod` 进程收到的特权 socket。

正常退出表示报告已生成。加上 `--require-compatible` 后，已执行检查中出现阻塞或失败
会返回 **3**；未测试的场景仍保持未测试。探针自测覆盖消息分段、连接断开、会话环境隔离、
缺少桌面、保留既有输出、二进制 fingerprint，以及启动失败后仍写入报告：

```sh
python3 -m unittest discover -s tests/desktop -p 'test_*.py'
```

## 补齐真实应用记录

保留生成的 JSON 作为证据。手工验收时复制一份，填写应用版本、安装来源、桌面版本、
Wayland/XWayland 模式、输入路径、启动选项、引擎/方案、日期及每个场景的证据。
截图和日志只使用合成测试文本。

| 场景 | 验收内容 |
|---|---|
| 预编辑 | 输入并修改未提交的短语；光标和选区正确 |
| 候选选择 | 键盘和鼠标选词、翻页；所选候选只插入一次 |
| 上屏 | 中英文与标点混输、文本中间插入；顺序与删除正确 |
| 失焦清理 | 组合时离开或关闭输入框，再打开其他输入框；文本和候选窗不串到新输入框 |
| 快捷键 | 复制粘贴、导航、切换输入法、松开 Shift/Ctrl；没有卡住的修饰键 |
| 候选位置 | 移动光标与窗口；覆盖屏幕下/右边缘和全屏 |
| 混合缩放 | 在 100%、125%、150% 显示器之间移动；位置、清晰度、点击正确 |
| 重启 | 分别重启引擎和平台；输入与选择可以恢复 |

`passed` 表示实际观察通过；`failed` 表示场景已运行但结果不正确；`blocked` 表示前置条件
阻止运行；`not-tested` 表示未执行。各适用场景有证据后，才能把桌面/应用标为完整验收。
无头测试无法证明真实 GPU、物理显示器或桌面启动器的行为。

## 探针确定的后端工作

KDE 需要在 KWin 传入的输入法连接上实现 IM v1 前端和 input-panel-v1 窗口承载。
文本协议和候选窗口角色均不同于现有 v2 实现；守护进程也需要能够在不要求 v2 前端的情况下
初始化核心。仅把现有引擎登记成 KWin 输入法还不能工作。候选 surface 与输入法仍须共用连接。

GNOME 首先采用 oh-my-ime 作为**系统 IBus** 中的一个引擎，需要解开守护进程生命周期与 v2
前端的耦合，增加 IBus 引擎适配，并让 Rime 可选地输出候选语义，供 GNOME 系统候选 UI 使用。
现有 `omime-ibus` 在平台内托管私有 IBus，方向相反，不能充当 GNOME 接入。
运行时验证的下一前置条件是提供支持探针所需无头参数的 GNOME 环境。
KDE 和 GNOME 目前均未达到输入验收状态。

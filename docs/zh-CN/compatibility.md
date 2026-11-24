# 桌面和真实应用验收

[English](../compatibility.md) | 简体中文

兼容性记录必须对应具体的应用版本、桌面、输入路径和场景。协议测试用于回归，
不能替代真实应用验收。首批机器可读记录保存在
[`tests/desktop/results/2026-11-06.json`](../../tests/desktop/results/2026-11-06.json)。

## 本轮实测

兼容性必须对应具体的应用版本、桌面、输入路径和场景。以下记录于 2026-11-24，使用当前源码构建和独立会话： 机器可读的原生基线见 [`tests/desktop/results/2026-11-24-native.json`](../../tests/desktop/results/2026-11-24-native.json)。

| 桌面 / 应用 | 结果 | 验证范围 |
|---|---|---|
| sway 1.12 / Firefox 159.0，原生 Wayland | 预编辑、候选、中文上屏和失焦清理通过 | 独立 Firefox 配置与双输入框页面；Rime `luna_pinyin` |
| sway 1.12 / Firefox 160.0，原生 Wayland | 预编辑、候选选择、中文上屏和失焦清理通过 | 新建 Firefox 配置；Rime `luna_pinyin`；`/tmp/omime-p0-sway-firefox-fixed/report.json` |
| sway 1.12 / foot 1.28.0，原生 Wayland | 候选和中文上屏通过 | 真实终端 stdin；Rime `luna_pinyin` |
| sway 1.12 / xterm 411，XWayland | 候选和中文上屏通过 | 真实终端 stdin，经 XIM |
| KWin 6.8.3，虚拟显示器 | IM v1 启动、Echo 组合、Rime 中国、候选角色、密码绕过、焦点恢复通过 | `/tmp/omime-native/kwin-run-8/report.json`，6 项检查 |
| GNOME Shell 50.4，虚拟显示器 | 系统 IBus 选择、真实 GTK3/GTK4 `Gtk.Entry` 与 Qt6 Quick `TextField` 上屏、行内预编辑、Rime 上屏、候选、失焦清理、密码绕过通过 | `/tmp/omime-gnome-toolkits-final2/report.json`，11 项检查 |
| IBus 1.5.34，无头原生引擎 | Rime 6 项、Echo 5 项通过，含翻页/点击与提交缓存清理 | `/tmp/omime-ibus-rime-b5/result.json`、`/tmp/omime-ibus-echo-b5/result.json` |

KWin 使用合成原生文本框，GNOME 另外启动真实 GTK3/GTK4 `Gtk.Entry` 和 Qt6 Quick `TextField`；两者都使用私有总线，证明适配器与桌面生命周期，
不代表所有应用。实际显示器、混合 DPI、全屏、启动器、鼠标定位，以及浏览器/Electron/办公软件仍需
分别验收。KWin 虚拟渲染器截图不稳定；GNOME 候选截图已成功保存。

2026-11-24 的 P0 隔离复验中，GNOME 工具包完整路径和 Firefox 160 场景全部通过。
KWin 通过启动、Echo 组合和 IM v1 候选 panel 角色检查，但 KWin 6.8.3 在虚拟截图请求时崩溃，
后续截图和 Rime 上屏被该虚拟渲染器阻塞。证据保留在 `/tmp/omime-p0-kwin-1/report.json` 和
`/tmp/omime-p0-gnome-toolkits/report.json`。

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

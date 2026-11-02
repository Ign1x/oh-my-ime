# 实现审查修复记录

修复日期：2026-11-03。基线：`d9899fcfb3d385ce2b77048bc3b28df78e93c346`。
原始发现和失败记录保留在 [implementation-review.zh-CN.md](implementation-review.zh-CN.md) 与 [verification.txt](verification.txt)。

## 五项复现对应的修复

| 编号 | 修复 | 正式回归 |
|---|---|---|
| F1 | 原生按键回送使用事件发生时的修饰键，随后恢复当前物理状态；托管输入法的虚拟键盘回送也保留 modifiers/time，送往 Wayland、Fcitx 与 XIM | 延迟 `Shift+A` 得到 `A`，之后普通 `z` 得到 `Az`；托管引擎延迟 `Shift+1` 得到 `!`，之后普通 `1` 得到 `!1` |
| F2 | 切换、失焦或旧进程退出后被放弃的回复只丢弃，不重置当前引擎；超时请求保留恢复状态，后续按键先直接放行，旧请求结束后通过正常队列完成 Reset，再接受新组合输入 | 旧引擎回复后保留 `aBC `；暂停 Echo 触发超时、恢复后得到 `abcDE `，没有混入旧组合内容 |
| F3 | Deactivated 同时结算正在处理与排队的按键；输入框已销毁时仍完成原 RPC，忽略旧文本动作 | FocusOut、DestroyIC、变成密码字段，分别覆盖 ProcessKeyEvent 与 ProcessKeyEventBatch |
| F4 | 非法 viewport 参数、重复 viewport、非法 buffer 缩放和变换在桥接层拒绝；清理时跳过等待 delete_id 的已销毁 shm pool；有错误对象归属时在重连前停用引擎，未读到错误详情时让引擎崩溃上限先于全局断线上限生效 | 持续非法 viewport 不断开 daemon 的上游连接；额外的非法 subsurface 场景中 daemon 保持可用，仍能切换到健康 Echo 输入 |
| F5 | Echo 按 ContextId 保存单词；Sparkle 按 ContextId 保存查询、候选与光标状态，失焦/重置只清理对应输入框；SDK 与双语教程同步示范 | 两个 seat 同时组合，分别得到 `AB ` / `CD `；Sparkle 分别得到 `🚀` / `❤️` |

用例位于 [regressions.rs](../../../tests/e2e/tests/regressions.rs) 和 [robustness.rs](../../../tests/e2e/tests/robustness.rs)。它们使用原有 E2E 的 ignored 标记，CI 的 `--ignored` 会执行这些测试。

引擎进入停止过程后不再接收新请求；旧进程代次的退出通知也不能清空新进程的状态。

## 安装与文档

- 源码安装附带 systemd 用户单元、配置示例和两种 Hyprland 会话模板。单元与自带引擎使用安装前缀内的绝对路径，避免依赖用户管理器是否将 `~/.local/bin` 放入 PATH。
- `OMIME_INSTALL_BIN_DIR` 支持安装预构建二进制，便于打包与临时前缀验证。
- 快速开始增加 `omime doctor` 和选择 Sparkle 的步骤；首次启动默认直接输入。
- 修正多 seat、敏感字段和应用兼容性的声明：XIM 不报告字段用途，协议接入条件不等于真实应用已验收。
- `pulldown-cmark` 从 0.13.5 更新到 0.13.6，修复依赖检查发现的 RUSTSEC-2026-0433；仓库的 cargo-deny 策略未放宽。

## 验证

最终检查结果见 [verification-fixes.txt](verification-fixes.txt)。

| 检查 | 结果 |
|---|---|
| 格式检查、Clippy（warnings 视为错误） | 通过 |
| 单元与文档测试 | 129 通过 |
| 开发构建完整 E2E | 96 通过 |
| Nix 发布包构建 | 通过 |
| Nix 安装产物完整 E2E | 96 通过 |
| cargo-deny 安全公告、依赖限制、许可证、来源检查 | 通过 |
| 源码安装脚本语法、ShellCheck、临时前缀安装与 systemd 单元校验 | 通过 |

安装产物：`/nix/store/d6pqcabzz50w39iy3l5ji5vzpzmsm9b1-oh-my-ime-0.3.0`。

标准验证命令：

```sh
cargo fmt --all --check
nix develop .#e2e --command cargo clippy --workspace --all-targets --locked -- -D warnings
nix develop .#e2e --command cargo test --workspace --locked
nix develop .#e2e --command cargo build --workspace --locked
nix develop .#e2e --command cargo test -p omime-e2e --locked --no-fail-fast -- --ignored
cargo deny --locked check
```

本环境的外层沙箱将 `/tmp/.X11-unix` 属主映射为 65534，导致 XWayland 拒绝启动。最终 X11 验证在同一沙箱内额外使用 bubblewrap 创建独立 `/tmp`，根文件系统只读、Cargo target 目录可写，测试仍使用私有 headless sway、D-Bus 和 XDG 目录。无需修改宿主的 X11 socket 目录。

## 当前支持边界

这些修复解决原审查的五个具体复现。多 seat 的组合文本已分离，但各 seat 独立选择输入法、候选窗与上下文绑定仍未完成，README 已标为进行中，正式 UI 使用流程仍为单 seat。

共享上游 Wayland 连接仍是故障边界；无法提前校验的请求可能造成重连，错误详情先于断线到达时才能立即归因。测试覆盖了错误详情可用和仅观察到断线的恢复途径。

源码安装现已提供所需文件，但旧框架接管、模块安装与桌面环境配置仍按指南进行。GTK/Qt、浏览器、Electron、微信/WPS 的完整版本矩阵，以及 KWin/Mutter 支持，仍是后续产品工作。

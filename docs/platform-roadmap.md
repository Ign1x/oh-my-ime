# Toward a mature Linux input platform

English | [简体中文](zh-CN/platform-roadmap.md)

Baseline: source version 0.5.0, assessed on 2026-11-24. This roadmap separates the
implemented native adapters from remaining acceptance work. Observed application results belong
in [desktop and application acceptance](compatibility.md).

Windows TSF and macOS InputMethodKit provide relatively consistent system entry
points. Linux requires separate routes for compositors, toolkits and display
protocols. oh-my-ime already has separate engine processes, per-application input
preferences, engine recovery, a UI bridge, settings, a tray, packages and regression
tests. The largest gaps are desktop coverage, application acceptance and complete
user journeys from installation through recovery.

## Capability gaps

| Capability | Experience to deliver | Current foundation | Remaining work |
|---|---|---|---|
| Desktop integration | Discover, enable and use an input method after login | IM v2 on Hyprland/sway; native KDE IM v1 and GNOME system IBus; XWayland routes | Physical desktop/app acceptance; pure X11 |
| Applications | Consistent composition in browsers, terminals, editors and office software | Synthetic tests and limited Firefox, foot and xterm evidence on sway | Separate GTK 3/4, Qt 5/6, Chromium/Electron, VS Code, WPS and WeChat runs, including sandbox packages and launchers |
| Candidate windows | Follow the cursor, stay visible at edges, scale and click correctly | Independent engine UIs, Wayland UI bridge and X11 cursor mirroring | Mixed DPI, fullscreen, edges and physical-display acceptance for each backend |
| Text editing | Correct commits, selection replacement, deletion and focus changes | Ordered Commit, Preedit and DeleteSurrounding actions; bounded output queues | Adapter-specific byte/character boundaries, serials and context lifetime; reconversion design |
| Recovery | Continue typing after failures, with actionable explanations | Key timeouts, restarts, isolation, diagnostics and isolated P1 recovery/resource checks | Desktop reconnection, suspend/resume, long-run hardware acceptance and graphical recovery actions |
| First use | Choose a language, enable an engine, try typing and finish | CLI setup, language suggestions, input-method ordering and a real settings-page typing test | Desktop-aware graphical onboarding and understandable application repair |
| Interface | Clear navigation, readable status, coherent themes, keyboard access | Native Slint settings/switcher, first-run typing card and Chinese/English localization | Consistent components, compact layouts, long labels, focus and screen-reader acceptance |
| Ecosystem | Stable installation, upgrade and integration contracts | Manifests, Rust SDK, Python/GTK example and hosted Fcitx5/IBus | Protocol 1.3 semantic candidate adoption by more engines; SDK examples, provenance and versions |
| Performance | Predictable latency and stable resources | Timeouts, queue bounds, resource regressions and isolated P1 latency/memory checks | Per-route p50/p95/p99 latency on hardware, suspend/resume and long-session tests |

Prediction, dictionaries, voice and handwriting can be provided by engines. The
platform owns connection, focus, delivery, presentation, recovery and management;
improving those foundations enables more engines without prescribing their UI.

## Architectural boundaries

The daemon shares core engine supervision, settings and control services across two
frontend families. Wayland automatically selects IM v2 + virtual keyboard or KWin's
privileged IM v1 + input-panel-v1. GNOME `--ibus` initializes without a Wayland input
connection and registers with the existing system IBus.

KDE stages surrounding text/content state until commit_state, preserves original
keyboard serials, and gives engine surfaces their panel role on the same privileged
connection. GNOME converts UTF-8 byte boundaries to IBus character offsets, bounds
incoming/outgoing work and invalidates output on focus loss. IBus private/hidden
hints and password/PIN purposes bypass engines.

Engine protocol 1.3 provides optional semantic candidates and click/page callbacks.
Rime's IBus presentation runs headless; Shell owns its candidate window. Engine-drawn
Wayland UI remains available on Wayland backends. Hosted framework mode and GNOME's
system IBus engine mode are distinct routes. See [native setup](native-desktops.md).

## Current implementation batch

1. Add automatic desktop detection and explicit Hyprland/Sway targeting to setup.
   Manage reversible Sway configuration, startup and launch environment; preserve
   old Hyprland journals and avoid silently configuring the wrong desktop.
2. Share compositor capability classification between daemon startup and CLI
   diagnostics, with concrete explanations of missing support.
3. Redesign native Slint navigation, typography, cards, light/dark colors and status
   presentation while retaining the existing controls.
4. Exercise Sway preview, configuration parsing, idempotency, undo and first typing
   in private sessions; retain Hyprland and input-behavior regressions.

5. Implement KDE IM v1/panel and GNOME system IBus adapters, protocol 1.3 candidates,
   headless Rime presentation, reversible native setup and Nix backend selection.
6. Validate real isolated KWin, IBus and Shell workflows. Keep protocol correctness,
   synthetic input acceptance and physical desktop acceptance separately observable.

Sway directly launched applications must inherit the input environment: UWSM can
source `env-sway`, while other launchers need exports before Sway starts. KDE/GNOME
native setup retains the desktop's lifecycle and toolkit environment.

## Subsequent milestones

### Native desktop and application acceptance

Complete candidate placement/clicking at screen edges and mixed scales, restart and
suspend/resume tests, then run GTK 3/4, Qt 5/6, browser/Electron and office applications
on each desktop. Expand native GNOME presentation beyond Rime/Echo. Establish the
limits of application identity and per-app state for mediated IBus contexts and v1.

### Daily use and ecosystem

Complete graphical onboarding and recovery actions. Add pure X11 and validate
more IM v2 compositors. Establish mixed-scale, accessibility, suspend/resume and
resource baselines. Publish adapter/engine capability contracts so the UI can explain
unsupported features precisely.

## Acceptance for each input route

| Scenario | Observable result |
|---|---|
| Composition | Edit preedit, page/select candidates, commit once and in order, including mixed scripts |
| Focus | Switching fields/apps and closing windows never leaks stale text or popups |
| Keys | Unhandled keys, shortcuts and releases reach the app without stuck modifiers |
| Text operations | Insertion, selection replacement and surrounding deletion preserve Unicode boundaries |
| Candidate window | Cursor tracking, edges, movement, scale and pointer hit testing work |
| Sensitive fields | Reported password/PIN/sensitive fields bypass engines; missing metadata is recorded |
| Recovery | Restart engine and daemon separately; ordinary typing remains available and composition recovers |
| Installation | Fresh login can activate input; undo restores prior setup; terminal and launcher apps agree |

Reports use `passed`, `failed`, `blocked` and `not-tested`, recording application and
desktop versions, input route, engine/schema and evidence. Protocol regressions do
not accept every application; headless tests do not establish physical-display or
launcher behavior.

## Validation history

On 2026-11-24, the native batch passed strict Rust formatting, Clippy with warnings denied,
workspace build and workspace tests (including 167 omimed tests), translation coverage,
42 packaging/script tests, 8 desktop-probe tests and cargo-deny advisory/license/source
checks. The isolated native probes passed KWin 6.8.3 (six checks), GNOME Shell 50.4
eight checks, IBus/Rime six checks and IBus/Echo five checks. Evidence paths and limits
are recorded in [desktop and application acceptance](compatibility.md).

The opt-in legacy E2E suite passed its setup, Sway, management, tray and Wayland groups.
Its XIM/Fcitx5 groups were blocked in this runner because the sandbox's `/tmp/.X11-unix`
was not root-owned and XWayland could not start; this is an environment prerequisite,
not a native KDE/GNOME failure. Re-run those groups in the CI desktop environment.

The settings redesign was generated with AGY `gemini-4-hydrogen-high`, reviewed
and corrected in Slint, then captured using private sway/D-Bus sessions with real
omimed/Rime. See [light](images/settings-light.png), [dark](images/settings-dark.png)
and [compact](images/settings-compact.png). These UI checks do not establish physical-display
or launcher acceptance.

The P1 recovery and resource baseline on 2026-11-24 passed isolated core teardown,
update-flood responsiveness, Fcitx/SDK output bounds, timeout recovery, engine restart,
daemon/Rime restart recovery and repeated focus entry. The machine-readable evidence is
[`tests/desktop/results/2026-11-24-p1.json`](../tests/desktop/results/2026-11-24-p1.json).
Physical suspend/resume, real multi-monitor or mixed-DPI hardware, and long-run hardware
p99 latency remain untested.

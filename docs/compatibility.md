# Desktop and application acceptance

English | [简体中文](zh-CN/compatibility.md)

Compatibility is recorded for a particular application build, desktop, input route and
scenario. Protocol tests remain useful regressions, but do not accept an application.
The initial machine-readable baseline is
[`tests/desktop/results/2026-11-06.json`](../tests/desktop/results/2026-11-06.json).

## Current evidence

The following probes used the **published v0.4.0 platform and Rime binaries**, not the
unreleased source changes, on x86_64 NixOS on 2026-11-06:

| Desktop / application | Result | Scope |
|---|---|---|
| sway 1.12 / Firefox 159.0, native Wayland | Passed: preedit, Space candidate selection, Chinese commit, focus cleanup | Private Firefox profile and local two-field HTML fixture; Rime `luna_pinyin` |
| sway 1.12 / foot 1.28.0, native Wayland | Passed: Space candidate selection, Chinese commit | Real terminal stdin receives exactly `中国` and newline; Rime `luna_pinyin` |
| sway 1.12 / xterm 411, XWayland | Passed: Space candidate selection, Chinese commit | Real terminal stdin receives exactly `中国` and newline through XIM |
| KWin 6.8.2, virtual output | Blocked: current platform requires IM v2 | Public registry and privileged `--inputmethod` connection both probed; privileged connection exposes `zwp_input_method_v1` and `zwp_input_panel_v1`, with no IM v2 or virtual-keyboard-v1 |
| GNOME | Blocked: `gnome-shell` is unavailable in the test environment | No GNOME input workflow was run |

The Firefox test observes actual DOM composition events and text values: typing
`zhongguo` then Space yields `中国`; after starting `ni`, the page focuses the second
field, where `hao` then Space yields only `好`. Focus changes through the DOM because
Tab may belong to Rime's composition controls. This does not assert whether the first
field commits or cancels its composition when losing focus.

Firefox's popup geometry, pointer selection, shortcuts, mixed DPI and restart have
not been accepted. foot/xterm preedit appearance and focus behavior remain untested.
GTK 3/4 applications, Qt 5/6 applications, Chromium, VS Code, WeChat and WPS remain separate rows requiring their own runs. The Firefox row
does not accept other GTK applications. Existing WPS findings remain documented in
[the application guide](hyprland.md#application-routes-and-verification).

## Development build regression

The M1 development binaries also passed the eight input scenarios above across
Firefox, foot and xterm. The [separate record](../tests/desktop/results/2026-11-06-development.json)
identifies the binaries by SHA-256. The first run could not find foot/xterm on PATH;
the second used explicit installed paths and passed their checks. This does not
extend acceptance beyond the scenarios listed above.

## Run a private probe

Build the workspace for the virtual keyboard test tool. The desktop probe itself uses
Python's standard library and does not require `wayland-info`:

```sh
nix develop .#e2e --command cargo build --workspace --locked
nix develop .#e2e --command python3 scripts/desktop-probe.py \
  --desktop sway --test-firefox --out /tmp/omime-sway-check \
  --bin-dir /path/to/installed/bin --require-compatible
```

Firefox must already be available on `PATH`, or be specified with `--firefox`. Omit
`--bin-dir` to test `target/debug`; an installed platform must include the Rime engine.
`--tools-dir` selects another build directory containing `omime-keyboard`.
Add `--test-foot` to exercise a real foot terminal; use `--foot /path/to/foot`
when it is not on `PATH`. `--test-xterm` also starts a private XWayland server and
checks XIM; `--xterm /path/to/xterm` selects its binary. All three applications may be
checked in the same private session. The XIM test injects a normal US keymap through
`omime-keyboard`, because wtype's synthetic keymap is unsuitable for XIM keycode lookup.

KWin and GNOME have separate capability/startup probes:

```sh
python3 scripts/desktop-probe.py --desktop kwin \
  --out /tmp/omime-kwin-check --bin-dir /path/to/installed/bin
python3 scripts/desktop-probe.py --desktop gnome \
  --out /tmp/omime-gnome-check --bin-dir /path/to/installed/bin
```

Use `--compositor /path/to/executable` for a particular version. GNOME needs a build
with `--headless` and `--wayland-display`. Missing dependencies are recorded as
`blocked`. The script does not download a desktop or install packages.

Every run requires a **new output directory**. It creates private XDG directories,
a session bus without service activation, a headless compositor, and an explicit
Firefox profile. It never runs `setup`, imports environment into the user manager,
or restarts session services. Cleanup targets only the process groups it launched.
Logs, the synthetic browser events, and `report.json` remain in the output directory.
Reports record the repository revision and dirty state, probe script SHA-256, and
daemon/Rime binary SHA-256, so development builds sharing a version number remain distinguishable.
The registry is inspected without binding an input method; KWin's separate helper
uses the privileged socket passed to its `--inputmethod` process.

A normal exit means the report was generated. `--require-compatible` exits **3** if
any executed check is blocked or failed; untested scenarios remain untested. The
probe's own tests cover partial protocol messages, disconnects, private environment
isolation, missing desktops, preserving previous output, binary fingerprints, and
writing the report after startup failure:

```sh
python3 -m unittest discover -s tests/desktop -p 'test_*.py'
```

## Complete a real-application row

Keep the generated JSON as evidence. For manual acceptance, make a separate copy and
fill in the application version, installation source, desktop version, display mode,
input route, launch options, engine/schema, date, and evidence for each scenario.
Use only synthetic text in screenshots and logs.

| Scenario | What to check |
|---|---|
| Preedit | Type and edit an uncommitted phrase; cursor and selection stay correct |
| Candidate selection | Select with keyboard and pointer; change pages; the chosen candidate is inserted once |
| Commit | Mix Chinese, Latin text and punctuation; insert in the middle of text; verify order and deletion |
| Focus cleanup | Leave a composing field, close it, and open another; no text or popup leaks to the next field |
| Shortcuts | Copy/paste, navigation, input-method switching, Shift/Ctrl release; no stuck modifiers |
| Popup position | Move the cursor/window; test bottom/right edges and fullscreen |
| Mixed scale | Move between 100%, 125% and 150% outputs and verify position, sharpness and clicks |
| Restart | Restart the engine and daemon separately; typing and selection recover |

`passed` requires observed results; `failed` means the scenario ran and produced an
incorrect result; `blocked` means a prerequisite prevented it; `not-tested` means it
was not run. A desktop/application is not fully accepted until its applicable scenarios
have evidence. Headless runs do not establish GPU, physical-display or launcher behavior.

## Backend work established by the probes

KDE needs an IM v1 frontend and an input-panel-v1 presentation path on the connection
KWin passes to its input method. Both the text protocol and popup role differ from the
current v2 implementation. The daemon must be able to initialize its core without
requiring the v2 frontend; simply advertising an engine as a KWin input method does
not work. Preserve the same-connection rule for popup surfaces.

GNOME's first route is an oh-my-ime engine connected to the **system IBus** instance.
It needs the daemon lifecycle decoupling, an IBus engine adapter, and optional semantic
candidate output from Rime for GNOME's candidate UI. The existing `omime-ibus` process
hosts a private IBus framework inside the platform; it implements the opposite direction
and does not provide GNOME integration. A desktop installed with the probe's required
headless flags is the next prerequisite for runtime validation. Neither KDE nor GNOME
is currently accepted for input, and these probes do not claim otherwise.

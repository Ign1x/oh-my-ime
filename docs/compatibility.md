# Desktop and application acceptance

English | [简体中文](zh-CN/compatibility.md)

Compatibility is recorded for a particular application build, desktop, input route and
scenario. Protocol tests remain useful regressions, but do not accept an application.
The initial machine-readable baseline is
[`tests/desktop/results/2026-11-06.json`](../tests/desktop/results/2026-11-06.json).

## Current evidence

Compatibility is recorded for a particular application build, desktop, input route and scenario.
The following rows were run in isolated sessions on 2026-11-24 with the current source build: The machine-readable native baseline is [`tests/desktop/results/2026-11-24-native.json`](../tests/desktop/results/2026-11-24-native.json).

| Desktop / application | Result | Scope |
|---|---|---|
| sway 1.12 / Firefox 159.0, native Wayland | Passed: preedit, candidate selection, Chinese commit and focus cleanup | Private Firefox profile and local two-field fixture; Rime `luna_pinyin` |
| sway 1.12 / Firefox 160.0, native Wayland | Passed: preedit, candidate selection, Chinese commit and focus cleanup | Fresh private Firefox profile; Rime `luna_pinyin`; `/tmp/omime-p0-sway-firefox-fixed/report.json` |
| sway 1.12 / foot 1.28.0, native Wayland | Passed: candidate selection and Chinese commit | Real terminal stdin; Rime `luna_pinyin` |
| sway 1.12 / xterm 411, XWayland | Passed: candidate selection and Chinese commit | Real terminal stdin through XIM |
| KWin 6.8.3, virtual output | Passed: IM v1 startup, Echo composition, Rime 中国, panel role, password bypass and focus recovery | `/tmp/omime-native/kwin-run-8/report.json`; six checks, virtual output |
| GNOME Shell 50.4, virtual output | Passed: system IBus selection, GTK3/GTK4 `Gtk.Entry` and Qt6 Quick `TextField` commits, inline preedit, Rime commit, candidate delivery/screenshot, focus cleanup and password bypass | `/tmp/omime-gnome-toolkits-final2/report.json`; eleven checks |
| IBus 1.5.34, headless native engine | Passed: Rime six checks and Echo five checks, including paging/clicking and commit-only cache clearing | `/tmp/omime-ibus-rime-b5/result.json`, `/tmp/omime-ibus-echo-b5/result.json` |

The KWin row uses a synthetic native text field; the GNOME row combines that field with real GTK3/GTK4
`Gtk.Entry` and Qt6 Quick `TextField` clients. Both use private buses. They establish the adapter and
desktop-owned lifecycle, not every application. Physical displays, mixed DPI, fullscreen, launchers,
pointer placement and browser/Electron/office applications remain separate acceptance rows. KWin
virtual-renderer screenshot capture is unstable; the GNOME candidate screenshot was captured successfully.

### P1 recovery and resource baseline

The isolated P1 run on 2026-11-24 passed the following non-application checks:

| Area | Passed checks | Evidence |
|---|---|---|
| Core recovery and resource bounds | 4,096 context teardowns without RSS growth; update-flood key latency 83–166 ms; input remained responsive | `release_core_bounds` ignored E2E suite |
| Fcitx and SDK output bounds | 8,192 preedit flood increased RSS by 132 KiB; SDK batch commit left the next key responsive; 256 FocusIn calls had no compositor query or FD leak | `release_fcitx_bounds` ignored E2E suite |
| Engine and daemon restart recovery | Timeout recovery resumed composition; crashed engine restarted; Rime restart restored application input mode | `tests/e2e/tests/regressions.rs`, `typing.rs`, `management.rs` |

The machine-readable record is [`tests/desktop/results/2026-11-24-p1.json`](../tests/desktop/results/2026-11-24-p1.json).
Physical suspend/resume, real multi-monitor or mixed-DPI hardware, and long-run hardware p99 latency were not tested.

The 2026-11-24 P0 isolated follow-up passed the complete GNOME toolkit run and Firefox 160 scenarios.
KWin passed startup, Echo composition and the IM v1 candidate-panel role, then KWin 6.8.3 crashed while
handling the virtual screenshot request. Later KWin screenshot and Rime commit checks are blocked by
that virtual renderer. Evidence remains at `/tmp/omime-p0-kwin-1/report.json` and
`/tmp/omime-p0-gnome-toolkits/report.json`.

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

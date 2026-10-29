# Sparkle ✨

An oh-my-ime engine that draws its own UI. Type `:` followed by a few letters of an
emoji's shortcode and pick from an animated deck that appears next to the text cursor.

![Sparkle in foot on Hyprland](../../docs/images/sparkle-in-foot.png)

| Key | Action |
|---|---|
| `:` then letters | start a query, e.g. `:rocket`, `:heart`, `:thumbs` |
| `Tab` / `→`, `Shift+Tab` / `←` | move the selection |
| `Space` / `Return` / click | insert the selected emoji |
| `Backspace` | edit the query; removing the colon ends it |
| `Esc` or any other key | keep what you typed as plain text |

Everything else you type passes through untouched.

## What it demonstrates

- **The UI is the engine's own.** The deck is an ordinary frameless
  [Slint](https://slint.dev) window with gradients, rounded corners, a drop shadow,
  animations, hover effects and a pointer cursor. oh-my-ime's UI bridge turns it into an
  input-method popup; Slint needs no special code.
- **Cursor awareness.** The platform reports where the text cursor is relative to the
  window, and the arrow follows it (and flips when the popup sits above the text).
- **Mouse input.** Clicks reach the engine, which commits through the SDK's `Host`.
- **Visibility is the platform's job.** The engine calls `set_popup_visible` instead of
  hiding its window.
- **Threading.** The engine logic runs on the SDK thread; the UI owns the main thread.

## Rendering

Slint renders with OpenGL (femtovg) when it can, using GPU buffers through the bridge,
and falls back to its software renderer otherwise. To force the software renderer, set
`SLINT_BACKEND=winit-software` in omimed's environment.

Emoji glyphs use the "Noto Color Emoji" font when it is installed.

## License

Sparkle's own code is MIT OR Apache-2.0 like the rest of oh-my-ime. It uses Slint under
the [Slint Royalty-free License](https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md);
oh-my-ime's platform crates do not depend on Slint.

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-whitebg.png)](https://slint.dev)

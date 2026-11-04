# Rime

oh-my-ime's Rime engine: [Rime](https://rime.im) (librime) does the input, with its
schemas as input methods of their own, and the engine shows the candidates in a window
of its own at the text cursor.

- **Xiaohe shuangpin first.** The first time, the engine puts Xiaohe shuangpin
  (`double_pinyin_flypy`, 小鹤双拼) and pinyin (`luna_pinyin`) in Rime's schema list,
  with simplified characters, and makes Shift switch between Chinese and English
  letters. Each schema is an input method: `omime switch rime/luna_pinyin`, the tray
  menu, `Ctrl+Shift+Space`.
- **The candidate window** follows the text cursor, numbered as Rime numbers them
  (`1`–`5`, or the schema's own keys), in a row or a column, light or dark. Click a
  candidate to pick it; the arrows and the wheel turn pages. Applications that leave the
  preedit to the input method, such as xterm, see it in the window.
- **One composition per text field**; Chinese or English letters are the same in all.
- **A menu** (the tray, or `omime properties` and `omime activate <id>`): English
  letters (`ascii_mode`), full width (`full_shape`), simplified characters
  (`simplification`), English punctuation (`ascii_punct`), and "Deploy again"
  (`deploy`) after changing Rime's configuration.

## Settings

Open **Configure Rime…** from the tray, the settings button beside a Rime input
method in oh-my-ime's settings, or run `omime settings rime`. The window offers:

- Schemas to offer as Rime input methods. Add them to the switching order in
  oh-my-ime's settings afterwards.
- Candidates per page (1–10) and what pressing Shift alone does: switch and commit
  the letters, commit the candidate, clear the composition, or do nothing.
- A horizontal or vertical candidate window, system/light/dark appearance, and
  candidate font size (12–32 logical pixels).

Save writes Rime's choices into `default.custom.yaml` in its user directory,
preserving unrelated customizations. Candidate appearance goes into
`~/.config/oh-my-ime/rime.toml` (`$XDG_CONFIG_HOME/oh-my-ime/rime.toml`):

```toml
layout = "vertical"       # "horizontal" is the default
appearance = "system"     # or "light", "dark"
font_size = 18
```

When the window closes, oh-my-ime restarts Rime and it deploys the changes. A failed
save keeps the window open with an error so you can retry. Cancel discards choices
not yet saved. A schema with its own page-size or Shift overrides may take precedence
over these defaults; **Open the user folder** gives access to its custom YAML files.
The settings window is available in English and Chinese; schema names use simplified
characters except in traditional Chinese locales.

## Data

- Rime's schemas and dictionaries come from `$OMIME_RIME_SHARED_DATA`, else from
  `rime-data` in `$XDG_DATA_DIRS` (`/usr/share/rime-data` on most distributions). The
  Nix package, `nix profile install github:Ign1x/oh-my-ime#rime`, brings nixpkgs'
  `rime-data`.
- Your data, where Rime builds the schemas, learns your words and reads your
  customizations (`default.custom.yaml`, `<schema>.custom.yaml`), is
  `~/.local/share/oh-my-ime/rime`, apart from other Rime front ends'. Change the
  files there, then "Deploy again".
- The first time, and after changes, Rime deploys: it builds the dictionaries, which
  takes a few seconds. Keys go to applications as they are until it is done; the tray
  shows `…`.

## Building

The engine links librime 1.11 or later (`pkg-config rime`); `scripts/install.sh`
leaves the engine out where librime is not installed. Its bindings are written by hand
for the part of librime's C API it uses, in [`src/rime`](src/rime), the only unsafe
code of the engine.

## License

The engine is MIT OR Apache-2.0 like the rest of oh-my-ime. It links librime
(BSD-3-Clause) and uses Slint under the
[Slint Royalty-free License](https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md).

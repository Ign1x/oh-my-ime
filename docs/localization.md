# Localization

English | [简体中文](zh-CN/localization.md)

The platform provides English and Simplified Chinese for settings, the tray,
notifications, the switcher, and command-line help, diagnostics and setup. Rime's
settings and menu, and Sparkle's operation hint, use the same catalog. Other engines
and hosted frameworks supply their own translations.

## Choosing a language

Set your desktop's language to Simplified Chinese and start a new session. The
programs select the first nonempty `LC_ALL`, `LC_MESSAGES`, or `LANG`, in that order.
For example, to preview the command-line help:

```sh
LC_ALL=zh_CN.UTF-8 omime help
LC_ALL=zh_CN.UTF-8 omimed --help
LC_ALL=zh_CN.UTF-8 omime-settings --help
```

POSIX locale names (`zh_CN.UTF-8`) and language tags (`zh-Hans`) are accepted.
`LANGUAGE=fr:zh_CN:en` tries languages in order, so it currently selects Chinese.
An English preference stops fallback. `C`, `C.UTF-8` and `POSIX` always select
English, including when `LANGUAGE` is set. Unavailable translations use the English
source text. Traditional Chinese does not yet have a separate catalog.

Language is selected at process startup. Restart settings after changing its
environment. Tray entries and engine names come from omimed and its engines, so
their service environment also needs the chosen locale. A new desktop login applies
the locale consistently; setting it only in a terminal changes commands launched
from that terminal. Changing display language does not select an input method.

JSON keys and state IDs (`direct`, `stopped`, engine IDs), D-Bus member names,
configuration keys and setup markers retain their spelling. Human-facing JSON
values, such as a tooltip, can be translated. OS/library error details and engine
messages may remain in the language supplied by those components.

## Contributing a translation

The shared catalog is `crates/omime-proto/po/zh_CN.po`; translation code is in
`crates/omime-proto/src/i18n.rs`. Catalogs are embedded in each binary, so installing
the platform requires no separate gettext catalog installation.

Copy the PO catalog, change its `Language` header and translate each `msgstr`.
Register the new language tag and file in `CATALOGS`. A base tag such as `ja` serves
its regional variants; a specific tag can provide a regional translation. The
current message set uses singular entries without gettext contexts or plural forms.
The reader skips fuzzy, obsolete, contextual and plural entries.

In Rust, translate a complete message with named values:

```rust
omime_proto::tr!("Configure {engine}…", engine = engine_name)
```

Use `i18n::text("…")` for a static label and `i18n::message("…")` to mark a message
selected before translation, such as a constant or match arm. Slint settings use
`Tr.text`, `Tr.text1` and `Tr.text2`. Their sequential `{}` arguments can be written
as `{0}` and `{1}` in translations when the order must change. Preserve every
placeholder and translate whole sentences rather than joining translated fragments.
Values are inserted once, without interpreting braces inside paths or engine names.

Run:

```sh
python3 scripts/check-translations.py
cargo test -p omime-proto -p omime-cli -p omime-settings
```

The checker finds marked Rust and Slint messages, verifies coverage, rejects
duplicate or fuzzy entries, and checks placeholders. Desktop launchers use their
own localized fields in `data/io.github.ign1x.OhMyIme.Settings.desktop`; engine
manifests use `[engine.i18n.<tag>]`. Add those labels with a new language too.

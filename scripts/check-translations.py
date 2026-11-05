#!/usr/bin/env python3
"""Check embedded UI catalogs against marked Rust and Slint messages.

No gettext installation is needed. Keep runtime-selected messages inside
i18n::message("...") so they are checked alongside tr! and Tr.text calls.
"""

import ast
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STRING = r'"(?:[^"\\]|\\[\s\S])*"'
MESSAGE = re.compile(
    r'(?:\btr!|\bi18n::(?:text|message)|\b(?:tr|texts|Tr)\.(?:text|text1|text2|format))'
    r'\s*\(\s*(' + STRING + r')'
)


def catalog(path):
    entries = {}
    for block in path.read_text().split("\n\n"):
        entry = {"msgid": "", "msgstr": ""}
        field = None
        for line in block.splitlines():
            if line.startswith(("msgid ", "msgstr ")):
                field, value = line.split(" ", 1)
                entry[field] = ast.literal_eval(value)
            elif line.startswith('"') and field:
                entry[field] += ast.literal_eval(line)
        source = entry["msgid"]
        if source:
            assert source not in entries, f"{path}: duplicate {source!r}"
            assert entry["msgstr"], f"{path}: untranslated {source!r}"
            assert not re.search(r"^#,.*\bfuzzy\b", block, re.M), f"{path}: fuzzy {source!r}"
            entries[source] = entry["msgstr"]
    return entries


def parameters(text):
    values = []
    position = 0
    for match in re.finditer(r"\{\{|\}\}|\{([^{}]*)\}", text):
        if match[1] is None:
            continue
        name = match[1]
        if not name:
            name = str(position)
            position += 1
        values.append(name)
    return sorted(values)


def main():
    messages = set()
    for directory in (ROOT / "crates", ROOT / "engines"):
        for path in sorted(directory.rglob("*")):
            if path.suffix not in (".rs", ".slint") or path.name == "i18n.rs":
                continue
            source = re.split(r"#\[cfg\(test\)\]\s*mod tests\s*\{", path.read_text())[0]
            for match in MESSAGE.finditer(source):
                literal = re.sub(r"\\\r?\n\s*", "", match[1]).replace("\n", r"\n")
                messages.add(ast.literal_eval(literal))
    for path in sorted((ROOT / "crates/omime-proto/po").glob("*.po")):
        entries = catalog(path)
        missing = messages - entries.keys()
        assert not missing, f"{path}: missing translations: {sorted(missing)!r}"
        for source, translation in entries.items():
            assert parameters(source) == parameters(translation), f"{path}: placeholders differ: {source!r}"
        print(f"{path.relative_to(ROOT)}: {len(entries)} translations; {len(messages)} marked messages covered")


if __name__ == "__main__":
    main()

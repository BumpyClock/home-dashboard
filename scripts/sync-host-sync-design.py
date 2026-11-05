#!/usr/bin/env python3
"""Export the frame's default theme, design tokens and bundled fonts to Tauri."""
import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WEB = ROOT / "host-sync" / "desktop" / "web"


def assets():
    theme = (ROOT / "ui/theme.slint").read_text()
    match = re.search(r"property <Palette> c:\s*\{(.*?)\};", theme, re.S)
    assert match is not None, "Default frame palette not found in ui/theme.slint"
    palette = match.group(1)
    declarations = [
        f"  --color-{name}: {value};"
        for name, value in re.findall(r"([\w-]+):\s*(#[\da-fA-F]+)", palette)
    ]
    for name, value in re.findall(r'property <string> (font-[\w-]+): "([^"]+)";', theme):
        declarations.append(f'  --{name}: "{value}";')
    tokens = (ROOT / "ui/tokens.slint").read_text()
    for group, body in re.findall(r"export global (\w+) \{(.*?)\n\}", tokens, re.S):
        if group not in {"Space", "Radius", "Type", "Alpha", "Fixed", "Size"}:
            continue
        for name, value in re.findall(r"property <\w+> ([\w-]+): ([^;]+);", body):
            declarations.append(f"  --{group.lower()}-{name}: {value};")
    css = "/* Generated from ui/theme.slint and ui/tokens.slint. Run scripts/sync-host-sync-design.py. */\n:root {\n"
    css += "\n".join(declarations) + "\n}\n"
    for family, filename, weight in [
        ("Inter", "Inter-Regular.ttf", 400),
        ("Inter", "Inter-Medium.ttf", 500),
        ("Inter", "Inter-SemiBold.ttf", 600),
        ("Instrument Serif", "InstrumentSerif-Regular.ttf", 400),
    ]:
        css += f'@font-face {{ font-family: "{family}"; src: url("fonts/{filename}") format("truetype"); font-weight: {weight}; font-display: swap; }}\n'
        yield WEB / "fonts" / filename, (ROOT / "ui/fonts" / filename).read_bytes()
    for filename in ["OFL-Inter.txt", "OFL-instrumentserif.txt"]:
        yield WEB / "fonts" / filename, (ROOT / "ui/fonts" / filename).read_bytes()
    yield WEB / "tokens.css", css.encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Fail if committed desktop assets drift from frame tokens/fonts.")
    args = parser.parse_args()
    stale = []
    for target, content in assets():
        if args.check:
            if not target.exists() or target.read_bytes() != content:
                stale.append(str(target.relative_to(ROOT)))
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(content)
    if stale:
        print("Frame design assets drifted. Run python3 scripts/sync-host-sync-design.py\n" + "\n".join(stale), file=sys.stderr)
        return 1
    if args.check:
        css = (WEB / "style.css").read_text()
        if re.search(r"#[\da-fA-F]{3,8}\b|prefers-color-scheme", css):
            print("Use frame tokens in style.css, not raw colors or an independent OS palette.", file=sys.stderr)
            return 1
        print("PASS desktop uses frame tokens and exact bundled fonts")
    return 0


if __name__ == "__main__":
    sys.exit(main())

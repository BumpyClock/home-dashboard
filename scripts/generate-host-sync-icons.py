#!/usr/bin/env python3
"""Regenerate committed desktop/tray icons. Requires Pillow (python3 -m pip install pillow)."""
import re
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1] / "host-sync" / "desktop"
ICONS = ROOT / "icons"
ICONS.mkdir(parents=True, exist_ok=True)

theme = (ROOT.parents[1] / "ui" / "theme.slint").read_text()
colors = dict(re.findall(r"([\w-]+):\s*(#[\da-fA-F]+)", theme))
icon = Image.new("RGBA", (1024, 1024))
draw = ImageDraw.Draw(icon)
draw.rounded_rectangle((0, 0, 1023, 1023), radius=230, fill=colors["bg"])
draw.rounded_rectangle((224, 192, 800, 800), radius=72, outline=colors["ink"], width=48)
draw.rounded_rectangle((316, 284, 708, 636), radius=12, fill=colors["surface"])
draw.line([(340, 560), (464, 424), (568, 552), (648, 472), (684, 512)], fill=colors["accent"], width=30, joint="curve")
draw.ellipse((620, 324, 660, 364), fill=colors["accent"])
draw.ellipse((492, 696, 532, 736), fill=colors["ink"])
icon.save(ICONS / "icon.icns", sizes=[(256, 256), (512, 512), (1024, 1024)])
icon.save(ICONS / "icon.ico", sizes=[(32, 32), (64, 64), (128, 128), (256, 256)])
icon.resize((256, 256), Image.Resampling.LANCZOS).save(ICONS / "icon.png")

tray = Image.new("RGBA", (128, 128))
draw = ImageDraw.Draw(tray)
draw.rounded_rectangle((20, 12, 108, 116), radius=14, outline="black", width=9)
draw.rounded_rectangle((36, 28, 92, 86), radius=4, outline="black", width=6)
draw.ellipse((59, 97, 69, 107), fill="black")
tray.resize((32, 32), Image.Resampling.LANCZOS).save(ICONS / "tray.png")

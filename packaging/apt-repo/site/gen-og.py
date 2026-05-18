#!/usr/bin/env python3
"""Generate assets/brand/og.png — the social-share preview card."""

from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter, ImageFont

OUT = Path(__file__).resolve().parents[3] / "assets" / "brand" / "og.png"
W, H = 1200, 630


def _load_font(size: int, weight: str = "regular") -> ImageFont.FreeTypeFont:
    candidates = {
        "regular": [
            "/System/Library/Fonts/Supplemental/Inter-Regular.ttf",
            "/Library/Fonts/Inter-Regular.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
            "/System/Library/Fonts/SFNS.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ],
        "bold": [
            "/System/Library/Fonts/Supplemental/Inter-Bold.ttf",
            "/Library/Fonts/Inter-Bold.ttf",
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        ],
        "mono": [
            "/System/Library/Fonts/SFNSMono.ttf",
            "/System/Library/Fonts/Menlo.ttc",
            "/Library/Fonts/JetBrainsMono-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        ],
    }[weight]
    for p in candidates:
        if Path(p).exists():
            try:
                return ImageFont.truetype(p, size=size)
            except OSError:
                continue
    return ImageFont.load_default()


def _hex(rgba: str) -> tuple:
    s = rgba.lstrip("#")
    if len(s) == 6:
        s += "ff"
    return tuple(int(s[i : i + 2], 16) for i in (0, 2, 4, 6))


def make() -> None:
    base = Image.new("RGB", (W, H), _hex("#07080a")[:3])

    # ---- aurora glow ----
    glow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    gd.ellipse((-220, -200, 720, 540), fill=(59, 130, 246, 130))
    gd.ellipse((520, -160, 1440, 520), fill=(168, 85, 247, 110))
    gd.ellipse((260, 220, 1100, 760), fill=(6, 182, 212, 80))
    glow = glow.filter(ImageFilter.GaussianBlur(110))
    base = Image.alpha_composite(base.convert("RGBA"), glow)

    # ---- grid overlay ----
    grid = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    gd = ImageDraw.Draw(grid)
    step = 56
    line = (255, 255, 255, 14)
    for x in range(0, W, step):
        gd.line([(x, 0), (x, H)], fill=line, width=1)
    for y in range(0, H, step):
        gd.line([(0, y), (W, y)], fill=line, width=1)
    # fade grid toward bottom-right
    mask = Image.new("L", (W, H), 0)
    md = ImageDraw.Draw(mask)
    for r, alpha in [(820, 220), (1040, 120), (1280, 50)]:
        md.ellipse((-r, -r, r, r), fill=alpha)
    mask = mask.filter(ImageFilter.GaussianBlur(120))
    grid.putalpha(Image.eval(mask, lambda v: int(v * 0.6)))
    base = Image.alpha_composite(base, grid)

    draw = ImageDraw.Draw(base)

    # ---- eyebrow chip ----
    chip_text = "THE FIRST AGENT-NATIVE OS"
    chip_font = _load_font(20, "bold")
    chip_w = draw.textlength(chip_text, font=chip_font)
    pad_x, pad_y = 22, 12
    chip_x, chip_y = 80, 100
    draw.rounded_rectangle(
        (chip_x, chip_y, chip_x + chip_w + pad_x * 2, chip_y + 44),
        radius=22,
        outline=(255, 255, 255, 50),
        width=1,
        fill=(255, 255, 255, 16),
    )
    # cyan pulse dot
    dot_cx, dot_cy = chip_x + 18, chip_y + 22
    draw.ellipse((dot_cx - 5, dot_cy - 5, dot_cx + 5, dot_cy + 5), fill=(6, 182, 212, 255))
    draw.text((chip_x + 34, chip_y + 11), chip_text, font=chip_font, fill=(232, 238, 246, 255))

    # ---- title ----
    title_font = _load_font(96, "bold")
    line1 = "An operating system"
    line2 = "for AI agents."
    title_x = 80
    title_y = 170
    draw.text((title_x, title_y), line1, font=title_font, fill=(245, 247, 250, 255))

    # gradient text on line2: render twice (mask + gradient)
    grad = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    gd = ImageDraw.Draw(grad)
    # build horizontal gradient strip
    strip = Image.new("RGBA", (W, 1))
    for x in range(W):
        t = x / W
        if t < 0.5:
            k = t / 0.5
            r = int(255 * (1 - k) + 198 * k)
            g = int(255 * (1 - k) + 226 * k)
            b = int(255 * (1 - k) + 255 * k)
        else:
            k = (t - 0.5) / 0.5
            r = int(198 * (1 - k) + 168 * k)
            g = int(226 * (1 - k) + 85 * k)
            b = int(255 * (1 - k) + 247 * k)
        strip.putpixel((x, 0), (r, g, b, 255))
    grad = strip.resize((W, H))
    text_mask = Image.new("L", (W, H), 0)
    ImageDraw.Draw(text_mask).text((title_x, title_y + 110), line2, font=title_font, fill=255)
    base.paste(grad, (0, 0), text_mask)

    # ---- lede ----
    lede_font = _load_font(28, "regular")
    lede1 = "Structured cos primitives, scoped approvals, checkpoint-and-rollback,"
    lede2 = "and a local model runtime — so an agent can drive your machine."
    draw = ImageDraw.Draw(base)
    draw.text((title_x, title_y + 250), lede1, font=lede_font, fill=(200, 205, 214, 255))
    draw.text((title_x, title_y + 288), lede2, font=lede_font, fill=(200, 205, 214, 255))

    # ---- footer row: brand + meta ----
    foot_y = H - 90
    brand_font = _load_font(28, "bold")
    draw.text((title_x, foot_y), "Claw OS", font=brand_font, fill=(245, 247, 250, 255))
    meta_font = _load_font(20, "mono")
    meta = "xiaoyu-work.github.io/claw-os"
    meta_w = draw.textlength(meta, font=meta_font)
    draw.text((W - 80 - meta_w, foot_y + 4), meta, font=meta_font, fill=(138, 147, 163, 255))

    # subtle hairline divider above footer
    draw.line(
        [(title_x, foot_y - 22), (W - 80, foot_y - 22)],
        fill=(255, 255, 255, 18),
        width=1,
    )

    OUT.parent.mkdir(parents=True, exist_ok=True)
    base.convert("RGB").save(OUT, "PNG", optimize=True)
    print(f"wrote {OUT}")


if __name__ == "__main__":
    make()

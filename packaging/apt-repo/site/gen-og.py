#!/usr/bin/env python3
"""Generate assets/brand/og.png — light-theme social-share preview card."""

from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter, ImageFont

REPO_ROOT = Path(__file__).resolve().parents[3]
OUT = REPO_ROOT / "assets" / "brand" / "og.png"
SYMBOL = REPO_ROOT / "assets" / "brand" / "clawos-symbol.png"
WORDMARK = REPO_ROOT / "assets" / "brand" / "clawos-wordmark.png"

W, H = 1200, 630
MARGIN = 80


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


def make() -> None:
    base = Image.new("RGBA", (W, H), (250, 251, 252, 255))

    # ---- soft aurora ----
    glow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    gd.ellipse((-220, -240, 760, 540), fill=(37, 99, 235, 70))
    gd.ellipse((520, -180, 1440, 520), fill=(124, 58, 237, 60))
    gd.ellipse((260, 220, 1100, 760), fill=(8, 145, 178, 45))
    glow = glow.filter(ImageFilter.GaussianBlur(130))
    base = Image.alpha_composite(base, glow)

    # ---- light grid ----
    grid = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    gd = ImageDraw.Draw(grid)
    step = 56
    line = (11, 18, 32, 14)
    for x in range(0, W, step):
        gd.line([(x, 0), (x, H)], fill=line, width=1)
    for y in range(0, H, step):
        gd.line([(0, y), (W, y)], fill=line, width=1)
    mask = Image.new("L", (W, H), 0)
    md = ImageDraw.Draw(mask)
    for r, alpha in [(820, 220), (1040, 120), (1280, 50)]:
        md.ellipse((-r, -r, r, r), fill=alpha)
    mask = mask.filter(ImageFilter.GaussianBlur(120))
    grid.putalpha(Image.eval(mask, lambda v: int(v * 0.55)))
    base = Image.alpha_composite(base, grid)

    draw = ImageDraw.Draw(base)

    # ---- eyebrow chip ----
    chip_text = "THE FIRST AGENT NATIVE OS"
    chip_font = _load_font(20, "bold")
    chip_w = draw.textlength(chip_text, font=chip_font)
    pad_x = 22
    chip_x, chip_y = MARGIN, 100
    draw.rounded_rectangle(
        (chip_x, chip_y, chip_x + chip_w + pad_x * 2, chip_y + 44),
        radius=22,
        outline=(37, 99, 235, 80),
        width=1,
        fill=(37, 99, 235, 22),
    )
    dot_cx, dot_cy = chip_x + 18, chip_y + 22
    draw.ellipse((dot_cx - 5, dot_cy - 5, dot_cx + 5, dot_cy + 5), fill=(8, 145, 178, 255))
    draw.text((chip_x + 34, chip_y + 11), chip_text, font=chip_font, fill=(52, 64, 84, 255))

    # ---- title ----
    title_font = _load_font(92, "bold")
    line1 = "The first agent native"
    line2 = "operating system."
    title_x = MARGIN
    title_y = 168

    # plain ink on line 1
    draw.text((title_x, title_y), line1, font=title_font, fill=(11, 18, 32, 255))

    # gradient on line 2
    strip = Image.new("RGBA", (W, 1))
    for x in range(W):
        t = x / W
        if t < 0.5:
            k = t / 0.5
            r = int(37  * (1 - k) + 8   * k)
            g = int(99  * (1 - k) + 145 * k)
            b = int(235 * (1 - k) + 178 * k)
        else:
            k = (t - 0.5) / 0.5
            r = int(8   * (1 - k) + 124 * k)
            g = int(145 * (1 - k) + 58  * k)
            b = int(178 * (1 - k) + 237 * k)
        strip.putpixel((x, 0), (r, g, b, 255))
    grad = strip.resize((W, H))
    text_mask = Image.new("L", (W, H), 0)
    ImageDraw.Draw(text_mask).text(
        (title_x, title_y + 108), line2, font=title_font, fill=255
    )
    base.paste(grad, (0, 0), text_mask)

    # ---- lede ----
    draw = ImageDraw.Draw(base)
    lede_font = _load_font(26, "regular")
    lede1 = "A Linux distribution rebuilt around the agent — structured cos"
    lede2 = "primitives, scoped approvals, checkpoint-and-rollback, local models."
    draw.text((title_x, title_y + 240), lede1, font=lede_font, fill=(52, 64, 84, 255))
    draw.text((title_x, title_y + 278), lede2, font=lede_font, fill=(52, 64, 84, 255))

    # ---- footer row: symbol + wordmark + url ----
    foot_y = H - 110
    # divider
    draw.line(
        [(MARGIN, foot_y - 6), (W - MARGIN, foot_y - 6)],
        fill=(11, 18, 32, 22),
        width=1,
    )

    if SYMBOL.exists():
        sym = Image.open(SYMBOL).convert("RGBA")
        sym.thumbnail((56, 56), Image.LANCZOS)
        base.paste(sym, (MARGIN, foot_y + 14), sym)
        wm_x = MARGIN + sym.width + 14
    else:
        wm_x = MARGIN

    if WORDMARK.exists():
        wm = Image.open(WORDMARK).convert("RGBA")
        wm.thumbnail((220, 38), Image.LANCZOS)
        base.paste(wm, (wm_x, foot_y + 24), wm)

    meta_font = _load_font(20, "mono")
    meta = "xiaoyu-work.github.io/claw-os"
    meta_w = draw.textlength(meta, font=meta_font)
    draw.text(
        (W - MARGIN - meta_w, foot_y + 32),
        meta,
        font=meta_font,
        fill=(91, 102, 117, 255),
    )

    OUT.parent.mkdir(parents=True, exist_ok=True)
    base.convert("RGB").save(OUT, "PNG", optimize=True)
    print(f"wrote {OUT}")


if __name__ == "__main__":
    make()

"""生成 liquid-glass-rs 的图标：圆角玻璃方块 + 高光 + 折射边。

输出 icon.ico（多尺寸）和 icon-preview.png。
"""

import math
import os

from PIL import Image, ImageDraw, ImageFilter

SIZE = 512
OUT_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def rounded_mask(size, radius):
    m = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(m)
    d.rounded_rectangle([0, 0, size - 1, size - 1], radius=radius, fill=255)
    return m


def make_icon(size=SIZE):
    # 背景：深色渐变（模拟桌面）
    bg = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(bg)
    for y in range(size):
        t = y / size
        r = int(28 + 40 * t)
        g = int(36 + 70 * t)
        b = int(58 + 120 * t)
        d.line([(0, y), (size, y)], fill=(r, g, b, 255))

    # 彩色光斑（玻璃后面透出来的东西）
    blobs = [
        ((0.18, 0.22), (255, 88, 120), 0.30),
        ((0.82, 0.28), (255, 200, 70), 0.26),
        ((0.30, 0.82), (70, 210, 255), 0.28),
        ((0.78, 0.78), (170, 110, 255), 0.24),
    ]
    for (cx, cy), color, rad in blobs:
        layer = Image.new("RGBA", (size, size), (0, 0, 0, 0))
        ld = ImageDraw.Draw(layer)
        r = int(size * rad)
        ld.ellipse(
            [int(cx * size) - r, int(cy * size) - r, int(cx * size) + r, int(cy * size) + r],
            fill=color + (110,),
        )
        layer = layer.filter(ImageFilter.GaussianBlur(size // 14))
        bg = Image.alpha_composite(bg, layer)

    # 玻璃卡片
    margin = int(size * 0.16)
    w = size - margin * 2
    radius = int(w * 0.26)

    glass = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glass)

    # 玻璃本体：半透明白
    gd.rounded_rectangle(
        [margin, margin, margin + w, margin + w],
        radius=radius,
        fill=(255, 255, 255, 46),
    )

    # 背景被"折射"进玻璃：把 bg 模糊后塞进卡片
    inner = bg.crop((margin, margin, margin + w, margin + w))
    inner = inner.filter(ImageFilter.GaussianBlur(w // 26))
    # 稍微放大一点，模拟折射位移
    inner = inner.resize((int(w * 1.06), int(w * 1.06)), Image.LANCZOS)
    off = (inner.width - w) // 2
    inner = inner.crop((off, off, off + w, off + w))
    inner.putalpha(rounded_mask(w, radius).point(lambda v: int(v * 0.92)))
    glass.paste(inner, (margin, margin), inner)

    # 顶部高光渐变
    hi = Image.new("RGBA", (w, w), (0, 0, 0, 0))
    hd = ImageDraw.Draw(hi)
    for y in range(w):
        a = int(70 * max(0.0, 1 - y / (w * 0.55)))
        hd.line([(0, y), (w, y)], fill=(255, 255, 255, a))
    hi.putalpha(Image.composite(hi.getchannel("A"), Image.new("L", (w, w), 0), rounded_mask(w, radius)))
    glass.paste(hi, (margin, margin), hi)

    # 边缘描边
    gd.rounded_rectangle(
        [margin, margin, margin + w, margin + w],
        radius=radius,
        outline=(255, 255, 255, 150),
        width=max(2, size // 160),
    )

    out = Image.alpha_composite(bg, glass)
    return out


def main():
    img = make_icon()
    os.makedirs(OUT_DIR, exist_ok=True)

    preview = os.path.join(OUT_DIR, "icon-preview.png")
    img.save(preview)

    ico = os.path.join(OUT_DIR, "icon.ico")
    sizes = [(256, 256), (128, 128), (64, 64), (48, 48), (32, 32), (16, 16)]
    img.save(ico, format="ICO", sizes=sizes)

    print(f"已生成: {ico}")
    print(f"已生成: {preview}")


if __name__ == "__main__":
    main()

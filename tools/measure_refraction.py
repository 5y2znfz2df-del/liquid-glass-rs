"""验证折射：测试图案是每 22px 一条的横线 + 竖线。

折射位移发生在**法线的水平方向**，所以要量竖线被掰弯的程度：
- 玻璃中心：位移≈0，竖线位置和原图案一致
- 玻璃左右边缘：位移最大，竖线明显左右错开

自适应窗口尺寸（玻璃居中、四周留 4px），不写死坐标。

用法: python measure_refraction.py <图片路径>
"""

import sys

from PIL import Image


def line_centers(d, w, y, x0, x1):
    """在一行里找白色竖线的中心（亮度局部峰值）"""

    def lum(x):
        i = (y * w + x) * 3
        return 0.299 * d[i] + 0.587 * d[i + 1] + 0.114 * d[i + 2]

    vals = [lum(x) for x in range(x0, x1)]
    peaks = []
    for k in range(1, len(vals) - 1):
        if vals[k] > 150 and vals[k] >= vals[k - 1] and vals[k] > vals[k + 1]:
            peaks.append(x0 + k)
    merged = []
    for p in peaks:
        if merged and p - merged[-1] <= 3:
            merged[-1] = (merged[-1] + p) // 2
        else:
            merged.append(p)
    return merged


def deviation(x):
    """离最近的 22 整数倍差多少（测试图案周期 22px）"""
    r = x % 22
    return r if r <= 11 else r - 22


def main(path):
    im = Image.open(path).convert("RGB")
    w, h = im.size
    d = im.tobytes()

    # 玻璃居中，四周留 4px
    gx0, gx1 = 4, w - 4
    gy0, gy1 = 4, h - 4
    mid_y = (gy0 + gy1) // 2

    print(f"图 {w}x{h}  玻璃 x[{gx0}..{gx1}] y[{gy0}..{gy1}]  取中间行 y={mid_y}")
    print()

    zones = [
        ("左边缘 6~40px", gx0 + 6, gx0 + 40),
        ("左中 60~120px", gx0 + 60, gx0 + 120),
        ("正中心 ±40px", w // 2 - 40, w // 2 + 40),
        ("右中 60~120px", gx1 - 120, gx1 - 60),
        ("右边缘 6~40px", gx1 - 40, gx1 - 6),
    ]
    for name, a, b in zones:
        cs = line_centers(d, w, mid_y, a, b)
        if not cs:
            print(f"{name:16} 没找到竖线")
            continue
        devs = [deviation(x) for x in cs]
        avg = sum(devs) / len(devs)
        print(f"{name:16} 竖线 x={cs}  位移 {devs}  平均 {avg:+.1f} px")

    print()
    print("判定：正中心位移应接近 0；左右边缘应有明显位移（越大说明折射越强）")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("用法: python measure_refraction.py <图片路径>")
        sys.exit(1)
    main(sys.argv[1])

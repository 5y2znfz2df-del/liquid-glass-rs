"""验证折射：测试图案是每 22px 一条的横线 + 竖线。

折射位移发生在**法线的水平方向**，所以要量竖线被掰弯的程度：
- 玻璃中心：位移≈0，竖线位置和原图案一致
- 玻璃边缘：位移最大，竖线明显左右错开

用法: python measure_refraction.py <图片路径>
"""

import sys

from PIL import Image


def main(path):
    im = Image.open(path).convert("RGB")
    W, H = im.size
    buf = im.tobytes()

    def pixel(x, y):
        o = (y * W + x) * 3
        return buf[o], buf[o + 1], buf[o + 2]

    def white_runs(y, x0, x1):
        """这一行里 [x0,x1) 范围内的白色区间"""
        runs = []
        start = None
        for x in range(x0, x1):
            r, g, b = pixel(x, y)
            w = r > 232 and g > 232 and b > 232
            if w and start is None:
                start = x
            elif not w and start is not None:
                runs.append((start + x - 1) / 2.0)
                start = None
        if start is not None:
            runs.append((start + x1 - 1) / 2.0)
        return runs

    print("图片:", path, (W, H))
    # 选一行不在横线上（横线每 22px，y=211 附近是空白）
    for y in [205, 211, 217]:
        print("  y=%d" % y)
        for label, x0, x1 in [
            ("左边缘区 x=95..170", 95, 170),
            ("中心区   x=250..330", 250, 330),
            ("右边缘区 x=450..525", 450, 525),
        ]:
            runs = white_runs(y, x0, x1)
            pretty = ", ".join("%.1f" % v for v in runs)
            print("    %-20s 竖线中心 x: %s" % (label, pretty))
        break

    print()
    print("  原图案竖线位置（偏移 0，每 22px 一条，从 -22 起）:")
    true_x = [v for v in range(-22, 620, 22)]
    print("    ...", ", ".join(str(v) for v in true_x[8:12]), "...")

    print()
    print("  左边缘处竖线的位移量（负=向左折射）:")
    ref = white_runs(211, 250, 330)
    base = min((abs(v - 110.0), v) for v in ref)[1] if ref else 110.0
    for y in [205, 211, 217]:
        runs = white_runs(y, 95, 170)
        cand = [v for v in runs if 95 <= v <= 135]
        if not cand:
            continue
        obs = min(cand, key=lambda v: abs(v - 110.0))
        print("    y=%d  观察到 x=%6.1f   位移 %+6.1f px" % (y, obs, obs - 110.0))


if __name__ == "__main__":
    main(sys.argv[1])

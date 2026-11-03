# liquid-glass-rs

Windows 上的**真·液态玻璃**渲染实验：Skia + SkSL 运行时着色器。

不是磨砂玻璃（`backdrop-filter: blur` 那种），而是**真折射**：

- 圆角矩形有符号距离场（SDF）→ 算出每个像素的边缘法线
- **斯涅尔折射定律**（`refract()`）→ 背景被真实扭曲
- **RGB 三通道色散** → 边缘出现棱镜彩边
- 方向性高光 + 边缘包边 → 玻璃的厚度感

## 当前进度

| 阶段 | 状态 |
| --- | --- |
| skia-safe 编译 | ✅ 通过（v0.156.0 + d3d） |
| SkSL 着色器编译运行 | ✅ 通过 |
| 折射 / 色散 / 高光 | ✅ 出效果 |
| 无边框透明置顶窗口 | ✅ 通过 |
| 抓真实桌面当背景 | ✅ 通过 |
| **跟手高光** | ✅ 光标方向决定光照方位 |
| **液态融合形变** | ✅ 光标靠近边缘，玻璃鼓起一个包（smin 平滑并集） |
| **点击穿透** | ✅ 按 `T` 切换 |
| **拖拽移动 + 位置记忆** | ✅ 下次启动回到原位 |
| 性能 | ✅ 复用 surface 和像素缓冲（不再每帧分配） |
| 打包 | ✅ release 5.3 MB，带图标和版本信息 |

**现在是一个能跑的液态玻璃窗口**：玻璃里显示窗口背后的真实桌面，文字被折射掰弯，
高光跟着光标转。

![真实渲染](final-frame.png)

## 操作

| 操作 | 作用 |
| --- | --- |
| **按住左键拖** | 移动窗口（位置自动记住，下次启动恢复） |
| 右键 | 退出 |
| `T` | 切换点击穿透（鼠标直接穿过窗口） |
| `ESC` | 退出 |

按键用 `GetAsyncKeyState` 轮询，**穿透模式下也能用**（那时窗口拿不到焦点）。

窗口位置存在 exe 同级的 `window-pos.txt`；崩溃会写 `liquid-glass-error.log`
（GUI 子系统没有控制台，没这个文件崩溃就是无声的）。

## 命令行参数

| 参数 | 作用 |
| --- | --- |
| 无 | 开窗口 |
| `--offscreen` | 只渲一张 PNG（测试图案背景） |
| `--dump-frame <路径>` | 跑 60 帧后把渲染结果存盘退出 |
| `--glass-only` | 只画玻璃不画背景，PNG 的 alpha 就是玻璃轮廓 |
| `--blob x,y,r` | 强制指定融合小圆（不受真实鼠标干扰，方便自动化验证） |
| `--no-exclude` | 不设 `WDA_EXCLUDEFROMCAPTURE`（调试用，见下方警告） |

### 怎么量化验证效果

`tools/` 下有两个脚本：

```powershell
python tools/measure_refraction.py liquid-glass-out.png
```

在测试图案（横线+竖线网格）上量竖线的位移：
玻璃中心位移 ≈ 0，玻璃边缘位移 ≈ 8~18 px —— 这就是折射的量。

```powershell
# 融合形变：看玻璃轮廓有没有被撑开
cargo run -- --glass-only --blob 0,0,0     --dump-frame shape-none.png
cargo run -- --glass-only --blob 210,0,46  --dump-frame shape-bulge.png
```

两个 PNG 的包围盒对比，能看出小圆把轮廓撑出去多少。

### ⚠️ 怎么截到自己的窗口

窗口设了 `WDA_EXCLUDEFROMCAPTURE`，所以**任何截屏工具都看不到它** ——
你用 PrintScreen 或 PowerShell 截那块区域，看到的是"背后的桌面"，不是玻璃。

想看渲染结果，用 `--dump-frame`，让程序自己把 Skia 画面存成 PNG。

**别用 `--no-exclude` 去截图验证效果**：那样抓背景会抓到自己的上一帧，
正反馈累积，玻璃会越来越白，最后糊成一片纯白。这个现象本身反过来证明了排除的必要性。

## 怎么跑

```powershell
cargo run                # 开窗口（ESC 退出）
cargo run -- --offscreen # 只渲染一张 PNG
```

### ⚠️ 首次编译的坑：Skia 预编译包下不来

`skia-bindings` 会从 GitHub Releases 下预编译 Skia。本机直连失败（curl error 35 / SSL），
失败后会回退到**源码编译**，那需要 LLVM，会报 `Unable to locate LLVM installation`。

**解法**：手动下载后让 cargo 读本地文件（`file://` 协议）。

```powershell
# 1. 用镜像下载（ghfast.top / ghproxy.net / gh-proxy.com 都行）
$url = "https://ghfast.top/https://github.com/rust-skia/skia-binaries/releases/download/0.156.0/skia-binaries-ce6727ef251518df37bc-x86_64-pc-windows-msvc-d3d-ganesh-jpegd-jpege-pdf.tar.gz"
# 用浏览器或 Python 下到本地，比如 C:\tmp\skia-binaries.tar.gz

# 2. 告诉 cargo 用本地文件（注意是 file:// 两个斜杠，三个斜杠会报 os error 123）
$env:SKIA_BINARIES_URL = "file://C:/tmp/skia-binaries.tar.gz"

# 3. 编译
cargo run
```

成功后会打印 `OK -> liquid-glass-out.png`。

### 其他环境变量

国内网络下 crates 走镜像：`~/.cargo/config.toml` 里配 rsproxy。

## 技术要点

### 透明窗口怎么做的

`WS_EX_LAYERED` + `UpdateLayeredWindow`：Skia 画到内存位图 → 拷进 32 位 DIB →
一次 `UpdateLayeredWindow` 上屏。逐像素 alpha，不需要 D3D12/DirectComposition。

三个坑：

1. **`pt_dst` 必须传窗口当前位置**，否则 ULW 会把窗口挪到 (0,0)。
2. **`peek_pixels()` 拿不到数据**（返回的 Pixmap 里 `pixels()` 是 `None`），
   要用 `surface.read_pixels(&info, &mut buf, row_bytes, (0,0))`。
3. **canvas 的可变借用会挡住 `image_snapshot()`**，画背景要用 `{ }` 包住作用域。

### 抓桌面怎么做的

一开始想用 Windows.Graphics.Capture（WinRT），但那是异步帧回调 + D3D11 设备，
接线复杂。**实测 BitBlt 就够了**：

```rust
BitBlt(memDC, 0, 0, w, h, screenDC, x, y, SRCCOPY)   // 抓窗口所在区域
GetDIBits(memDC, bmp, ...)                            // 取出 BGRA 像素
```

关键是**必须先把窗口排除在捕获之外**，否则会抓到自己的玻璃 → 无限套娃：

```rust
SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
```

实测（2026-11-02）：设了之后 BitBlt 抓到的是**窗口背后的真实桌面**，
不是黑块、不是自己的画面。A/B 对比验证过。

代价：BitBlt 抓不到硬件加速的内容（视频播放器、游戏画面可能全黑）。
要支持那些得换 Windows.Graphics.Capture。

### 液态融合是怎么做的

形状不是画出来的，是**距离场（SDF）算出来的**：

```glsl
float sdRoundRect(float2 p, float2 halfSize, float r) { ... }  // 圆角矩形
float smin(float a, float b, float k) { ... }                   // 平滑并集
float sceneD(float2 p) {                                        // 整体形状
    float d = sdRoundRect(p, rectSize * 0.5, corner);
    if (blobR > 0.5) { d = smin(d, length(p - blobPos) - blobR, 30.0); }
    return d;
}
```

`blobPos` / `blobR` 是跟手小圆的位置和半径，Rust 侧按光标位置算，
每帧用 `dt` 插值（所以快速移动光标有液态的甩尾）。

**法线用中心差分求**，因为融合后的形状没有解析梯度：

```glsl
float2 grad = float2(
    sceneD(local + float2(e, 0.0)) - sceneD(local - float2(e, 0.0)),
    sceneD(local + float2(0.0, e)) - sceneD(local - float2(0.0, e))
);
```

⚠️ 有个坑：绘制区域必须比玻璃本体**大 70px**，否则融合出来的小圆会被
绘制区域裁掉。区域外的像素着色器返回全透明，不会有硬边。

⚠️ 还有一个坑：距离场在**中轴线**上梯度会退化（distance-to-boundary 的
极小值线），那里 `normalize(0)` 会得到 NaN，整块玻璃会变透明。
代码里判断了梯度长度，太小就退回矩形的解析方向。

### 下一步

1. **多块玻璃**：现在是单个小圆，要真正的多块融合得把 SDF 改成数组循环
2. **真正的形变动画**：形态变化（比如滑入的方形变圆形）需要两个 shape 的权重混合
3. **高 DPI**：现在不处理缩放，150% 缩放下窗口偏小
4. **硬件加速内容**：BitBlt 抓不到，得换 Windows.Graphics.Capture

## 许可证

**GPLv3**（见 `LICENSE`）。

着色器为原创实现，使用的是公开光学公式。设计思路参考了 WinIsland(GPLv3)、
Apple Liquid Glass 的公开设计、ios-siri-animation(MIT)，**未复制任何代码**。

如果后续引入他人 GPLv3 代码，本项目整体保持 GPLv3，符合协议。

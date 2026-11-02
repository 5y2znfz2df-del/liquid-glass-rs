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
| **点击穿透** | ✅ 按 `T` 切换 |
| 性能 | ✅ 复用 surface 和像素缓冲（不再每帧分配） |

**现在是一个能跑的液态玻璃窗口**：玻璃里显示窗口背后的真实桌面，文字被折射掰弯，
高光跟着光标转。

![真实渲染](final-frame.png)

## 操作

| 按键 | 作用 |
| --- | --- |
| `T` | 切换点击穿透（鼠标直接穿过窗口） |
| `ESC` | 退出 |

按键用 `GetAsyncKeyState` 轮询，**穿透模式下也能用**（那时窗口拿不到焦点）。

## 命令行参数

| 参数 | 作用 |
| --- | --- |
| 无 | 开窗口 |
| `--offscreen` | 只渲一张 PNG（测试图案背景） |
| `--dump-frame <路径>` | 跑 60 帧后把渲染结果存盘退出 |
| `--no-exclude` | 不设 `WDA_EXCLUDEFROMCAPTURE`（调试用，见下方警告） |

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

### 下一步

1. **跟手高光**：鼠标位置 → 调整 `lightAngle` uniform
2. **点击穿透**：`WS_EX_TRANSPARENT`
3. **性能优化**：现在每帧都新建 Skia surface + 抓屏，可以复用

## 许可证

**GPLv3**（见 `LICENSE`）。

着色器为原创实现，使用的是公开光学公式。设计思路参考了 WinIsland(GPLv3)、
Apple Liquid Glass 的公开设计、ios-siri-animation(MIT)，**未复制任何代码**。

如果后续引入他人 GPLv3 代码，本项目整体保持 GPLv3，符合协议。

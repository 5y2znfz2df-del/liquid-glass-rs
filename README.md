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
| 折射 / 色散 / 高光 | ✅ 出效果（见 `liquid-glass-out.png`） |
| 窗口 + 桌面抓取 | ⬜ 未做 |

现在跑的是**离屏渲染**：画一张图、存成 PNG，不涉及窗口。

## 怎么跑

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

## 下一步

1. **窗口**：winit 建无边框透明窗口
2. **抓背景**：Windows.Graphics.Capture 抓窗口背后的画面
3. **合成**：Skia D3D12 后端 + DirectComposition 上屏
4. **交互**：跟随鼠标的高光、点击穿透

## 许可证

**GPLv3**（见 `LICENSE`）。

着色器为原创实现，使用的是公开光学公式。设计思路参考了 WinIsland(GPLv3)、
Apple Liquid Glass 的公开设计、ios-siri-animation(MIT)，**未复制任何代码**。

如果后续引入他人 GPLv3 代码，本项目整体保持 GPLv3，符合协议。

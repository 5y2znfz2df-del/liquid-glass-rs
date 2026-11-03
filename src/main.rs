// 液态玻璃 · 里程碑 2：真窗口
//
// 里程碑 1 已验证 SkSL 着色器（折射+色散+高光）能跑。
// 这一步把它搬进一个**无边框、透明、置顶**的窗口。
//
// 透明方案：WS_EX_LAYERED + UpdateLayeredWindow（逐像素 alpha）。
// 比 D3D12 + DirectComposition 简单得多，效果一样，性能足够。
//
// 用法：
//   cargo run                → 开窗口
//   cargo run -- --offscreen → 只渲染一张 PNG（保留里程碑 1 的验证能力）
//
// 许可证：GPLv3（见 LICENSE）。着色器为原创实现，未复制他人代码。

// 不弹控制台窗口（否则会盖在玻璃上面）
#![windows_subsystem = "windows"]

use skia_safe::{
    surfaces, Color, Data, EncodedImageFormat, Paint, PaintStyle, Rect, RuntimeEffect,
    SamplingOptions, Surface, TileMode,
};
use std::ffi::c_void;
use std::fs;
use std::time::Instant;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject,
    GetDC, GetDIBits, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE, VK_T};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetWindowLongPtrW, GetWindowRect, SetWindowDisplayAffinity, SetWindowLongPtrW,
    SetWindowPos, UpdateLayeredWindow, GWL_EXSTYLE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    ULW_ALPHA, WDA_EXCLUDEFROMCAPTURE, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId, WindowLevel};

/// 窗口尺寸
const W: i32 = 620;
const H: i32 = 420;

// ---------------------------------------------------------------- 着色器 --
const LIQUID_SKSL: &str = r#"
uniform shader backdrop;

uniform float2 rectSize;
uniform float2 center;
uniform float  corner;
uniform float  bezel;
uniform float  thickness;
uniform float  ior;
uniform float  dispersion;
uniform float  lightAngle;
uniform float  tintAmount;
uniform float  pad0;
uniform float2 blobPos;
uniform float  blobR;
uniform float  pad1;
uniform float4 tintColor;

// 圆角矩形的有符号距离场
float sdRoundRect(float2 p, float2 halfSize, float r) {
    float2 q = abs(p) - halfSize + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

// 平滑并集：让两块玻璃"融"在一起，而不是硬边拼接
float smin(float a, float b, float k) {
    float h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

// 整个玻璃的形状 = 主体矩形 ∪ 跟手小圆
float sceneD(float2 p) {
    float d = sdRoundRect(p, rectSize * 0.5, corner);
    if (blobR > 0.5) {
        d = smin(d, length(p - blobPos) - blobR, 30.0);
    }
    return d;
}

half4 main(float2 coord) {
    float2 local = coord - center;

    float d = sceneD(local);
    if (d > 0.0) { return half4(0.0); }

    // 形状可能是两个图形融出来的，没有解析梯度，用中心差分求法线
    float e = 1.0;
    float2 grad = float2(
        sceneD(local + float2(e, 0.0)) - sceneD(local - float2(e, 0.0)),
        sceneD(local + float2(0.0, e)) - sceneD(local - float2(0.0, e))
    );
    float glen = length(grad);
    // 中轴线上梯度会退化，这时退回矩形的解析方向
    float2 gRect = normalize(float2(
        local.x / max(rectSize.x * 0.5, 1.0),
        local.y / max(rectSize.y * 0.5, 1.0)
    ) + float2(1e-5, 1e-5));
    float2 g = glen > 0.002 ? grad / glen : gRect;

    float inward = -d;
    float band = clamp(inward / max(bezel, 0.001), 0.0, 1.0);

    float u = 1.0 - band;
    float slope = band < 1.0
        ? (2.0 * u * u * u) / sqrt(max(1.0 - u * u * u * u, 0.0001))
        : 0.0;
    float3 normal = normalize(float3(g * slope, 1.0));

    float3 incident = float3(0.0, 0.0, -1.0);
    float3 ray = refract(incident, normal, 1.0 / max(ior, 1.0001));

    float2 shift = float2(0.0);
    if (ray.z < -0.0001) {
        shift = (ray.xy / -ray.z) * thickness * (0.35 + 0.65 * band);
    }

    float3 glass;
    if (dispersion < 0.001) {
        glass = backdrop.eval(coord + shift).rgb;
    } else {
        glass = float3(
            backdrop.eval(coord + shift * (1.0 + dispersion)).r,
            backdrop.eval(coord + shift).g,
            backdrop.eval(coord + shift * (1.0 - dispersion)).b
        );
    }

    glass = mix(glass, tintColor.rgb, tintAmount);

    float2 L = float2(cos(lightAngle), sin(lightAngle));
    float spec = pow(max(dot(g, L), 0.0), 6.0) * band * band;
    glass += spec * 0.85;

    // 边缘：先压一道暗边（白底上才看得见轮廓），再叠高光
    float rim = 1.0 - smoothstep(0.0, 1.8, inward);
    glass *= 1.0 - rim * 0.22;
    glass += rim * 0.25;

    float alpha = 1.0 - smoothstep(-1.0, 0.0, d);
    return half4(glass * alpha, alpha);
}
"#;

// ------------------------------------------------------------ 抓屏 --
/// 抓取屏幕上一个矩形区域，返回 BGRA 像素（alpha 强制 255）。
///
/// 因为窗口设了 WDA_EXCLUDEFROMCAPTURE，抓到的内容里**不含自己**，
/// 所以拿到的就是"玻璃背后的桌面"。
fn capture_screen(x: i32, y: i32, w: i32, h: i32) -> Option<Vec<u8>> {
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        if bmp.is_invalid() {
            ReleaseDC(None, screen);
            return None;
        }
        let old = SelectObject(mem, bmp.into());

        let ok = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY);

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // 自上而下
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            ..Default::default()
        };

        let mut buf = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(
            mem,
            bmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);

        if ok.is_err() || lines == 0 {
            return None;
        }

        // BitBlt 出来的 alpha 是 0，强制不透明
        for i in (3..buf.len()).step_by(4) {
            buf[i] = 255;
        }
        Some(buf)
    }
}

// ------------------------------------------------------------ 渲染一帧 --
/// 把一帧画进 surface。`t` 是秒，用来让背景动起来（证明是实时渲染）。
///
/// `backdrop` 为 None 时画测试图案（离屏调试用）。
fn draw_frame(
    surface: &mut Surface,
    effect: &RuntimeEffect,
    t: f32,
    backdrop: Option<&skia_safe::Image>,
    light_angle: f32,
    blob_pos: (f32, f32),
    blob_r: f32,
    // 只画玻璃不画背景（调试：PNG 的 alpha 就是玻璃轮廓，方便量化验证）
    glass_only: bool,
) {
    // ---- 背景 ----
    // 有真实抓屏就用它；没有就画测试图案（离屏调试用）。
    {
        let canvas = surface.canvas();
        canvas.clear(Color::TRANSPARENT);

        if let Some(img) = backdrop {
            // 直接把桌面截图铺满，作为玻璃的"背景"
            canvas.draw_image(img, (0, 0), None);
        } else if glass_only {
            // 只要玻璃的形状，背景留透明
        } else {
            let colors = [
                Color::from_rgb(255, 59, 107),
                Color::from_rgb(255, 201, 60),
                Color::from_rgb(46, 204, 113),
                Color::from_rgb(74, 125, 255),
                Color::from_rgb(168, 85, 247),
                Color::from_rgb(0, 200, 220),
            ];
            for (i, c) in colors.iter().enumerate() {
                let mut p = Paint::default();
                p.set_color(*c);
                p.set_anti_alias(true);
                let cw = W as f32 / 3.0;
                let ch = H as f32 / 2.0;
                let x = (i as f32 % 3.0) * cw;
                let y = (i as f32 / 3.0) * ch;
                canvas.draw_rect(Rect::from_xywh(x, y, cw, ch), &p);
            }

            // 滚动的细线：折射后扭曲会很明显（只在测试图案下画）
            let mut line = Paint::default();
            line.set_color(Color::WHITE);
            line.set_style(PaintStyle::Stroke);
            line.set_stroke_width(2.0);
            let offset = (t * 40.0) % 22.0;
            let mut y = offset - 22.0;
            while y < H as f32 {
                canvas.draw_line((0.0, y), (W as f32, y), &line);
                y += 22.0;
            }
            // 竖线：折射位移是水平方向的，竖线被掰弯才量得出来
            let mut x = offset - 22.0;
            while x < W as f32 {
                canvas.draw_line((x, 0.0), (x, H as f32), &line);
                x += 22.0;
            }
        }
    } // 借用在这里结束，下面才能 snapshot

    // ---- 玻璃 ----
    let image = surface.image_snapshot();
    let bg_shader = image
        .to_shader(
            Some((TileMode::Clamp, TileMode::Clamp)),
            SamplingOptions::default(),
            None,
        )
        .expect("to_shader");

    let rect_w = 420.0f32;
    let rect_h = 240.0f32;
    // 布局必须和 SkSL 里的 uniform 声明一一对应（含 16 字节对齐的 float4）
    // 0 rectSize | 2 center | 4..11 标量 | 12 blobPos | 14 blobR | 16 tintColor
    let uniform: [f32; 20] = [
        rect_w,
        rect_h,
        W as f32 / 2.0,
        H as f32 / 2.0,
        52.0,           // corner
        38.0,           // bezel：边缘折射带
        34.0,           // thickness：玻璃厚度
        1.42,           // ior
        0.05,           // dispersion
        light_angle,    // 跟手：光源方向 = 光标相对玻璃中心的方位角
        0.10,           // tintAmount
        0.0,            // pad0（对齐用）
        blob_pos.0,     // blobPos：跟手小圆的位置（相对玻璃中心）
        blob_pos.1,
        blob_r,         // blobR：小圆半径，0 = 不融合
        0.0,            // pad1
        0.90,           // tintColor
        0.92,
        0.95,
        1.0,
    ];
    let bytes: Vec<u8> = uniform.iter().flat_map(|f| f.to_le_bytes()).collect();

    if let Some(shader) = effect.make_shader(Data::new_copy(&bytes), &[bg_shader.into()], None) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| eprintln!("[调试] 着色器创建成功, lightAngle={light_angle:.3}"));
        let mut paint = Paint::default();
        paint.set_shader(shader);
        paint.set_anti_alias(true);
        // 绘制区域要比玻璃本体大一圈：融合出的小圆可能在矩形之外，
        // 区域外的像素由着色器返回透明，不会有硬边。
        let pad = 70.0f32;
        let rect = Rect::from_xywh(
            (W as f32 - rect_w) / 2.0 - pad,
            (H as f32 - rect_h) / 2.0 - pad,
            rect_w + pad * 2.0,
            rect_h + pad * 2.0,
        );
        // 重新借一次 canvas 画玻璃
        let canvas = surface.canvas();
        canvas.draw_rect(rect, &paint);
    } else {
        static ONCE2: std::sync::Once = std::sync::Once::new();
        ONCE2.call_once(|| eprintln!("[错误] make_shader 返回 None，uniform {} 字节", bytes.len()));
    }
}

// ------------------------------------------------------- DIB + 传输到窗口 --
/// 一块 32 位、自上而下的 DIB，供 UpdateLayeredWindow 使用。
struct Dib {
    mem_dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: i32,
    h: i32,
}

impl Dib {
    fn new(w: i32, h: i32) -> Self {
        unsafe {
            let screen_dc = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen_dc));
            ReleaseDC(None, screen_dc);

            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // 负数 = 自上而下
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            };

            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(
                Some(mem_dc),
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            )
            .expect("CreateDIBSection 失败");

            let old = SelectObject(mem_dc, bitmap.into());

            Self {
                mem_dc,
                bitmap,
                old,
                bits: bits as *mut u8,
                w,
                h,
            }
        }
    }

    /// 把 BGRA 像素拷进来并提交到窗口
    fn blit(&self, hwnd: HWND, pixels: &[u8]) {
        unsafe {
            let need = (self.w * self.h * 4) as usize;
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.bits, need.min(pixels.len()));

            // 关键：pt_dst 必须用窗口当前的位置，否则 ULW 会把窗口挪到 (0,0)
            let mut rect = windows::Win32::Foundation::RECT::default();
            let _ = GetWindowRect(hwnd, &mut rect);
            let mut pt_dst = POINT {
                x: rect.left,
                y: rect.top,
            };

            let screen_dc = GetDC(None);
            let size = SIZE {
                cx: self.w,
                cy: self.h,
            };
            let pt_src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let r = UpdateLayeredWindow(
                hwnd,
                Some(screen_dc),
                Some(&mut pt_dst),
                Some(&size),
                Some(self.mem_dc),
                Some(&pt_src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            if let Err(e) = r {
                static ONCE: std::sync::Once = std::sync::Once::new();
                ONCE.call_once(|| {
                    eprintln!("[错误] UpdateLayeredWindow 失败: {e:?} (code {})", e.code().0);
                    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    eprintln!("[调试] exstyle = 0x{ex:X}, LAYERED位 = {}", (ex & WS_EX_LAYERED.0 as isize) != 0);
                });
            }
            ReleaseDC(None, screen_dc);
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem_dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.mem_dc);
        }
    }
}

// ------------------------------------------------------------ 位置记忆 --
/// 配置文件放在 exe 同级目录（绿色软件，卸载时删目录即可）
fn pos_file() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("window-pos.txt")))
        .unwrap_or_else(|| std::path::PathBuf::from("window-pos.txt"))
}

/// 崩溃日志（GUI 子系统没有控制台，panic 不写文件就彻底没线索）
fn log_file() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("liquid-glass-error.log")))
        .unwrap_or_else(|| std::path::PathBuf::from("liquid-glass-error.log"))
}

/// 装一个 panic 钩子：把崩溃信息写到 exe 同级目录
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!(
            "[{}] PANIC: {info}\n",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
        let _ = fs::write(log_file(), msg);
    }));
}

fn load_pos() -> Option<(i32, i32)> {
    let text = fs::read_to_string(pos_file()).ok()?;
    let mut parts = text.trim().split(',');
    let x: i32 = parts.next()?.trim().parse().ok()?;
    let y: i32 = parts.next()?.trim().parse().ok()?;
    // 简单合法性检查：别恢复到屏幕外面去
    if x.abs() > 10000 || y.abs() > 10000 {
        return None;
    }
    Some((x, y))
}

fn save_pos(hwnd: HWND) {    if hwnd.is_invalid() {
        return;
    }
    unsafe {
        let mut rect = windows::Win32::Foundation::RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok() {
            let _ = fs::write(pos_file(), format!("{},{}", rect.left, rect.top));
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        save_pos(self.hwnd);
    }
}

// ------------------------------------------------------------- 窗口应用 --
struct App {
    window: Option<std::sync::Arc<Window>>,
    effect: Option<RuntimeEffect>,
    dib: Option<Dib>,
    hwnd: HWND,
    start: Instant,
    /// 复用的 Skia 画布（避免每帧重新分配）
    surface: Option<Surface>,
    /// 复用的像素缓冲
    buf: Vec<u8>,
    /// 是否处于点击穿透状态
    click_through: bool,
    /// 上一帧的按键状态，用于边沿检测
    prev_esc: bool,
    prev_t: bool,
    /// 调试：把渲染帧存到这个路径，然后退出
    dump_path: Option<String>,
    frames: u32,
    /// 跟手小圆（液态融合）：当前半径和位置，插值出来的所以有"甩"的惯性
    blob_r: f32,
    blob_pos: (f32, f32),
    /// 上一帧的时间，用来算 dt（让插值和帧率无关）
    last_frame: Instant,
    /// 只画玻璃不画背景（调试）
    glass_only: bool,
    /// 调试计数器
    dbg_n: u32,
    /// 调试：强制指定融合小圆的位置和半径（不受鼠标影响）
    blob_override: Option<(f32, f32, f32)>,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            effect: None,
            dib: None,
            hwnd: HWND(std::ptr::null_mut()),
            start: Instant::now(),
            surface: None,
            buf: Vec::new(),
            click_through: false,
            prev_esc: false,
            prev_t: false,
            dump_path: None,
            frames: 0,
            blob_r: 0.0,
            blob_pos: (0.0, 0.0),
            last_frame: Instant::now(),
            glass_only: false,
            dbg_n: 0,
            blob_override: None,
        }
    }

    /// 切换点击穿透（WS_EX_TRANSPARENT）
    fn toggle_click_through(&mut self) {
        self.click_through = !self.click_through;
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let new = if self.click_through {
                ex | WS_EX_TRANSPARENT.0 as isize
            } else {
                ex & !(WS_EX_TRANSPARENT.0 as isize)
            };
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, new);
        }
    }

    /// 用 GetAsyncKeyState 轮询按键，不受窗口焦点影响
    /// （穿透模式下窗口拿不到焦点，普通键盘事件收不到）
    fn poll_keys(&mut self) -> bool {
        unsafe {
            let esc = (GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8001) != 0;
            let t = (GetAsyncKeyState(VK_T.0 as i32) as u16 & 0x8001) != 0;

            let mut quit = false;
            if esc && !self.prev_esc {
                quit = true;
            }
            if t && !self.prev_t {
                self.toggle_click_through();
                // 给个视觉反馈：标题栏（这里没有）→ 用控制台输出不了，
                // 所以靠手感；穿透后鼠标直接穿过窗口
            }
            self.prev_esc = esc;
            self.prev_t = t;
            quit
        }
    }

    /// 光标相对玻璃中心的方位角（弧度），用于跟手高光
    /// 光标相对玻璃中心的位置（surface 像素坐标，y 向下）
    fn cursor_local(&self) -> (f32, f32) {
        let (pt, rect) = self.cursor_and_rect();
        let cx = pt.x - rect.left;
        let cy = pt.y - rect.top;
        (
            cx as f32 - W as f32 / 2.0,
            cy as f32 - H as f32 / 2.0,
        )
    }

    /// 原始的光标屏幕坐标和窗口矩形（调试用）
    fn cursor_and_rect(&self) -> (POINT, windows::Win32::Foundation::RECT) {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let mut rect = windows::Win32::Foundation::RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        (pt, rect)
    }

    fn light_angle(&self) -> f32 {
        let (dx, dy) = self.cursor_local();
        dy.atan2(dx)
    }

    /// 跟手小圆：光标靠近玻璃边缘时，玻璃朝光标鼓起一个包。
    ///
    /// 位置和半径都做插值，所以快速移动光标时会有液态的甩尾。
    fn update_blob(&mut self, dt: f32) {
        // 调试覆盖：--blob x,y,r 直接指定融合小圆，不受真实鼠标影响
        if let Some((x, y, r)) = self.blob_override {
            self.blob_pos = (x, y);
            self.blob_r = r;
            return;
        }
        let (cx, cy) = self.cursor_local();
        let half_w = 210.0f32;
        let half_h = 120.0f32;
        let len = (cx * cx + cy * cy).sqrt();

        let mut bx = 0.0f32;
        let mut by = 0.0f32;
        let mut target = 0.0f32;

        if len > 1.0 {
            let dir_x = cx / len;
            let dir_y = cy / len;
            // 从中心沿光标方向射到矩形边界
            let tx = if dir_x.abs() > 1e-4 {
                half_w / dir_x.abs()
            } else {
                f32::MAX
            };
            let ty = if dir_y.abs() > 1e-4 {
                half_h / dir_y.abs()
            } else {
                f32::MAX
            };
            let t = tx.min(ty);
            bx = dir_x * t;
            by = dir_y * t;

            let outside = len - t; // >0 表示光标在玻璃外面
            let dist = ((cx - bx).powi(2) + (cy - by).powi(2)).sqrt();
            if outside > -6.0 {
                // 越贴近边缘包越大，离远了就收回去
                target = 46.0 * (1.0 - (dist - 10.0) / 120.0).clamp(0.0, 1.0);
            }
        }

        let k = (dt * 9.0).min(1.0);
        self.blob_r += (target - self.blob_r) * k;
        self.blob_pos.0 += (bx - self.blob_pos.0) * k;
        self.blob_pos.1 += (by - self.blob_pos.1) * k;
        if self.blob_r < 0.4 {
            self.blob_r = 0.0;
        }

        // 调试日志只在 debug 构建里打，release 版本保持安静
        self.dbg_n += 1;
        if cfg!(debug_assertions) && self.dbg_n % 20 == 1 {
            let (pt, rect) = self.cursor_and_rect();
            eprintln!(
                "[blob] 光标=({},{}) 窗口=({},{}) 局部=({:.0},{:.0}) 目标r={:.1} 当前r={:.1} 位置=({:.0},{:.0})",
                pt.x, pt.y, rect.left, rect.top, cx, cy,
                target, self.blob_r, self.blob_pos.0, self.blob_pos.1
            );
        }
    }

    fn draw(&mut self) {
        // 跟手小圆：按 dt 插值，帧率无关
        // （必须放在下面的不可变借用之前，否则 self 借出冲突）
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        self.update_blob(dt);

        let (Some(_window), Some(effect), Some(dib)) = (&self.window, &self.effect, &self.dib) else {
            return;
        };
        let t = self.start.elapsed().as_secs_f32();
        let angle = self.light_angle();

        // 1) 抓取窗口背后的桌面（自己已被 WDA_EXCLUDEFROMCAPTURE 排除）
        let mut rect = windows::Win32::Foundation::RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        // --glass-only：跳过抓屏，PNG 的 alpha 就是玻璃轮廓
        let glass_only = self.glass_only;
        let backdrop_img = if glass_only {
            None
        } else {
            capture_screen(rect.left, rect.top, W, H).and_then(|pixels| {
                let info = skia_safe::ImageInfo::new(
                    (W, H),
                    skia_safe::ColorType::BGRA8888,
                    skia_safe::AlphaType::Premul,
                    None,
                );
                skia_safe::images::raster_from_data(&info, Data::new_copy(&pixels), (W * 4) as usize)
            })
        };

        // 2) 渲染一帧（复用 surface，不每帧重新分配）
        let surface = self
            .surface
            .get_or_insert_with(|| surfaces::raster_n32_premul((W, H)).expect("surface"));
        draw_frame(
            surface,
            effect,
            t,
            backdrop_img.as_ref(),
            angle,
            self.blob_pos,
            self.blob_r,
            glass_only,
        );

        // 用 read_pixels 取出来（peek_pixels 在这个版本拿不到数据）
        let info = skia_safe::ImageInfo::new(
            (W, H),
            skia_safe::ColorType::BGRA8888,
            skia_safe::AlphaType::Premul,
            None,
        );
        // 复用缓冲，避免每帧 vec![0u8; ...] 分配
        if self.buf.len() != (W * H * 4) as usize {
            self.buf = vec![0u8; (W * H * 4) as usize];
        }
        let ok = surface.read_pixels(&info, &mut self.buf, (W * 4) as usize, (0, 0));

        if ok {
            dib.blit(self.hwnd, &self.buf);
        }

        // 调试：跑够若干帧后把渲染结果存盘（绕过截屏，看到的就是真实渲染）
        if let Some(path) = &self.dump_path {
            self.frames += 1;
            if self.frames >= 60 {
                if let Some(surface) = &mut self.surface {
                    let img = surface.image_snapshot();
                    if let Some(data) = img.encode(None, EncodedImageFormat::PNG, 100) {
                        let _ = fs::write(path, data.as_bytes());
                        println!("已保存渲染帧 -> {path}");
                    }
                }
                // process::exit 不会跑 Drop，所以这里手动存一次位置
                save_pos(self.hwnd);
                std::process::exit(0);
            }
        }

        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title("Liquid Glass")
            .with_inner_size(LogicalSize::new(W as f64, H as f64))
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop);

        let window = std::sync::Arc::new(event_loop.create_window(attrs).expect("创建窗口失败"));

        // 取 HWND
        let handle = window.window_handle().expect("拿不到窗口句柄");
        let hwnd = match handle.as_raw() {
            RawWindowHandle::Win32(h) => HWND(h.hwnd.get() as *mut c_void),
            _ => panic!("不是 Win32 窗口"),
        };

        unsafe {
            // 加 layered + toolwindow（不进任务栏）
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                ex | WS_EX_LAYERED.0 as isize | WS_EX_TOOLWINDOW.0 as isize,
            );

            // 关键：把自己排除在屏幕捕获之外，否则抓背景会抓到自己的玻璃（无限套娃）
            // 调试时用 --no-exclude 关掉，这样才能截到窗口本身来验证渲染
            let no_exclude = std::env::args().any(|a| a == "--no-exclude");
            if no_exclude {
                println!("[调试] 已跳过 WDA_EXCLUDEFROMCAPTURE（截图可以看到窗口）");
            } else {
                let ok = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
                if ok.is_err() {
                    eprintln!("[警告] SetWindowDisplayAffinity 失败，抓背景会出现自反馈");
                } else {
                    println!("已设置 WDA_EXCLUDEFROMCAPTURE（本窗口不进截图）");
                }
            }
        }

        let effect = RuntimeEffect::make_for_shader(LIQUID_SKSL, None)
            .unwrap_or_else(|e| panic!("SkSL 编译失败: {e}"));

        self.hwnd = hwnd;
        self.dib = Some(Dib::new(W, H));
        self.effect = Some(effect);
        self.window = Some(window);
        self.start = Instant::now();

        // 恢复上次的位置（首次运行就用默认位置）
        if let Some((x, y)) = load_pos() {
            unsafe {
                let _ = SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
            println!("已恢复位置 ({x}, {y})");
        }

        println!("窗口已创建  hwnd={:?}", hwnd);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            // 左键按下 → 拖动窗口（winit 的系统级拖动，比手动 SetWindowPos 顺滑）
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => match button {
                MouseButton::Left => {
                    // 穿透状态下不该能拖动（鼠标事件本来就到不了窗口，这里加个保险）
                    if !self.click_through {
                        if let Some(w) = &self.window {
                            let _ = w.drag_window();
                        }
                    }
                }
                // 右键 = 退出（无边框窗口给个退路）
                MouseButton::Right => event_loop.exit(),
                _ => {}
            },
            WindowEvent::KeyboardInput { event, .. } => {
                // 按 ESC 退出，免得无边框窗口关不掉
                if event.logical_key
                    == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
                {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // 轮询按键（穿透模式下窗口没焦点，收不到普通键盘事件）
        if self.window.is_some() && self.poll_keys() {
            event_loop.exit();
            return;
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

// ------------------------------------------------------------------ 入口 --
fn render_offscreen() {
    let effect = RuntimeEffect::make_for_shader(LIQUID_SKSL, None)
        .unwrap_or_else(|e| panic!("SkSL 编译失败: {e}"));
    let mut surface = surfaces::raster_n32_premul((W, H)).expect("surface");
    draw_frame(&mut surface, &effect, 0.0, None, 2.2, (0.0, 0.0), 0.0, false);

    let image = surface.image_snapshot();
    let data = image
        .encode(None, EncodedImageFormat::PNG, 100)
        .expect("PNG 编码失败");
    let path = "liquid-glass-out.png";
    fs::write(path, data.as_bytes()).expect("写文件失败");
    println!("OK -> {path}");
}

fn main() {
    install_panic_hook();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--offscreen") {
        render_offscreen();
        return;
    }

    let event_loop = EventLoop::new().expect("事件循环创建失败");
    let mut app = App::new();

    // 调试：--dump-frame <path> 跑 60 帧后把渲染结果存盘退出
    if let Some(i) = args.iter().position(|a| a == "--dump-frame") {
        if let Some(p) = args.get(i + 1) {
            app.dump_path = Some(p.clone());
        }
    }
    // 调试：--glass-only 只画玻璃不画背景（PNG 的 alpha 就是玻璃轮廓）
    if args.iter().any(|a| a == "--glass-only") {
        app.glass_only = true;
    }
    // 调试：--blob x,y,r 强制指定融合小圆（不受真实鼠标干扰，方便自动化验证）
    if let Some(i) = args.iter().position(|a| a == "--blob") {
        if let Some(v) = args.get(i + 1) {
            let nums: Vec<f32> = v
                .split(',')
                .filter_map(|s| s.trim().parse::<f32>().ok())
                .collect();
            if nums.len() == 3 {
                app.blob_override = Some((nums[0], nums[1], nums[2]));
            }
        }
    }

    event_loop.run_app(&mut app).expect("运行失败");
}

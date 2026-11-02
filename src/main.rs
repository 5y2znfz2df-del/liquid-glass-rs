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

use skia_safe::{
    surfaces, Color, Data, EncodedImageFormat, Paint, PaintStyle, Rect, RRect, RuntimeEffect,
    SamplingOptions, Surface, TileMode,
};
use std::ffi::c_void;
use std::fs;
use std::time::Instant;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP,
    HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, GetWindowRect, SetWindowLongPtrW, UpdateLayeredWindow, GWL_EXSTYLE, ULW_ALPHA,
    WS_EX_LAYERED, WS_EX_TOOLWINDOW,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
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
uniform float4 tintColor;

float sdRoundRect(float2 p, float2 halfSize, float r, out float2 g) {
    float2 sgn = float2(p.x < 0.0 ? -1.0 : 1.0, p.y < 0.0 ? -1.0 : 1.0);
    float2 q = abs(p) - halfSize + r;
    if (q.x > 0.0 && q.y > 0.0) {
        float2 m = q / r;
        float len = length(m);
        g = (m / max(len, 0.0001)) * sgn;
        return (len - 1.0) * r;
    }
    g = q.x > q.y ? float2(sgn.x, 0.0) : float2(0.0, sgn.y);
    return max(q.x, q.y) - r;
}

half4 main(float2 coord) {
    float2 halfSize = rectSize * 0.5;
    float2 local = coord - center;

    float2 g;
    float d = sdRoundRect(local, halfSize, corner, g);
    if (d > 0.0) { return half4(0.0); }

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

    float rim = 1.0 - smoothstep(0.0, 1.6, inward);
    glass += rim * 0.25;

    float alpha = 1.0 - smoothstep(-1.0, 0.0, d);
    return half4(glass * alpha, alpha);
}
"#;

// ------------------------------------------------------------ 渲染一帧 --
/// 把一帧画进 surface。`t` 是秒，用来让背景动起来（证明是实时渲染）。
fn draw_frame(surface: &mut Surface, effect: &RuntimeEffect, t: f32) {
    // ---- 背景（模拟"窗口后面的桌面"）----
    // 里程碑 3 会用真实桌面截图替换这块。
    {
        let canvas = surface.canvas();
        canvas.clear(Color::TRANSPARENT);

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

        // 滚动的细线：折射后扭曲会很明显
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
    let uniform: [f32; 16] = [
        rect_w,
        rect_h,
        W as f32 / 2.0,
        H as f32 / 2.0,
        48.0,
        30.0,
        40.0,
        1.42,
        0.04,
        2.2,
        0.08,
        0.0,
        0.92,
        0.95,
        0.98,
        1.0,
    ];
    let bytes: Vec<u8> = uniform.iter().flat_map(|f| f.to_le_bytes()).collect();

    if let Some(shader) = effect.make_shader(Data::new_copy(&bytes), &[bg_shader.into()], None) {
        let mut paint = Paint::default();
        paint.set_shader(shader);
        paint.set_anti_alias(true);
        let rect = Rect::from_xywh(
            (W as f32 - rect_w) / 2.0,
            (H as f32 - rect_h) / 2.0,
            rect_w,
            rect_h,
        );
        // 重新借一次 canvas 画玻璃
        let canvas = surface.canvas();
        canvas.draw_rrect(RRect::new_rect_xy(rect, 48.0, 48.0), &paint);
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

// ------------------------------------------------------------- 窗口应用 --
struct App {
    window: Option<std::sync::Arc<Window>>,
    effect: Option<RuntimeEffect>,
    dib: Option<Dib>,
    hwnd: HWND,
    start: Instant,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            effect: None,
            dib: None,
            hwnd: HWND(std::ptr::null_mut()),
            start: Instant::now(),
        }
    }

    fn draw(&mut self) {
        let (Some(window), Some(effect), Some(dib)) = (&self.window, &self.effect, &self.dib) else {
            return;
        };
        let t = self.start.elapsed().as_secs_f32();

        let mut surface = surfaces::raster_n32_premul((W, H)).expect("surface");
        draw_frame(&mut surface, effect, t);

        // 用 read_pixels 取出来（peek_pixels 在这个版本拿不到数据）
        let info = skia_safe::ImageInfo::new(
            (W, H),
            skia_safe::ColorType::BGRA8888,
            skia_safe::AlphaType::Premul,
            None,
        );
        let mut buf = vec![0u8; (W * H * 4) as usize];
        let ok = surface.read_pixels(&info, &mut buf, (W * 4) as usize, (0, 0));

        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| eprintln!("[调试] 第一次 draw: read_pixels={ok}"));

        if ok {
            dib.blit(self.hwnd, &buf);
        }

        window.request_redraw();
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
        }

        let effect = RuntimeEffect::make_for_shader(LIQUID_SKSL, None)
            .unwrap_or_else(|e| panic!("SkSL 编译失败: {e}"));

        self.hwnd = hwnd;
        self.dib = Some(Dib::new(W, H));
        self.effect = Some(effect);
        self.window = Some(window);
        self.start = Instant::now();

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

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
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
    draw_frame(&mut surface, &effect, 0.0);

    let image = surface.image_snapshot();
    let data = image
        .encode(None, EncodedImageFormat::PNG, 100)
        .expect("PNG 编码失败");
    let path = "liquid-glass-out.png";
    fs::write(path, data.as_bytes()).expect("写文件失败");
    println!("OK -> {path}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--offscreen") {
        render_offscreen();
        return;
    }

    let event_loop = EventLoop::new().expect("事件循环创建失败");
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("运行失败");
}

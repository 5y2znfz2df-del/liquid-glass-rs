// 液态玻璃番茄钟
//
// 玻璃不是装饰：它就是番茄钟的外壳。数字、进度条、状态都画在玻璃上，
// 玻璃后面是真实桌面（每帧抓屏），所以桌面上的东西会被折射掰弯。
//
// 透明方案：WS_EX_LAYERED + UpdateLayeredWindow（逐像素 alpha）。
//
// 用法：
//   cargo run                 → 开窗口（番茄钟）
//   cargo run -- --offscreen  → 只渲染一张 PNG
//
// 许可证：GPLv3（见 LICENSE）。着色器为原创实现，未复制他人代码。

// 不弹控制台窗口（否则会盖在玻璃上面）
#![windows_subsystem = "windows"]

mod timer;

use skia_safe::{
    surfaces, Color, Data, EncodedImageFormat, Font, Paint, PaintStyle, Rect, RuntimeEffect,
    SamplingOptions, Surface, TileMode,
};use std::ffi::c_void;
use std::fs;
use std::time::Instant;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject,
    GetDC, GetDIBits, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
};
use windows::Win32::System::Diagnostics::Debug::Beep;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_ESCAPE, VK_R, VK_S, VK_SHIFT, VK_SPACE, VK_T,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, SetWindowDisplayAffinity,
    SetWindowLongPtrW, SetWindowPos, UpdateLayeredWindow, GWL_EXSTYLE, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER, ULW_ALPHA, WDA_EXCLUDEFROMCAPTURE, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId, WindowLevel};

/// 窗口尺寸
const W: i32 = 428;
const H: i32 = 248;
/// 抓屏时窗口四周多抓多少像素。窗口在这么多像素内移动都不用重抓，
/// 只靠 backdropOffset 平移就能保证画面跟窗口对齐（拖动不割裂、不掉帧）。
const CAP_MARGIN: i32 = 120;
/// 鼠标离开多少秒后自动收起到屏幕边缘
const EDGE_HIDE_SECS: f32 = 5.0;
/// 收起后还露在外面的宽度（够鼠标碰到就行）
const EDGE_PEEK: i32 = 10;

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
uniform float4 tintColor2;
uniform float2 backdropOffset;

// 圆角矩形的有符号距离场
float sdRoundRect(float2 p, float2 halfSize, float r) {
    float2 q = abs(p) - halfSize + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

// 整个玻璃的形状 = 一个圆角矩形
float sceneD(float2 p) {
    return sdRoundRect(p, rectSize * 0.5, corner);
}

half4 main(float2 coord) {
    float2 local = coord - center;

    float d = sceneD(local);
    if (d > 0.0) { return half4(0.0); }

    float inward = -d;
    float band = clamp(inward / max(bezel, 0.001), 0.0, 1.0);

    // 形状可能是两个图形融出来的，没有解析梯度，用差分求法线。
    //
    // 两个省时间的点：
    // 1) 只有边缘带（band < 1）才用得到法线 —— 里面 slope=0，法线乘上去也是 0
    // 2) 用前向差分（2 次 SDF）而不是中心差分（4 次 SDF）
    float2 g = float2(0.0, 1.0);
    if (band < 1.0) {
        float e = 1.0;
        float2 grad = float2(
            sceneD(local + float2(e, 0.0)) - d,
            sceneD(local + float2(0.0, e)) - d
        );
        float glen = length(grad);
        // 中轴线上梯度会退化，这时退回矩形的解析方向
        float2 gRect = normalize(float2(
            local.x / max(rectSize.x * 0.5, 1.0),
            local.y / max(rectSize.y * 0.5, 1.0)
        ) + float2(1e-5, 1e-5));
        g = glen > 0.002 ? grad / glen : gRect;
    }

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
    // 背景是"带余量"抓的：抓屏原点和窗口位置不同，采样时要加上这个偏移。
    // 这样窗口移动时画面永远跟窗口对齐，不会出现割裂的错位切片。
    float2 bo = backdropOffset;
    if (dispersion < 0.001) {
        glass = backdrop.eval(coord + shift + bo).rgb;
    } else {
        glass = float3(
            backdrop.eval(coord + shift * (1.0 + dispersion) + bo).r,
            backdrop.eval(coord + shift + bo).g,
            backdrop.eval(coord + shift * (1.0 - dispersion) + bo).b
        );
    }

    // 染色用竖向渐变：上端近黑，下端偏蓝灰
    float gy = clamp((local.y + rectSize.y * 0.5) / max(rectSize.y, 1.0), 0.0, 1.0);
    float4 tint = mix(tintColor, tintColor2, gy);
    glass = mix(glass, tint.rgb, tintAmount);

    // 高光是"玻璃边缘被掰弯的那一圈"反射出来的，只能出现在边缘带。
    // edge: 边缘处 1，往里走迅速衰减到 0。
    //
    // ⚠️ 这里原来写的是 `* band * band`，而 band 在玻璃内部恒等于 1 ——
    // 等于内部高光完全没衰减。光标在玻璃下方时光线方向正好对上内部占位法线
    // (0,1)，整个内部就被灌满白光（整窗变白）。改成按"离边缘多远"衰减。
    float2 L = float2(cos(lightAngle), sin(lightAngle));
    float edge = 1.0 - band;
    float spec = pow(max(dot(g, L), 0.0), 6.0) * edge * edge;
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
/// 抓取屏幕上一个矩形区域，BGRA 像素写进 `buf`（alpha 强制 255）。
///
/// 因为窗口设了 WDA_EXCLUDEFROMCAPTURE，抓到的内容里**不含自己**，
/// 所以拿到的就是"玻璃背后的桌面"。
///
/// `buf` 由调用方复用，避免每帧重新分配 1MB。
fn capture_screen(x: i32, y: i32, w: i32, h: i32, buf: &mut Vec<u8>) -> Option<()> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let need = (w * h * 4) as usize;
    if buf.len() != need {
        buf.resize(need, 0);
    }
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let mem = CreateCompatibleDC(Some(screen));
        if mem.is_invalid() {
            ReleaseDC(None, screen);
            return None;
        }
        let bmp = CreateCompatibleBitmap(screen, w, h);
        if bmp.is_invalid() {
            // 这里必须把 mem 一起释放，否则每次失败都漏一个 DC
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            return None;
        }
        let old = SelectObject(mem, bmp.into());

        let t0 = Instant::now();
        let ok = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY);
        let ms_blt = t0.elapsed().as_secs_f32() * 1000.0;

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

        let t1 = Instant::now();
        let lines = GetDIBits(
            mem,
            bmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        let ms_dib = t1.elapsed().as_secs_f32() * 1000.0;
        if std::env::var_os("LG_DEBUG_CAPTURE").is_some() {
            eprintln!("[抓屏] BitBlt {ms_blt:.1} ms | GetDIBits {ms_dib:.1} ms");
        }

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
        Some(())
    }
}

// ------------------------------------------------------- 玻璃上面的文字 --
/// 番茄钟的字体。数字用等宽（Consolas Bold），这样 25:00 → 24:59 时
/// 字形宽度不变，字不会左右跳。
struct Overlay {
    digits: Font,
    label: Font,
    hint: Font,
}

impl Overlay {
    fn load() -> Option<Self> {
        let fm = skia_safe::FontMgr::new();
        let bold = skia_safe::FontStyle::bold();
        let normal = skia_safe::FontStyle::normal();
        // 数字用等宽粗体（Consolas），换字不左右跳；中文用微软雅黑
        let digits_face = fm
            .match_family_style("Consolas", bold)
            .or_else(|| fm.match_family_style("Segoe UI", bold))
            .or_else(|| fm.legacy_make_typeface(None, bold))?;
        let label_face = fm
            .match_family_style("Microsoft YaHei", normal)
            .or_else(|| fm.match_family_style("Segoe UI", normal))
            .or_else(|| fm.legacy_make_typeface(None, normal))?;
        let hint_face = label_face.clone();
        Some(Self {
            digits: Font::new(digits_face, 88.0),
            label: Font::new(label_face, 21.0),
            hint: Font::new(hint_face, 12.5),
        })
    }
}

/// 居中画一行字，带一层暗色阴影（不管背后是亮桌面还是暗桌面都读得清）
fn draw_centered(
    canvas: &skia_safe::Canvas,
    text: &str,
    cx: f32,
    baseline: f32,
    font: &Font,
    alpha: u8,
) {
    // 阴影
    let mut shadow = Paint::default();
    shadow.set_anti_alias(true);
    shadow.set_color(Color::from_argb(150, 0, 0, 0));
    let (w, _) = font.measure_str(text, Some(&shadow));
    canvas.draw_str(text, (cx - w / 2.0 + 1.0, baseline + 2.0), font, &shadow);

    // 正文
    let mut p = Paint::default();
    p.set_anti_alias(true);
    p.set_color(Color::from_argb(alpha, 255, 255, 255));
    let (w, _) = font.measure_str(text, Some(&p));
    canvas.draw_str(text, (cx - w / 2.0, baseline), font, &p);
}

/// 在玻璃上画番茄钟：大数字 + 副标题 + 进度条 + 按键提示
fn draw_overlay(
    canvas: &skia_safe::Canvas,
    overlay: &Overlay,
    pomo: &timer::Pomodoro,
    glass_rect: Rect,
) {
    let cx = glass_rect.center_x();
    // 玻璃高度 240，从上往下排：数字 / 副标题 / 进度条 / 提示
    let top = glass_rect.top();

    draw_centered(canvas, &pomo.text(), cx, top + 128.0, &overlay.digits, 245);
    draw_centered(canvas, &pomo.subtitle(), cx, top + 168.0, &overlay.label, 210);

    // 进度条：底槽 + 已用部分
    let bar_w = 232.0f32;
    let bar_h = 4.0f32;
    let bar_x = cx - bar_w / 2.0;
    let bar_y = top + 190.0;
    let mut track = Paint::default();
    track.set_anti_alias(true);
    track.set_color(Color::from_argb(60, 255, 255, 255));
    canvas.draw_rrect(
        skia_safe::RRect::new_rect_xy(
            Rect::from_xywh(bar_x, bar_y, bar_w, bar_h),
            bar_h / 2.0,
            bar_h / 2.0,
        ),
        &track,
    );
    let p = pomo.progress();
    if p > 0.001 {
        let mut fill = Paint::default();
        fill.set_anti_alias(true);
        fill.set_color(if pomo.phase.is_break() {
            Color::from_argb(235, 150, 235, 190) // 休息：薄荷绿
        } else {
            Color::from_argb(235, 255, 255, 255) // 专注：白色
        });
        canvas.draw_rrect(
            skia_safe::RRect::new_rect_xy(
                Rect::from_xywh(bar_x, bar_y, (bar_w * p).max(bar_h), bar_h),
                bar_h / 2.0,
                bar_h / 2.0,
            ),
            &fill,
        );
    }

    // 按键提示
    let hint = if pomo.running {
        "空格 暂停    R 重置    S 跳过    T 穿透"
    } else {
        "空格 开始    R 重置    S 跳过    T 穿透"
    };
    draw_centered(canvas, hint, cx, top + 218.0, &overlay.hint, 150);
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
    // 番茄钟状态（画在玻璃上面）
    pomo: &timer::Pomodoro,
    overlay: Option<&Overlay>,
    // 抓屏原点和窗口位置的差（surface 像素）。背景是带余量抓的，
    // 采样时要加这个偏移才能跟窗口对齐。
    backdrop_offset: (f32, f32),
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

    // ⚠️ 关键：快照拿到之后再清空画布。
    // 抓来的桌面只能当"着色器的输入"，不能留在画布上 ——
    // 否则玻璃外面会露出一整块**没经过折射**的旧桌面
    // （看起来就是一块错位的图，也就是"边框外不正确"）。
    // image_snapshot 是不可变快照，之后 clear 会触发 copy-on-write，快照仍然有效。
    surface.canvas().clear(Color::TRANSPARENT);

    let rect_w = 420.0f32;
    let rect_h = 240.0f32;
    // 布局必须和 SkSL 里的 uniform 声明一一对应（含 16 字节对齐的 float4）
    // 0 rectSize | 2 center | 4..11 标量 | 12 tintColor | 16 tintColor2 | 20 backdropOffset
    // （共 22 个 float = 88 字节）
    let uniform: [f32; 22] = [
        rect_w,
        rect_h,
        W as f32 / 2.0,
        H as f32 / 2.0,
        46.0,           // corner
        26.0,           // bezel：边缘折射带（调窄了，边框更细）
        34.0,           // thickness：玻璃厚度
        1.42,           // ior
        0.05,           // dispersion
        light_angle,    // 跟手：光源方向 = 光标相对玻璃中心的方位角
        0.70,           // tintAmount：黑玻璃，染得重才能压成黑色
        0.0,            // pad0（对齐用）
        // tintColor：渐变上端，近黑
        0.020,
        0.022,
        0.030,
        1.0,
        // tintColor2：渐变下端，偏蓝灰
        0.095,
        0.110,
        0.140,
        1.0,
        // backdropOffset
        backdrop_offset.0,
        backdrop_offset.1,
    ];
    let bytes: Vec<u8> = uniform.iter().flat_map(|f| f.to_le_bytes()).collect();

    if let Some(shader) = effect.make_shader(Data::new_copy(&bytes), &[bg_shader.into()], None) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            // 自检：Skia 认为的 uniform 布局必须和我们的 20 个 f32（80 字节）对上，
            // 否则 tintColor 会被挤到错的位置（表现为玻璃染色异常）
            let us = effect.uniforms();
            let total = us
                .iter()
                .map(|u| u.offset() + u.size_in_bytes())
                .max()
                .unwrap_or(0);
            let mut layout = String::new();
            for u in us {
                layout.push_str(&format!(" {}=@{}+{}", u.name(), u.offset(), u.size_in_bytes()));
            }
            eprintln!("[调试] 着色器创建成功, lightAngle={light_angle:.3}");
            eprintln!("[调试] uniform 布局 (共 {total} 字节):{layout}");
            let expect = uniform.len() * 4;
            if total != expect {
                eprintln!("[警告] uniform 布局对不上（期望 {expect} 字节）！着色器读到的参数会错位");
            }
        });
        let mut paint = Paint::default();
        paint.set_shader(shader);
        // 边缘是着色器自己算的平滑 alpha，不需要几何抗锯齿；
        // 而且只能画玻璃本体这一块（外面一律透明），否则会盖住不该盖的地方。
        paint.set_anti_alias(false);
        // pad 只留 2px 给边缘那 1.8px 的轮廓过渡，别再往外扩了 ——
        // 扩多少就多画多少像素，这是每帧渲染时间的直接来源。
        let pad = 2.0f32;
        let rect = Rect::from_xywh(
            (W as f32 - rect_w) / 2.0 - pad,
            (H as f32 - rect_h) / 2.0 - pad,
            rect_w + pad * 2.0,
            rect_h + pad * 2.0,
        );
        // 重新借一次 canvas 画玻璃
        let canvas = surface.canvas();
        canvas.draw_rect(rect, &paint);

        // ---- 番茄钟的文字（画在玻璃之上，不参与折射，保证读得清）----
        if let Some(ov) = overlay {
            let glass = Rect::from_xywh(
                (W as f32 - rect_w) / 2.0,
                (H as f32 - rect_h) / 2.0,
                rect_w,
                rect_h,
            );
            draw_overlay(canvas, ov, pomo, glass);
        }
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

/// 把窗口位置写进记忆文件。传 `Some((x,y))` 就直接用这个位置，
/// 传 None 就读当前窗口位置。
fn save_pos_at(hwnd: HWND, at: Option<(i32, i32)>) {
    let pos = match at {
        Some(p) => Some(p),
        None => {
            if hwnd.is_invalid() {
                return;
            }
            unsafe {
                let mut rect = windows::Win32::Foundation::RECT::default();
                if GetWindowRect(hwnd, &mut rect).is_ok() {
                    Some((rect.left, rect.top))
                } else {
                    None
                }
            }
        }
    };
    if let Some((x, y)) = pos {
        let _ = fs::write(pos_file(), format!("{x},{y}"));
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // 收在屏幕边缘时不能记那个位置，要记"家的位置"，否则下次启动就藏在边缘了
        save_pos_at(self.hwnd, Some(self.rest_pos));
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
    prev_space: bool,
    prev_r: bool,
    prev_s: bool,
    /// 番茄钟
    pomo: timer::Pomodoro,
    /// 玻璃上的字体（加载失败就只显示玻璃）
    overlay: Option<Overlay>,
    /// 调试：把渲染帧存到这个路径，然后退出
    dump_path: Option<String>,
    frames: u32,
    /// 跟手小圆（液态融合）：当前半径和位置，插值出来的所以有"甩"的惯性
    /// 上一帧的时间，用来算 dt（让插值和帧率无关）
    last_frame: Instant,
    /// 只画玻璃不画背景（调试）
    glass_only: bool,
    /// 调试计数器
    /// debug：上一帧打印过的 (抓屏原点, 是否重抓)，用来只在变化时打日志
    dbg_cap: ((i32, i32), bool),
    /// 调试：强制指定融合小圆的位置和半径（不受鼠标影响）
    /// 抓屏缓冲，复用避免每帧分配
    cap_buf: Vec<u8>,
    /// 缓存的背景图（隔帧刷新）
    backdrop: Option<skia_safe::Image>,
    /// 帧计数（判断要不要重新抓屏）
    frame_no: u32,
    /// 上一帧时窗口的位置（用来判断"是否正在移动"）
    last_win_pos: (i32, i32),
    /// 当前这张背景图是从屏幕哪个点开始抓的
    cap_origin: (i32, i32),
    /// 抓这张图的时候窗口在哪（用来判断窗口跑出去多远、要不要重抓）
    cap_win_pos: (i32, i32),
    /// 调试：让窗口自己每帧挪动，模拟拖动
    sim_drag: bool,
    /// 边缘收纳：鼠标离开 5 秒后缩到最近的屏幕边，只留一条边
    edge_enabled: bool,
    /// 0 = 完全展开，1 = 完全收起
    edge_anim: f32,
    /// 鼠标离开窗口多久了（秒）
    edge_idle: f32,
    /// "家"的位置：展开时窗口该在哪（收起时不能拿当前位置当记忆）
    rest_pos: (i32, i32),
    /// 是否正在拖动（手动拖动：跟着鼠标位移走，不用系统模态拖动）
    dragging: bool,
    /// 按下时鼠标在哪、窗口在哪
    drag_from_cursor: (i32, i32),
    drag_from_window: (i32, i32),
    /// FPS 统计（debug 用）
    fps_n: u32,
    fps_t: Instant,
    /// 每帧自己实际花掉的 CPU 毫秒（debug 用）
    busy_ms: f32,
    ms_cap: f32,
    ms_rnd: f32,
    ms_read: f32,
    ms_blit: f32,
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
            prev_space: false,
            prev_r: false,
            prev_s: false,
            pomo: timer::Pomodoro::new(),
            overlay: Overlay::load(),
            dump_path: None,
            frames: 0,
            last_frame: Instant::now(),
            glass_only: false,
            dbg_cap: ((i32::MIN, i32::MIN), false),
            cap_buf: Vec::new(),
            backdrop: None,
            frame_no: 0,
            last_win_pos: (i32::MIN, i32::MIN),
            cap_origin: (0, 0),
            cap_win_pos: (i32::MIN / 2, i32::MIN / 2),
            sim_drag: false,
            edge_enabled: true,
            edge_anim: 0.0,
            edge_idle: 0.0,
            rest_pos: (0, 0),
            dragging: false,
            drag_from_cursor: (0, 0),
            drag_from_window: (0, 0),
            fps_n: 0,
            fps_t: Instant::now(),
            busy_ms: 0.0,
            ms_cap: 0.0,
            ms_rnd: 0.0,
            ms_read: 0.0,
            ms_blit: 0.0,
        }
    }

    /// debug 构建里打印番茄钟状态变化，release 保持安静
    fn log_pomo(&self, what: &str) {
        if cfg!(debug_assertions) {
            eprintln!(
                "[番茄钟] {} {} ({})",
                what,
                self.pomo.text(),
                self.pomo.subtitle()
            );
        }
    }

    /// 切换点击穿透（WS_EX_TRANSPARENT）
    fn toggle_click_through(&mut self) {        self.click_through = !self.click_through;
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
            let esc_raw = GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16;
            let t_raw = GetAsyncKeyState(VK_T.0 as i32) as u16;
            let sp_raw = GetAsyncKeyState(VK_SPACE.0 as i32) as u16;
            let r_raw = GetAsyncKeyState(VK_R.0 as i32) as u16;
            let s_raw = GetAsyncKeyState(VK_S.0 as i32) as u16;
            let esc = (esc_raw & 0x8001) != 0;
            let t = (t_raw & 0x8001) != 0;
            let space = (sp_raw & 0x8001) != 0;
            let r = (r_raw & 0x8001) != 0;
            let s = (s_raw & 0x8001) != 0;

            if cfg!(debug_assertions) {
                // 只在状态变化时打印，免得刷屏
                if esc != self.prev_esc || t != self.prev_t {
                    eprintln!("[按键] ESC原始={esc_raw:#06x} T原始={t_raw:#06x}");
                }
            }

            let mut quit = false;
            if esc && !self.prev_esc {
                quit = true;
            }
            if t && !self.prev_t {
                self.toggle_click_through();
                // 给个视觉反馈：标题栏（这里没有）→ 用控制台输出不了，
                // 所以靠手感；穿透后鼠标直接穿过窗口
            }
            // ---- 番茄钟控制 ----
            if space && !self.prev_space {
                self.pomo.toggle();
                let what = if self.pomo.running { "开始" } else { "暂停" };
                self.log_pomo(what);
            }
            if r && !self.prev_r {
                // Shift+R = 连轮次也清空，从第一个番茄重新开始
                let shift = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
                if shift {
                    self.pomo.reset_all();
                    self.log_pomo("全部重置");
                } else {
                    self.pomo.reset();
                    self.log_pomo("重置本阶段");
                }
            }
            if s && !self.prev_s {
                self.pomo.advance();
                self.pomo.running = false; // 跳过之后先停住，让人自己按空格
                self.log_pomo("跳过");
            }
            self.prev_esc = esc;
            self.prev_t = t;
            self.prev_space = space;
            self.prev_r = r;
            self.prev_s = s;
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

    /// 手动拖动：每帧按"鼠标总位移"重新摆窗口。
    ///
    /// 不用 winit 的 `drag_window()` —— 那会进系统的模态移动循环，
    /// 期间我们的事件循环被卡住，**画面是冻结的**，窗口在动内容不动，
    /// 看起来就是"拖动不丝滑 / 画面割裂"。手动拖动的话每一帧都能重画。
    fn update_drag(&mut self) {
        if !self.dragging {
            return;
        }
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let dx = pt.x - self.drag_from_cursor.0;
        let dy = pt.y - self.drag_from_cursor.1;
        let (nx, ny) = (self.drag_from_window.0 + dx, self.drag_from_window.1 + dy);
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                nx,
                ny,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// 边缘收纳：鼠标离开 5 秒 → 滑到最近的屏幕边，只留一条 PEEK 宽的缝；
    /// 鼠标碰到那条缝 → 滑回来。
    ///
    /// 返回 true 表示这一帧完全收起、可以跳过渲染（省 CPU，它是长期挂着的）。
    fn update_edge_hide(&mut self, dt: f32) -> bool {
        if !self.edge_enabled {
            return false;
        }
        let (pt, rect) = self.cursor_and_rect();
        let over = pt.x >= rect.left && pt.x < rect.right && pt.y >= rect.top && pt.y < rect.bottom;

        // 屏幕（含多显示器的虚拟桌面范围）
        unsafe {
            let sx = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let sy = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let sw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let sh = GetSystemMetrics(SM_CYVIRTUALSCREEN);

            // 鼠标在窗口上（收起时那条缝也算）→ 清零计时并展开
            if over {
                self.edge_idle = 0.0;
            } else {
                self.edge_idle += dt;
            }
            // 展开状态下把当前位置记成"家"，拖动时自动跟着更新
            if self.edge_anim < 0.02 {
                self.rest_pos = (rect.left, rect.top);
            }

            let target = if self.edge_idle > EDGE_HIDE_SECS && !self.dragging {
                1.0
            } else {
                0.0
            };
            // 平滑滑动（约 0.25 秒完成）
            self.edge_anim += (target - self.edge_anim) * (dt * 10.0).min(1.0);
            if (target - self.edge_anim).abs() < 0.01 {
                self.edge_anim = target;
            }

            if self.edge_anim <= 0.001 {
                return false;
            }

            // 用"实际窗口尺寸"算，不要用渲染尺寸 W/H ——
            // 开了高 DPI 缩放时两者不一样，用 W/H 会把窗口推到屏幕外面去。
            let ww = rect.right - rect.left;
            let wh = rect.bottom - rect.top;

            // 选最近的那条边
            let cx = self.rest_pos.0 + ww / 2;
            let cy = self.rest_pos.1 + wh / 2;
            let (dl, dr) = (cx - sx, sx + sw - cx);
            let (dtop, dbot) = (cy - sy, sy + sh - cy);
            let m = dl.min(dr).min(dtop).min(dbot);
            let (hx, hy) = if m == dr {
                (sx + sw - EDGE_PEEK, self.rest_pos.1)
            } else if m == dl {
                (sx - ww + EDGE_PEEK, self.rest_pos.1)
            } else if m == dbot {
                (self.rest_pos.0, sy + sh - EDGE_PEEK)
            } else {
                (self.rest_pos.0, sy - wh + EDGE_PEEK)
            };

            let a = self.edge_anim;
            let x = (self.rest_pos.0 as f32 + (hx - self.rest_pos.0) as f32 * a).round() as i32;
            let y = (self.rest_pos.1 as f32 + (hy - self.rest_pos.1) as f32 * a).round() as i32;
            if (x, y) != (rect.left, rect.top) {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
            // 完全收起后只留一条缝，没必要每帧都抓屏+渲染
            if cfg!(debug_assertions) && std::env::var_os("LG_DEBUG_EDGE").is_some() {
                eprintln!(
                    "[收纳] 鼠标=({},{}) 窗口=({},{}) 上={} 空闲={:.1}s 进度={:.2} 家=({},{})",
                    pt.x, pt.y, rect.left, rect.top, over, self.edge_idle, self.edge_anim,
                    self.rest_pos.0, self.rest_pos.1
                );
            }
            self.edge_anim > 0.98
        }

    /// 每帧的统计（FPS / 分段耗时）。原来的跟手小圆（液态鼓包）已按需求移除。
    fn update_stats(&mut self, _dt: f32) {
        self.fps_n += 1;
        if self.fps_n >= 60 {
            let el = self.fps_t.elapsed().as_secs_f32();
            let n = self.fps_n as f32;
            eprintln!(
                "[性能] {} 帧用了 {:.2} 秒 = {:.1} FPS\n         抓屏 {:.1} ms | 渲染 {:.1} ms | 读回 {:.1} ms | 上屏 {:.1} ms | 合计 {:.1} ms/帧",
                self.fps_n,
                el,
                n / el,
                self.ms_cap / n,
                self.ms_rnd / n,
                self.ms_read / n,
                self.ms_blit / n,
                self.busy_ms / n
            );
            self.fps_n = 0;
            self.busy_ms = 0.0;
            self.ms_cap = 0.0;
            self.ms_rnd = 0.0;
            self.ms_read = 0.0;
            self.ms_blit = 0.0;
            self.fps_t = Instant::now();
        }
    }

    fn draw(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        self.update_stats(dt);

        // 番茄钟推进。阶段切完响一声（放后台线程，不然 Beep 会把这一帧卡住 200ms）
        self.pomo.tick(dt);
        if self.pomo.chime {
            std::thread::spawn(|| unsafe {
                let _ = Beep(880, 150);
                let _ = Beep(1180, 220);
            });
            // 换阶段时把窗口弹出来给人看见（收起状态也先展开 5 秒）
            self.edge_idle = 0.0;
        }

        // 拖动 / 边缘收纳（都要在渲染之前摆好窗口位置）
        self.update_drag();
        let parked = self.update_edge_hide(dt);
        if parked {
            // 已经收在屏幕边上，只剩一条缝：跳过抓屏+渲染，但要保持低频轮询
            // （番茄钟还得继续走，鼠标碰到那条缝还得能滑回来）。
            // 不 sleep 的话这里会空转到 500+ FPS 白烧 CPU。
            std::thread::sleep(std::time::Duration::from_millis(80));
            self.frame_no += 1;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }

        let t_frame = Instant::now();

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
        let win_pos = (rect.left, rect.top);
        let moved = self.last_win_pos != win_pos;
        self.last_win_pos = win_pos;
        self.frame_no += 1;

        // 调试：--sim-drag 让窗口自己每帧挪一点，用来测"拖动时"的抓屏策略和帧率
        if self.sim_drag {
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    win_pos.0 + 2,
                    win_pos.1 + 1,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }

        let t_cap = Instant::now();
        let mut need_capture_log = false;
        if !glass_only {
            // 抓屏是"带余量"的：窗口四周各多抓 CAP_MARGIN 像素。
            // 好处：窗口在余量范围内移动时，只要把 backdropOffset 改一下，
            // 画面就跟窗口严丝合缝 —— 不用每帧重新 BitBlt（一次要 20ms，还容易错位割裂）。
            let sw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
            let sh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
            let sx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
            let sy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };

            // 期望区域：以窗口为中心向外扩 margin，再夹到屏幕范围内
            let cw = W + CAP_MARGIN * 2;
            let ch = H + CAP_MARGIN * 2;
            let want_x = win_pos.0 - CAP_MARGIN;
            let want_y = win_pos.1 - CAP_MARGIN;
            let cap_x = if sw > cw { want_x.clamp(sx, sx + sw - cw) } else { sx };
            let cap_y = if sh > ch { want_y.clamp(sy, sy + sh - ch) } else { sy };

            // 判断要不要重抓：看窗口从"上次抓屏时所在的位置"跑了多远。
            // 不能按"窗口是否完整落在图里"判断 —— 窗口贴屏幕边时抓屏区域会被夹住，
            // 那个条件会永远为真，导致每帧都重抓（比不优化还卡）。
            // 用位移判断既简单又不会自锁；超出范围的部分采样会 clamp（边缘轻微拉伸）。
            let drift = (win_pos.0 - self.cap_win_pos.0)
                .abs()
                .max((win_pos.1 - self.cap_win_pos.1).abs());
            let outside = self.backdrop.is_none() || drift > CAP_MARGIN;
            // 拖动时不做定期刷新（只靠偏移平移，最省最顺）；
            // 静止时每 2 帧刷新一次，让玻璃里的桌面跟得上变化
            // 静止时也不用每帧抓：桌面很少变，3 帧刷一次（约 100ms）足够，
            // 省下的都是 CPU —— 这东西是长期挂着的番茄钟，不是游戏。
            let need_capture = outside || (!moved && self.frame_no % 4 == 0);
            need_capture_log = need_capture;
            if std::env::var_os("LG_DEBUG_CAP").is_some() {
                let off = (win_pos.0 - self.cap_origin.0, win_pos.1 - self.cap_origin.1);
                eprintln!(
                    "[抓屏诊断] win=({},{}) cap=({},{}) off=({},{}) 位移={} 图={}x{} 抓={}",
                    win_pos.0, win_pos.1, self.cap_origin.0, self.cap_origin.1,
                    off.0, off.1, drift, cw, ch, need_capture
                );
            }

            if need_capture {
                let cap = &mut self.cap_buf;
                let ok = capture_screen(cap_x, cap_y, cw, ch, cap).is_some();
                if ok {
                    let info = skia_safe::ImageInfo::new(
                        (cw, ch),
                        skia_safe::ColorType::BGRA8888,
                        skia_safe::AlphaType::Premul,
                        None,
                    );
                    self.backdrop = skia_safe::images::raster_from_data(
                        &info,
                        Data::new_copy(cap),
                        (cw * 4) as usize,
                    );
                    self.cap_origin = (cap_x, cap_y);
                    self.cap_win_pos = win_pos;
                }
            }
        }
        // 用当前窗口位置实时算偏移：不管这一帧抓没抓，采样点永远对得上窗口
        let backdrop_offset = (
            (win_pos.0 - self.cap_origin.0) as f32,
            (win_pos.1 - self.cap_origin.1) as f32,
        );
        if cfg!(debug_assertions)
            && (moved || self.dbg_cap != (self.cap_origin, need_capture_log))
        {
            self.dbg_cap = (self.cap_origin, need_capture_log);
            eprintln!(
                "[抓屏] 窗口=({},{}) 抓屏原点=({},{}) 偏移=({:.0},{:.0}) 重抓={}",
                win_pos.0,
                win_pos.1,
                self.cap_origin.0,
                self.cap_origin.1,
                backdrop_offset.0,
                backdrop_offset.1,
                if need_capture_log { "是" } else { "否" }
            );
        }
        let ms_cap = t_cap.elapsed().as_secs_f32() * 1000.0;

        // 2) 渲染一帧（复用 surface，不每帧重新分配）

        // 2) 渲染一帧（复用 surface，不每帧重新分配）
        let t_rnd = Instant::now();
        let surface = self
            .surface
            .get_or_insert_with(|| surfaces::raster_n32_premul((W, H)).expect("surface"));
        draw_frame(
            surface,
            effect,
            t,
            self.backdrop.as_ref(),
            angle,
            &self.pomo,
            self.overlay.as_ref(),
            backdrop_offset,
            glass_only,
        );
        let ms_rnd = t_rnd.elapsed().as_secs_f32() * 1000.0;

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
        let t_read = Instant::now();
        let ok = surface.read_pixels(&info, &mut self.buf, (W * 4) as usize, (0, 0));
        let ms_read = t_read.elapsed().as_secs_f32() * 1000.0;

        let t_blit = Instant::now();
        if ok {
            dib.blit(self.hwnd, &self.buf);
        }
        let ms_blit = t_blit.elapsed().as_secs_f32() * 1000.0;

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
                save_pos_at(self.hwnd, Some(self.rest_pos));
                std::process::exit(0);
            }
        }

        if let Some(w) = &self.window {
            w.request_redraw();
        }

        // 分段计时：帧间隔 vs 我们真正花掉的 CPU 时间
        // （差值就是 DWM 合成/等待的时间）
        self.busy_ms += t_frame.elapsed().as_secs_f32() * 1000.0;
        self.ms_cap += ms_cap;
        self.ms_rnd += ms_rnd;
        self.ms_read += ms_read;
        self.ms_blit += ms_blit;
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
            // 左键松开 → 结束拖动，记住落点
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.dragging {
                    self.dragging = false;
                    let (_pt, rect) = self.cursor_and_rect();
                    self.rest_pos = (rect.left, rect.top);
                    // 拖到哪就记哪（下次启动回到这里）
                    save_pos_at(self.hwnd, Some(self.rest_pos));
                    if cfg!(debug_assertions) {
                        eprintln!("[拖动] 结束 落点={:?}", self.rest_pos);
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => match button {
                MouseButton::Left => {
                    // 穿透状态下不该能拖动（鼠标事件本来就到不了窗口，这里加个保险）
                    if !self.click_through {
                        self.dragging = true;
                        self.edge_anim = 0.0;
                        self.edge_idle = 0.0;
                        let (pt, rect) = self.cursor_and_rect();
                        self.drag_from_cursor = (pt.x, pt.y);
                        self.drag_from_window = (rect.left, rect.top);
                        self.rest_pos = (rect.left, rect.top);
                        if cfg!(debug_assertions) {
                            eprintln!(
                                "[拖动] 开始 鼠标={:?} 窗口={:?}",
                                self.drag_from_cursor, self.drag_from_window
                            );
                        }
                    }
                }
                // 右键 = 退出（无边框窗口给个退路）
                MouseButton::Right => event_loop.exit(),
                _ => {}
            },
            WindowEvent::KeyboardInput { event, .. } => {
                use winit::keyboard::{Key, NamedKey};
                if event.state != ElementState::Pressed {
                    return;
                }
                // 按 ESC 退出，免得无边框窗口关不掉
                if event.logical_key == Key::Named(NamedKey::Escape) {
                    event_loop.exit();
                    return;
                }
                // 键走两条路：
                //   1) 窗口有焦点时走这里（可靠，快速点按也不会漏）
                //   2) 穿透/无焦点时靠 poll_keys() 轮询
                // 这里处理完就把对应的 prev_* 置 true，轮询那边就不会重复触发，
                // 等松手后轮询再把它归零。
                let ch = match &event.logical_key {
                    Key::Character(c) => c.to_lowercase(),
                    _ => String::new(),
                };
                let is_space = event.logical_key == Key::Named(NamedKey::Space);
                if is_space {
                    self.pomo.toggle();
                    let what = if self.pomo.running { "开始" } else { "暂停" };
                    self.log_pomo(what);
                    self.prev_space = true;
                } else if ch == "r" {
                    let shift = (unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 } & 0x8000) != 0;
                    if shift {
                        self.pomo.reset_all();
                        self.log_pomo("全部重置");
                    } else {
                        self.pomo.reset();
                        self.log_pomo("重置本阶段");
                    }
                    self.prev_r = true;
                } else if ch == "s" {
                    self.pomo.advance();
                    self.pomo.running = false;
                    self.log_pomo("跳过");
                    self.prev_s = true;
                } else if ch == "t" {
                    self.toggle_click_through();
                    self.prev_t = true;
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
    // 调试：--offset dx,dy 直接给背景采样加偏移（验证"抓屏带余量"的采样数学）
    let mut off = (0.0f32, 0.0f32);
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--offset") {
        if let Some(v) = args.get(i + 1) {
            let nums: Vec<f32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
            if nums.len() == 2 {
                off = (nums[0], nums[1]);
            }
        }
    }
    draw_frame(
        &mut surface,
        &effect,
        0.0,
        None,
        2.2,
        &timer::Pomodoro::new(),
        Overlay::load().as_ref(),
        off,
        false,
    );

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
    // --no-edge-hide：关掉「鼠标离开 5 秒自动收起到屏幕边缘」
    if args.iter().any(|a| a == "--no-edge-hide") {
        app.edge_enabled = false;
    }
    // 调试：--sim-drag 窗口自己每帧挪一点（测拖动时的抓屏策略/帧率）
    if args.iter().any(|a| a == "--sim-drag") {
        app.sim_drag = true;
    }
    // 调试：--time <秒> 直接指定剩余时间（验证数字渲染用）
    if let Some(i) = args.iter().position(|a| a == "--time") {
        if let Some(v) = args.get(i + 1) {
            if let Ok(secs) = v.trim().parse::<f32>() {
                app.pomo.remaining = secs.max(0.0);
            }
        }
    }
    // 调试：--phase focus|short|long 指定阶段（验证休息配色用）
    if let Some(i) = args.iter().position(|a| a == "--phase") {
        match args.get(i + 1).map(|s| s.as_str()) {
            Some("short") => {
                app.pomo.phase = timer::Phase::Short;
                app.pomo.remaining = timer::SHORT_SECS;
            }
            Some("long") => {
                app.pomo.phase = timer::Phase::Long;
                app.pomo.remaining = timer::LONG_SECS;
            }
            Some("focus") => {
                app.pomo.phase = timer::Phase::Focus;
                app.pomo.remaining = timer::FOCUS_SECS;
            }
            _ => {}
        }
    }

    event_loop.run_app(&mut app).expect("运行失败");
}

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
const W: i32 = 620;
const H: i32 = 420;
/// 抓屏时窗口四周多抓多少像素。窗口在这么多像素内移动都不用重抓，
/// 只靠 backdropOffset 平移就能保证画面跟窗口对齐（拖动不割裂、不掉帧）。
const CAP_MARGIN: i32 = 120;

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
uniform float2 blob0Pos;
uniform float  blob0R;
uniform float  pad1;
uniform float2 blob1Pos;
uniform float  blob1R;
uniform float  pad2;
uniform float2 blob2Pos;
uniform float  blob2R;
uniform float  pad3;
uniform float4 tintColor;
uniform float4 tintColor2;
uniform float2 backdropOffset;

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

// 整个玻璃的形状 = 主体矩形 ∪ 若干跟手小圆
//
// 三个小圆 = 光标当前位置 + 前两个历史位置（半径递减），
// 快速移动光标时会拉出一条液态拖尾，多个圆跟主体平滑融合。
float sceneD(float2 p) {
    float d = sdRoundRect(p, rectSize * 0.5, corner);
    // 三个小圆通常一个都没激活：先做一次总判断直接返回，
    // 省掉每像素三次分支（sceneD 每像素会被调用 1~3 次）
    if (blob0R + blob1R + blob2R < 0.5) {
        return d;
    }
    if (blob0R > 0.5) {
        d = smin(d, length(p - blob0Pos) - blob0R, 30.0);
    }
    if (blob1R > 0.5) {
        d = smin(d, length(p - blob1Pos) - blob1R, 24.0);
    }
    if (blob2R > 0.5) {
        d = smin(d, length(p - blob2Pos) - blob2R, 18.0);
    }
    return d;
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
    // 三个跟手小圆：(x, y, 半径)，都相对玻璃中心
    blobs: [(f32, f32, f32); 3],
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

    let rect_w = 420.0f32;
    let rect_h = 240.0f32;
    // 布局必须和 SkSL 里的 uniform 声明一一对应（含 16 字节对齐的 float4）
    // 0 rectSize | 2 center | 4..11 标量 | 12..15 blob0 | 16..19 blob1 | 20..23 blob2
    // | 24 tintColor | 28 tintColor2 | 32 backdropOffset（共 34 个 float = 136 字节）
    let uniform: [f32; 34] = [
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
        0.70,           // tintAmount：黑玻璃，染得重才能压成黑色
        0.0,            // pad0（对齐用）
        blobs[0].0,     // blob0Pos：跟手小圆的位置（相对玻璃中心）
        blobs[0].1,
        blobs[0].2,     // blob0R：半径，0 = 不融合
        0.0,            // pad1
        blobs[1].0,     // blob1Pos：拖尾第二个
        blobs[1].1,
        blobs[1].2,     // blob1R
        0.0,            // pad2
        blobs[2].0,     // blob2Pos：拖尾第三个
        blobs[2].1,
        blobs[2].2,     // blob2R
        0.0,            // pad3
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
    /// 三个跟手小圆：(x, y, 半径)，相对玻璃中心
    blobs: [(f32, f32, f32); 3],
    /// 光标轨迹（最近 3 个位置），用来做液态拖尾
    trail: [(f32, f32); 3],
    /// 上一帧的时间，用来算 dt（让插值和帧率无关）
    last_frame: Instant,
    /// 只画玻璃不画背景（调试）
    glass_only: bool,
    /// 调试计数器
    dbg_n: u32,
    /// debug：上一帧打印过的 (抓屏原点, 是否重抓)，用来只在变化时打日志
    dbg_cap: ((i32, i32), bool),
    /// 调试：强制指定融合小圆的位置和半径（不受鼠标影响）
    blob_override: Option<[(f32, f32, f32); 3]>,
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
            blobs: [(0.0, 0.0, 0.0); 3],
            trail: [(0.0, 0.0); 3],
            last_frame: Instant::now(),
            glass_only: false,
            dbg_n: 0,
            dbg_cap: ((i32::MIN, i32::MIN), false),
            blob_override: None,
            cap_buf: Vec::new(),
            backdrop: None,
            frame_no: 0,
            last_win_pos: (i32::MIN, i32::MIN),
            cap_origin: (0, 0),
            cap_win_pos: (i32::MIN / 2, i32::MIN / 2),
            sim_drag: false,
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

    /// 跟手小圆：光标靠近玻璃边缘时，玻璃朝光标鼓起一个包。
    ///
    /// 三个小圆 = 光标当前位置 + 两个历史位置（半径递减），
    /// 快速移动光标时会拉出一条液态拖尾，全都跟主体平滑融合。
    fn update_blob(&mut self, dt: f32) {
        // 调试覆盖：--blob 直接指定融合小圆，不受真实鼠标影响
        // （注意：只跳过小圆计算，后面的调试统计照样要跑）
        if let Some(bs) = self.blob_override {
            self.blobs = bs;
        } else {
        let (cx, cy) = self.cursor_local();

        // 记轨迹：光标移动超过 6px 就往前挪一格（形成滞后）
        // ⚠️ 顺序很重要：必须先把旧值往后挪，再写新的 trail[0]，
        //    否则 trail[1] 存的永远是当前位置，拖尾就消失了。（复查发现过的 bug）
        let moved = ((cx - self.trail[0].0).powi(2) + (cy - self.trail[0].1).powi(2)).sqrt();
        if moved > 6.0 {
            self.trail[2] = self.trail[1];
            self.trail[1] = self.trail[0];
        }
        self.trail[0] = (cx, cy);

        let half_w = 210.0f32;
        let half_h = 120.0f32;
        let k = (dt * 9.0).min(1.0);

        // 每个小圆的基准半径：越靠后越小
        const BASE_R: [f32; 3] = [46.0, 34.0, 24.0];

        for i in 0..3 {
            let (sx, sy) = self.trail[i];
            let len = (sx * sx + sy * sy).sqrt();
            let mut bx = 0.0f32;
            let mut by = 0.0f32;
            let mut target = 0.0f32;

            if len > 1.0 {
                let dir_x = sx / len;
                let dir_y = sy / len;
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
                let dist = ((sx - bx).powi(2) + (sy - by).powi(2)).sqrt();
                if outside > -6.0 {
                    // 越贴近边缘包越大，离远了就收回去
                    target = BASE_R[i] * (1.0 - (dist - 10.0) / 120.0).clamp(0.0, 1.0);
                }
            }

            let b = &mut self.blobs[i];
            b.2 += (target - b.2) * k;
            b.0 += (bx - b.0) * k;
            b.1 += (by - b.1) * k;
            if b.2 < 0.4 {
                b.2 = 0.0;
            }
        }
        } // 小圆计算结束（--blob 覆盖时会跳过这一段）

        // 调试日志只在 debug 构建里打，release 版本保持安静
        self.dbg_n += 1;
        if cfg!(debug_assertions) && self.dbg_n % 20 == 1 {
            let (pt, rect) = self.cursor_and_rect();
            eprintln!(
                "[blob] 光标=({},{}) 窗口=({},{}) 小圆 r=({:.0},{:.0},{:.0}) 位置=({:.0},{:.0})/({:.0},{:.0})/({:.0},{:.0})",
                pt.x, pt.y, rect.left, rect.top,
                self.blobs[0].2, self.blobs[1].2, self.blobs[2].2,
                self.blobs[0].0, self.blobs[0].1,
                self.blobs[1].0, self.blobs[1].1,
                self.blobs[2].0, self.blobs[2].1
            );
        }
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
        // 跟手小圆：按 dt 插值，帧率无关
        // （必须放在下面的不可变借用之前，否则 self 借出冲突）
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        self.update_blob(dt);

        // 番茄钟推进。阶段切完响一声（放后台线程，不然 Beep 会把这一帧卡住 200ms）
        self.pomo.tick(dt);
        if self.pomo.chime {
            std::thread::spawn(|| unsafe {
                let _ = Beep(880, 150);
                let _ = Beep(1180, 220);
            });
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
            self.blobs,
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
                save_pos(self.hwnd);
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
        [(0.0, 0.0, 0.0); 3],
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
    // 调试：--blob x,y,r 强制指定融合小圆（不受真实鼠标干扰，方便自动化验证）
    // 可以写多组，用分号隔开：--blob "10,20,30;100,50,24"
    if let Some(i) = args.iter().position(|a| a == "--blob") {
        if let Some(v) = args.get(i + 1) {
            let mut blobs = [(0.0f32, 0.0f32, 0.0f32); 3];
            let mut n = 0;
            for group in v.split(';') {
                if n >= 3 {
                    break;
                }
                let nums: Vec<f32> = group
                    .split(',')
                    .filter_map(|s| s.trim().parse::<f32>().ok())
                    .collect();
                if nums.len() == 3 {
                    blobs[n] = (nums[0], nums[1], nums[2]);
                    n += 1;
                }
            }
            if n > 0 {
                app.blob_override = Some(blobs);
            }
        }
    }

    event_loop.run_app(&mut app).expect("运行失败");
}

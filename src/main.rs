// 液态玻璃 · 第一步验证
//
// 目标：不涉及窗口，直接用 Skia 的 CPU 光栅画一张图，验证 SkSL 运行时着色器能跑。
// 画面 = 彩色背景 + 一块"液态玻璃"圆角矩形（边缘折射 + 色散 + 高光）。
//
// 许可证：GPLv3
//   - 本项目独立于 MIT 的 floating-pomodoro 仓库。
//   - 着色器为原创实现，用的是公开的光学公式（斯涅尔折射定律、RGB 三通道色散）。
//     参考了公开的 Liquid Glass 设计思路，未复制他人代码。

use skia_safe::{
    Color, Data, EncodedImageFormat, Paint, PaintStyle, Rect, RRect, RuntimeEffect,
    SamplingOptions, TileMode,
};
use std::fs;

/// 液态玻璃着色器（原创实现）
///
/// 原理：
///   1. 用圆角矩形的有符号距离场(SDF)算出每个像素离边缘多远、梯度朝哪
///   2. 边缘 `bezel` 宽度内是个圆角斜面，法线随位置变化
///   3. 用斯涅尔定律折射视线，得到背景采样偏移
///   4. RGB 三通道用不同折射率 → 色散（彩边）
///   5. 光照方向决定高光
const LIQUID_SKSL: &str = r#"
uniform shader backdrop;

uniform float2 rectSize;    // 矩形宽高
uniform float2 center;      // 矩形中心（画布坐标）
uniform float  corner;      // 圆角半径
uniform float  bezel;       // 边缘斜面宽度
uniform float  thickness;   // 玻璃厚度
uniform float  ior;         // 折射率
uniform float  dispersion;  // 色散强度
uniform float  lightAngle;  // 光源角度(弧度)
uniform float  tintAmount;  // 着色浓度
uniform float  pad0;        // 对齐填充（float4 需要 16 字节对齐）
uniform float4 tintColor;   // 着色

// 圆角矩形 SDF：返回到边界的距离(内部为负)，g 为梯度(向外)
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
    // coord 是画布坐标，减去矩形中心得到局部坐标
    float2 local = coord - center;

    float2 g;
    float d = sdRoundRect(local, halfSize, corner, g);

    // 外部完全透明
    if (d > 0.0) { return half4(0.0); }

    float inward = -d;                                  // 离边缘多远（向内为正）
    float band = clamp(inward / max(bezel, 0.001), 0.0, 1.0);

    // 边缘斜面：越靠外斜得越陡，形成圆角透镜
    float u = 1.0 - band;
    float slope = band < 1.0
        ? (2.0 * u * u * u) / sqrt(max(1.0 - u * u * u * u, 0.0001))
        : 0.0;
    float3 normal = normalize(float3(g * slope, 1.0));

    // 斯涅尔折射：视线自上而下射入玻璃
    float3 incident = float3(0.0, 0.0, -1.0);
    float eta = 1.0 / max(ior, 1.0001);
    float3 ray = refract(incident, normal, eta);

    float2 shift = float2(0.0);
    if (ray.z < -0.0001) {
        shift = (ray.xy / -ray.z) * thickness * (0.35 + 0.65 * band);
    }

    // 色散：三个通道用略微不同的折射率
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

    // 着色
    glass = mix(glass, tintColor.rgb, tintAmount);

    // 高光：法线朝向光源的地方最亮
    float2 L = float2(cos(lightAngle), sin(lightAngle));
    float spec = pow(max(dot(g, L), 0.0), 6.0) * band * band;
    glass += spec * 0.85;

    // 边缘一圈细高光，做出玻璃的"包边"
    float rim = 1.0 - smoothstep(0.0, 1.6, inward);
    glass += rim * 0.25;

    float alpha = 1.0 - smoothstep(-1.0, 0.0, d);
    return half4(glass * alpha, alpha);
}
"#;

fn make_backdrop(width: i32, height: i32) -> skia_safe::Image {
    let mut surface = skia_safe::surfaces::raster_n32_premul((width, height)).unwrap();
    let canvas = surface.canvas();

    canvas.clear(Color::from_rgb(18, 18, 22));

    // 高对比色块，方便看折射
    let colors = [
        Color::from_rgb(255, 59, 107),
        Color::from_rgb(255, 201, 60),
        Color::from_rgb(46, 204, 113),
        Color::from_rgb(74, 125, 255),
        Color::from_rgb(168, 85, 247),
    ];
    for (i, c) in colors.iter().enumerate() {
        let mut p = Paint::default();
        p.set_color(*c);
        p.set_anti_alias(true);
        let x = (i as f32 % 3.0) * (width as f32 / 3.0);
        let y = (i as f32 / 3.0) * (height as f32 / 2.0);
        canvas.draw_rect(
            Rect::from_xywh(x, y, width as f32 / 3.0, height as f32 / 2.0),
            &p,
        );
    }

    // 一些细线，折射后会明显扭曲
    let mut line = Paint::default();
    line.set_color(Color::WHITE);
    line.set_style(PaintStyle::Stroke);
    line.set_stroke_width(2.0);
    let mut y = 10.0;
    while y < height as f32 {
        canvas.draw_line((0.0, y), (width as f32, y), &line);
        y += 22.0;
    }

    surface.image_snapshot()
}

fn main() {
    let width = 620;
    let height = 420;

    let backdrop_image = make_backdrop(width, height);

    let effect = RuntimeEffect::make_for_shader(LIQUID_SKSL, None)
        .unwrap_or_else(|e| panic!("SkSL 编译失败: {e}"));

    // uniform 必须严格按声明顺序、按 Skia 的对齐规则排列
    let rect_w = 420.0f32;
    let rect_h = 220.0f32;
    let cx = width as f32 / 2.0;
    let cy = height as f32 / 2.0;
    let uniform = [
        rect_w, rect_h,     // rectSize  (offset 0)
        cx, cy,             // center    (offset 8)
        44.0,               // corner    (offset 16)
        28.0,               // bezel
        36.0,               // thickness
        1.42,               // ior
        0.035,              // dispersion
        2.2,                // lightAngle
        0.10,               // tintAmount (offset 40)
        0.0,                // pad0      (offset 44)
        0.92, 0.95, 0.98, 1.0, // tintColor (offset 48, 16 字节对齐)
    ];
    let uniforms: Vec<u8> = uniform.iter().flat_map(|f| f.to_le_bytes()).collect();

    let bg_shader = backdrop_image
        .to_shader(
            Some((TileMode::Clamp, TileMode::Clamp)),
            SamplingOptions::default(),
            None,
        )
        .expect("to_shader 失败");

    let shader = effect
        .make_shader(Data::new_copy(&uniforms), &[bg_shader.into()], None)
        .expect("make_shader 失败");

    let mut out = skia_safe::surfaces::raster_n32_premul((width, height)).unwrap();
    let canvas = out.canvas();
    canvas.draw_image(&backdrop_image, (0, 0), None);

    let mut paint = Paint::default();
    paint.set_shader(shader);
    paint.set_anti_alias(true);

    let rect = Rect::from_xywh(
        (width as f32 - rect_w) / 2.0,
        (height as f32 - rect_h) / 2.0,
        rect_w,
        rect_h,
    );
    canvas.draw_rrect(RRect::new_rect_xy(rect, 44.0, 44.0), &paint);

    let image = out.image_snapshot();
    let data = image
        .encode(None, EncodedImageFormat::PNG, 100)
        .expect("PNG 编码失败");

    let path = "liquid-glass-out.png";
    fs::write(path, data.as_bytes()).expect("写文件失败");
    println!("OK -> {path}");
}

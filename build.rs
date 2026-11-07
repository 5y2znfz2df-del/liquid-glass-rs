// 把图标和版本信息编进 exe（资源管理器里能看到图标和属性）
//
// 版本号从 Cargo.toml 自动取，不要再写死 —— 写死过一次，
// 结果 exe 属性里一直显示 0.1.0，而 Cargo.toml 已经是 0.5.0 了。
#[cfg(windows)]
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=icon.ico");

    let ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    // Windows 版本资源要四段：0.5.0 → 0.5.0.0
    let four = if ver.matches('.').count() >= 3 {
        ver.clone()
    } else {
        format!("{ver}.0")
    };

    let mut res = winres::WindowsResource::new();
    res.set_icon("icon.ico");
    res.set("ProductName", "液态玻璃番茄钟");
    res.set("FileDescription", "液态玻璃番茄钟 · Liquid Glass Pomodoro");
    res.set("CompanyName", "XiangFu");
    res.set("LegalCopyright", "GPL-3.0-or-later © 2026 XiangFu");
    res.set("OriginalFilename", "LiquidGlassPomodoro.exe");
    res.set("FileVersion", &four);
    res.set("ProductVersion", &ver);
    if let Err(e) = res.compile() {
        // 没有 rc.exe 也不该让整个构建失败
        println!("cargo:warning=winres 编译失败（图标可能没嵌进去）: {e}");
    }
}

#[cfg(not(windows))]
fn main() {}

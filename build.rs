// 把图标和版本信息编进 exe（资源管理器里能看到图标和属性）
#[cfg(windows)]
fn main() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("icon.ico");
    res.set("ProductName", "Liquid Glass");
    res.set("FileDescription", "Liquid Glass - Windows 液态玻璃渲染 demo");
    res.set("CompanyName", "XiangFu");
    res.set("LegalCopyright", "GPL-3.0-or-later");
    res.set("FileVersion", "0.1.0.0");
    res.set("ProductVersion", "0.1.0");
    if let Err(e) = res.compile() {
        // 没有 rc.exe 也不该让整个构建失败
        println!("cargo:warning=winres 编译失败（图标可能没嵌进去）: {e}");
    }
}

#[cfg(not(windows))]
fn main() {}

//! Hopper desktop's exe: icon and main-thread stack. Build-time values the
//! logic needs (keys, release notes) are embedded by crates/hopper-core.

fn main() {
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    if !windows {
        return;
    }
    // The logo as the exe's icon (resource 1): Explorer, the taskbar and
    // GPUI's window icon all read it.
    println!("cargo:rerun-if-changed=assets/hopper.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon_with_id("assets/hopper.ico", "1");
    if let Err(e) = res.compile() {
        println!("cargo:warning=no exe icon: {e}");
    }
    // GPUI builds deep element trees on the main thread; Windows gives it
    // 1 MB by default, which a debug build can outgrow.
    println!("cargo:rustc-link-arg-bin=hopper=/STACK:16777216");
}

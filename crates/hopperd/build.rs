//! Copies the built web UI (ui/dist, from `pnpm build` in ui/) into OUT_DIR
//! for the binary to embed. Without a build, a small page says how to make
//! one, so `cargo build` works without Node.

use std::path::Path;

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

/// A .gz copy next to every text file worth compressing, which the panel
/// serves to browsers that accept gzip.
fn gzip_all(dir: &Path) {
    use std::io::Write as _;
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            gzip_all(&p);
            continue;
        }
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
        if !matches!(ext, "js" | "css" | "html" | "svg" | "json" | "webmanifest") {
            continue;
        }
        let data = std::fs::read(&p).unwrap();
        if data.len() < 1024 {
            continue;
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gz.write_all(&data).unwrap();
        let mut name = p.file_name().unwrap().to_os_string();
        name.push(".gz");
        std::fs::write(p.with_file_name(name), gz.finish().unwrap()).unwrap();
    }
}

/// The panel's logo: Hopper desktop's pixel hopper (assets/hopper-256.png,
/// 16 x 16 pixels) with a small server-rack badge in the corner. One grid
/// for every copy: the exe and tray icon (all .ico sizes) and the web UI's
/// logo.svg (favicon, sidebar, sign-in).
const LOGO: [&str; 16] = [
    "..DDDDDDDDDDDD..",
    ".DDDDDDoODDDDDD.",
    "DDDDDDDOODDDDDDD",
    "DDDDDDDDDDDDDDDD",
    "DDLLLLLLLLLLLLDD",
    "DDMMKKKKKKKKMMDD",
    "DDMMKKKKKKKKMMDD",
    "DDMMMMMMMMMMMMDD",
    "DDDDGGGGGGGGDDDD",
    "DDDDSSSSSSSSDDDD",
    "DDDDSSSSSXXXXXXX",
    "DDDDDDSSSXWWWWgX",
    "DDDDDDSSSXXXXXXX",
    "DDDDDDDKKXWWWWOX",
    ".DDDDDDKOXXXXXXX",
    "..DDDDDDDDDDDD..",
];

fn color(c: char) -> Option<[u8; 3]> {
    Some(match c {
        'D' => [0x18, 0x1d, 0x26],
        'O' => [0xf1, 0x64, 0x36],
        'o' => [0xff, 0x96, 0x5a],
        'L' => [0xa8, 0xae, 0xba],
        'M' => [0x7a, 0x80, 0x8c],
        'K' => [0x28, 0x2c, 0x36],
        'G' => [0x68, 0x6e, 0x7a],
        'S' => [0x4e, 0x54, 0x60],
        'X' => [0x0b, 0x0e, 0x13],
        'W' => [0xd6, 0xda, 0xe2],
        'g' => [0x22, 0xc5, 0x5e],
        _ => return None,
    })
}

/// The logo as RGBA, `size` x `size`. Sizes that are a multiple of 16 stay
/// crisp; others are box-sampled so no pixel row doubles up.
fn logo(size: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    let n = 16.0 / size as f32;
    for y in 0..size {
        for x in 0..size {
            let (mut rgb, mut a, mut count) = ([0f32; 3], 0f32, 0f32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let gx = ((x as f32 + (sx as f32 + 0.5) / 4.0) * n) as usize;
                    let gy = ((y as f32 + (sy as f32 + 0.5) / 4.0) * n) as usize;
                    count += 1.0;
                    if let Some(c) = LOGO[gy.min(15)].chars().nth(gx.min(15)).and_then(color) {
                        for i in 0..3 {
                            rgb[i] += c[i] as f32;
                        }
                        a += 1.0;
                    }
                }
            }
            let ch = |i: usize| if a > 0.0 { (rgb[i] / a).round() as u8 } else { 0 };
            px.extend_from_slice(&[ch(0), ch(1), ch(2), (a / count * 255.0).round() as u8]);
        }
    }
    px
}

/// The logo as an SVG of square pixels, for the web UI.
fn logo_svg() -> String {
    let mut s = String::from(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\" shape-rendering=\"crispEdges\">",
    );
    for (y, row) in LOGO.iter().enumerate() {
        let cells: Vec<char> = row.chars().collect();
        let mut x = 0;
        while x < cells.len() {
            let c = cells[x];
            let run = cells[x..].iter().take_while(|&&d| d == c).count();
            if let Some([r, g, b]) = color(c) {
                s.push_str(&format!("<rect x=\"{x}\" y=\"{y}\" width=\"{run}\" height=\"1\" fill=\"#{r:02x}{g:02x}{b:02x}\"/>"));
            }
            x += run;
        }
    }
    s.push_str("</svg>
");
    s
}

/// A .ico with 32-bit images of the logo.
fn ico() -> Vec<u8> {
    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 256];
    let mut images = Vec::new();
    for &n in &sizes {
        let rgba = logo(n);
        let mut img = Vec::new();
        for v in [40u32, n, n * 2] {
            img.extend_from_slice(&v.to_le_bytes());
        }
        img.extend_from_slice(&1u16.to_le_bytes());
        img.extend_from_slice(&32u16.to_le_bytes());
        img.extend_from_slice(&[0u8; 24]);
        for y in (0..n).rev() {
            for x in 0..n {
                let i = ((y * n + x) * 4) as usize;
                img.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
            }
        }
        img.extend(std::iter::repeat_n(0u8, (n.div_ceil(32) * 4 * n) as usize));
        images.push(img);
    }
    let mut out = vec![0, 0, 1, 0, sizes.len() as u8, 0];
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&n, img) in sizes.iter().zip(&images) {
        let b = if n >= 256 { 0 } else { n as u8 };
        out.extend_from_slice(&[b, b, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        out.extend(img);
    }
    out
}

/// On Windows: the logo as the exe's icon (resource 1, also the tray icon)
/// and a manifest for the first-run dialog's look (common controls 6).
fn windows_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let path = Path::new(&std::env::var("OUT_DIR").unwrap()).join("hopperd.ico");
    std::fs::write(&path, ico()).unwrap();
    let mut res = winresource::WindowsResource::new();
    res.set_icon_with_id(&path.to_string_lossy(), "1");
    res.set_manifest(MANIFEST);
    if let Err(e) = res.compile() {
        println!("cargo:warning=no exe icon or manifest: {e}");
    }
}

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency><dependentAssembly>
    <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
  </dependentAssembly></dependency>
  <application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings>
    <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
    <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
  </windowsSettings></application>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3"><security><requestedPrivileges>
    <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
  </requestedPrivileges></security></trustInfo>
</assembly>
"#;

fn main() {
    windows_resources();
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("ui");
    let _ = std::fs::remove_dir_all(&out);
    let dist = Path::new("ui/dist");
    println!("cargo:rerun-if-changed=ui/dist");
    let svg = logo_svg();
    println!("cargo:rerun-if-changed=ui/public/logo.svg");
    if std::fs::read_to_string("ui/public/logo.svg").ok().as_deref() != Some(svg.as_str()) {
        println!("cargo:warning=ui/public/logo.svg differs from build.rs's LOGO: copy it from OUT_DIR/ui/logo.svg");
    }
    if dist.join("index.html").is_file() {
        copy_dir(dist, &out).expect("copying ui/dist");
        std::fs::write(out.join("logo.svg"), &svg).unwrap();
        gzip_all(&out);
    } else {
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            out.join("index.html"),
            "<!doctype html><meta charset=utf-8><title>Hopper Server</title>\
             <body style=\"font:15px system-ui;background:#141418;color:#ececf1;padding:40px\">\
             <h1>Hopper Server</h1><p>The web UI isn't built into this copy. \
             Run <code>pnpm install &amp;&amp; pnpm build</code> in <code>crates/hopperd/ui</code>, \
             then build hopperd again. The API at <code>/api/v1</code> works.</p>",
        )
        .unwrap();
        std::fs::write(out.join("logo.svg"), &svg).unwrap();
    }
}

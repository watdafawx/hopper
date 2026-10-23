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

fn main() {
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("ui");
    let _ = std::fs::remove_dir_all(&out);
    let dist = Path::new("ui/dist");
    println!("cargo:rerun-if-changed=ui/dist");
    if dist.join("index.html").is_file() {
        copy_dir(dist, &out).expect("copying ui/dist");
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
    }
}

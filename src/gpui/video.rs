//! Playing a description's video.

use gpui_kit::App;
use hopper::richtext::Video;

/// Plays `v` in Hopper's player window (`hopper --video`), or in the
/// browser when that can't run (no WebView2, or not Windows).
pub fn play(v: &Video, cx: &mut App) {
    let url = v.url.clone();
    #[cfg(windows)]
    {
        let run = {
            let url = url.clone();
            cx.background_executor().spawn(async move {
                let exe = std::env::current_exe()?;
                std::process::Command::new(exe).arg("--video").arg(&url).status()
            })
        };
        cx.spawn(async move |cx| {
            // 2: no WebView2. A player killed or crashed mid-video stays shut.
            let code = run.await.map(|s| s.code());
            if matches!(code, Err(_) | Ok(Some(1 | 2))) {
                log::warn!("video window: {code:?}");
                let _ = cx.update(|cx| cx.open_url(&url));
            }
        })
        .detach();
    }
    #[cfg(not(windows))]
    cx.open_url(&url);
}

//! Playing a description's video.

use gpui_kit::App;
use hopper::richtext::Video;

/// Plays `v`: in the browser for now.
pub fn play(v: &Video, cx: &mut App) {
    cx.open_url(&v.url);
}

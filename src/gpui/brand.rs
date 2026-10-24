//! Site and loader logos: CurseForge's and Modrinth's (from Simple Icons)
//! and the loader icons Modrinth uses, served beside GPUI Kit's icons.
//! They're single-color, so they take the color they're drawn in.

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::*;
use std::borrow::Cow;

const FILES: &[(&str, &[u8])] = &[
    ("brand/curseforge.svg", include_bytes!("../../assets/brand/curseforge.svg")),
    ("brand/modrinth.svg", include_bytes!("../../assets/brand/modrinth.svg")),
    ("brand/neoforge.svg", include_bytes!("../../assets/brand/neoforge.svg")),
    ("brand/forge.svg", include_bytes!("../../assets/brand/forge.svg")),
    ("brand/fabric.svg", include_bytes!("../../assets/brand/fabric.svg")),
    ("brand/quilt.svg", include_bytes!("../../assets/brand/quilt.svg")),
];

/// GPUI Kit's icons plus the logos above.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = FILES.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit::assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut out = gpui_kit::assets::AllAssets.list(path)?;
        out.extend(FILES.iter().filter(|(p, _)| p.starts_with(path)).map(|(p, _)| SharedString::from(*p)));
        Ok(out)
    }
}

/// CurseForge's or Modrinth's logo.
pub fn site_icon(modrinth: bool) -> Icon {
    Icon::new(IconName::Globe).path(if modrinth { "brand/modrinth.svg" } else { "brand/curseforge.svg" })
}

/// A mod loader's icon and color, by name ("NeoForge", "fabric").
pub fn loader(name: &str) -> Option<crate::widgets::PickIcon> {
    let (path, color) = match name.to_ascii_lowercase().as_str() {
        "neoforge" => ("brand/neoforge.svg", crate::theme::palette::NEOFORGE),
        "forge" => ("brand/forge.svg", crate::theme::palette::FORGE),
        "fabric" => ("brand/fabric.svg", crate::theme::palette::FABRIC),
        "quilt" => ("brand/quilt.svg", crate::theme::palette::QUILT),
        _ => return None,
    };
    Some(crate::widgets::PickIcon::Svg(path, color))
}

/// Screenshots of getting a CurseForge API key (the account and key
/// blanked out): the console's API keys link, then its copy button.
pub fn cf_key_steps() -> [std::sync::Arc<Image>; 2] {
    use std::sync::{Arc, OnceLock};
    static STEPS: OnceLock<[Arc<Image>; 2]> = OnceLock::new();
    STEPS
        .get_or_init(|| {
            let png = |b: &[u8]| Arc::new(Image::from_bytes(ImageFormat::Png, b.to_vec()));
            [
                png(include_bytes!("../../assets/cfkey/step1.png")),
                png(include_bytes!("../../assets/cfkey/step2.png")),
            ]
        })
        .clone()
}

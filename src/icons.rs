//! Registers the application's bundled SVG icons with GPUI's asset system.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// Serves bundled icon files and falls back to GPUI's built-in assets.
pub struct IconAssets;

impl AssetSource for IconAssets {
    // Resolves known application paths to embedded bytes and delegates other paths.
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let svg: &'static [u8] = match path {
            "icons/archive.svg" => include_bytes!("../assets/icons/archive.svg"),
            "icons/archive-locked.svg" => include_bytes!("../assets/icons/archive-locked.svg"),
            "icons/binary.svg" => include_bytes!("../assets/icons/binary.svg"),
            "icons/audio.svg" => include_bytes!("../assets/icons/audio.svg"),
            "icons/chevron-down.svg" => include_bytes!("../assets/icons/chevron-down.svg"),
            "icons/chevron-left.svg" => include_bytes!("../assets/icons/chevron-left.svg"),
            "icons/chevron-right.svg" => include_bytes!("../assets/icons/chevron-right.svg"),
            "icons/chevron-up.svg" => include_bytes!("../assets/icons/chevron-up.svg"),
            "icons/code.svg" => include_bytes!("../assets/icons/code.svg"),
            "icons/csv.svg" => include_bytes!("../assets/icons/csv.svg"),
            "icons/document.svg" => include_bytes!("../assets/icons/document.svg"),
            "icons/documents.svg" => include_bytes!("../assets/icons/documents.svg"),
            "icons/file-spreadsheet.svg" => include_bytes!("../assets/icons/file-spreadsheet.svg"),
            "icons/file.svg" => include_bytes!("../assets/icons/file.svg"),
            "icons/folder.svg" => include_bytes!("../assets/icons/folder.svg"),
            "icons/home.svg" => include_bytes!("../assets/icons/home.svg"),
            "icons/image.svg" => include_bytes!("../assets/icons/image.svg"),
            "icons/pdf.svg" => include_bytes!("../assets/icons/pdf.svg"),
            "icons/plus.svg" => include_bytes!("../assets/icons/plus.svg"),
            "icons/search.svg" => include_bytes!("../assets/icons/search.svg"),
            "icons/toolbox.svg" => include_bytes!("../assets/icons/toolbox.svg"),
            "icons/video.svg" => include_bytes!("../assets/icons/video.svg"),
            "icons/box-open.svg" => include_bytes!("../assets/icons/box-open.svg"),
            "icons/folder-open.svg" => include_bytes!("../assets/icons/folder-open.svg"),
            "icons/shield-halved.svg" => include_bytes!("../assets/icons/shield-halved.svg"),
            "icons/downloads.svg" => include_bytes!("../assets/icons/downloads.svg"),
            "icons/keyboard.svg" => include_bytes!("../assets/icons/keyboard.svg"),
            "icons/swatchbook.svg" => include_bytes!("../assets/icons/swatchbook.svg"),
            "icons/network.svg" => include_bytes!("../assets/icons/network.svg"),
            _ => return gpui::AssetSource::load(&gpui::assets::AllAssets, path).or(Ok(None)),
        };
        Ok(Some(Cow::Borrowed(svg)))
    }

    // Lists bundled icon names so GPUI can discover the custom asset set.
    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        if path != "icons" {
            return Ok(Vec::new());
        }
        Ok([
            "archive.svg",
            "archive-locked.svg",
            "binary.svg",
            "shield-halved.svg",
            "audio.svg",
            "chevron-down.svg",
            "chevron-left.svg",
            "chevron-right.svg",
            "chevron-up.svg",
            "code.svg",
            "csv.svg",
            "document.svg",
            "documents.svg",
            "file-spreadsheet.svg",
            "file.svg",
            "folder.svg",
            "home.svg",
            "image.svg",
            "pdf.svg",
            "plus.svg",
            "search.svg",
            "toolbox.svg",
            "video.svg",
            "box-open.svg",
            "shield-halved.svg",
            "swatchbook.svg",
            "keyboard.svg",
            "folder-open.svg",
            "downloads.svg",
            "network.svg",
        ]
        .into_iter()
        .map(SharedString::from)
        .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::IconAssets;

    #[test]
    /// Ensures every registered icon is valid SVG and unknown names are absent.
    fn registered_icons_load_as_svg_assets() {
        let paths = [
            "icons/archive.svg",
            "icons/archive-locked.svg",
            "icons/binary.svg",
            "icons/audio.svg",
            "icons/chevron-down.svg",
            "icons/chevron-left.svg",
            "icons/chevron-right.svg",
            "icons/chevron-up.svg",
            "icons/code.svg",
            "icons/csv.svg",
            "icons/document.svg",
            "icons/documents.svg",
            "icons/file-spreadsheet.svg",
            "icons/file.svg",
            "icons/folder.svg",
            "icons/home.svg",
            "icons/image.svg",
            "icons/pdf.svg",
            "icons/plus.svg",
            "icons/search.svg",
            "icons/toolbox.svg",
            "icons/video.svg",
            "icons/box-open.svg",
            "icons/shield-halved.svg",
            "icons/folder-open.svg",
            "icons/downloads.svg",
            "icons/keyboard.svg",
            "icons/swatchbook.svg",
            "icons/network.svg"
        ];

        for path in paths {
            let asset = <IconAssets as gpui::AssetSource>::load(&IconAssets, path)
                .unwrap()
                .unwrap();
            let svg = std::str::from_utf8(&asset).unwrap();
            assert!(svg.starts_with("<svg "));
            assert!(svg.trim_end().ends_with("</svg>"));
        }

        assert!(
            <IconAssets as gpui::AssetSource>::load(&IconAssets, "icons/missing.svg")
                .unwrap()
                .is_none()
        );
    }
}

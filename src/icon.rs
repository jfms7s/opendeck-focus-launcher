use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use std::path::Path;

#[derive(Debug)]
pub enum IconEncodeError {
    Read(std::io::Error),
}

/// Builds the `image` string OpenDeck's `setImage` event expects from a
/// resolved icon file on disk: raw SVG markup for `.svg` icons (the Stream
/// Deck SDK accepts SVG directly, no encoding needed), or a base64 data URI
/// for anything else - PNG is by far the common case for icon themes' raster
/// fallback, but JPEG/BMP are also valid `setImage` mime types.
pub fn build_image_payload(path: &Path) -> Result<String, IconEncodeError> {
    let is_svg = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"));

    if is_svg {
        return std::fs::read_to_string(path).map_err(IconEncodeError::Read);
    }

    let bytes = std::fs::read(path).map_err(IconEncodeError::Read)?;
    let mime = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("bmp") => "image/bmp",
        _ => "image/png",
    };
    let encoded = STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // A minimal valid 1x1 transparent PNG (67 bytes), small enough to embed
    // as a literal so this test needs no real icon theme on disk.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn encodes_a_raster_icon_as_a_base64_data_uri() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.png");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(TINY_PNG)
            .unwrap();

        let payload = build_image_payload(&path).unwrap();

        let prefix = "data:image/png;base64,";
        assert!(payload.starts_with(prefix), "got: {payload}");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&payload[prefix.len()..])
            .unwrap();
        assert_eq!(decoded, TINY_PNG);
    }

    #[test]
    fn picks_jpeg_mime_for_a_jpg_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.jpg");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"not-really-a-jpeg")
            .unwrap();

        let payload = build_image_payload(&path).unwrap();

        assert!(payload.starts_with("data:image/jpeg;base64,"));
    }

    #[test]
    fn passes_svg_through_as_raw_markup_unencoded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        std::fs::write(&path, svg).unwrap();

        let payload = build_image_payload(&path).unwrap();

        assert_eq!(payload, svg);
    }

    #[test]
    fn reports_an_error_for_a_missing_file() {
        let result = build_image_payload(Path::new("/nonexistent/icon.png"));
        assert!(matches!(result, Err(IconEncodeError::Read(_))));
    }
}

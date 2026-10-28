//! QR codes as `data:` PNG images, the kind every hyperb renderer shows.

use base64::Engine;
use qrcode::{Color, QrCode};

/// Pixels per module.
const SCALE: usize = 8;
/// Modules of white around the code, as the standard asks.
const QUIET: usize = 4;

/// `text` as a QR code in a `data:image/png;base64,…` URI.
pub fn data_uri(text: &str) -> Option<String> {
    let code = QrCode::new(text.as_bytes()).ok()?;
    let modules = code.width();
    let side = (modules + 2 * QUIET) * SCALE;
    let mut pixels = vec![0xffu8; side * side];
    for y in 0..modules {
        for x in 0..modules {
            if code[(x, y)] != Color::Dark {
                continue;
            }
            for dy in 0..SCALE {
                let row = ((y + QUIET) * SCALE + dy) * side;
                let start = row + (x + QUIET) * SCALE;
                pixels[start..start + SCALE].fill(0);
            }
        }
    }
    let mut png = vec![];
    let mut enc = png::Encoder::new(&mut png, side as u32, side as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().ok()?.write_image_data(&pixels).ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn makes_a_png() {
        let uri = super::data_uri("grassline:ab:cd").unwrap();
        let b64 = uri.strip_prefix("data:image/png;base64,").unwrap();
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }
}

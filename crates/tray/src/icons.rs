//! Embedded tray icons.

use ldtray::Icon;

use crate::view::Kind;

/// Width and height of each embedded icon, in pixels.
pub const SIZE: u32 = 48;

/// Return the icon for a tray state.
pub fn icon(kind: Kind) -> Icon {
    let rgba: &[u8] = match kind {
        Kind::Idle => include_bytes!("../icons/idle.rgba"),
        Kind::Recording => include_bytes!("../icons/recording.rgba"),
        Kind::Transcribing => include_bytes!("../icons/transcribing.rgba"),
        Kind::Error => include_bytes!("../icons/error.rgba"),
    };
    Icon::from_rgba(SIZE, SIZE, rgba.to_vec())
        .expect("embedded tray icon must contain 48x48 RGBA pixels")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_are_48x48_rgba() {
        for kind in [Kind::Idle, Kind::Recording, Kind::Transcribing, Kind::Error] {
            let icon = icon(kind);
            assert_eq!((icon.width(), icon.height()), (48, 48));
        }
    }
}

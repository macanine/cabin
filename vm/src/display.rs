//! Shared desktop geometry used by QEMU, Sway, Chrome and the display stream.
//!
//! `desktop_scale` is a compositor scale. Physical output pixels are divided
//! by that scale to obtain browser CSS pixels; it is not an extra CSS zoom.

use crate::{Result, message};

pub const MIN_WIDTH: u32 = 640;
pub const MIN_HEIGHT: u32 = 360;
pub const MAX_WIDTH: u32 = 8192;
pub const MAX_HEIGHT: u32 = 8192;
pub const MIN_SCALE: f64 = 1.0;
pub const MAX_SCALE: f64 = 4.0;
pub const WAYBAR_HEIGHT: u32 = 28;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayGeometry {
    physical_width: u32,
    physical_height: u32,
    scale: f64,
}

impl DisplayGeometry {
    pub fn parse(resolution: &str, scale: f64) -> Result<Self> {
        let normalized = resolution.trim().to_ascii_lowercase();
        let Some((width, height)) = normalized.split_once('x') else {
            return message("--desktop-resolution must look like 1600x900");
        };
        if height.contains('x') {
            return message("--desktop-resolution must look like 1600x900");
        }
        let width = width
            .parse()
            .map_err(|_| crate::Error::Message("invalid desktop width".into()))?;
        let height = height
            .parse()
            .map_err(|_| crate::Error::Message("invalid desktop height".into()))?;
        Self::new(width, height, scale)
    }

    pub fn new(width: u32, height: u32, scale: f64) -> Result<Self> {
        if !(MIN_WIDTH..=MAX_WIDTH).contains(&width) || !(MIN_HEIGHT..=MAX_HEIGHT).contains(&height)
        {
            return message(format!(
                "desktop resolution must be between {MIN_WIDTH}x{MIN_HEIGHT} and {MAX_WIDTH}x{MAX_HEIGHT}"
            ));
        }
        if !scale.is_finite() || !(MIN_SCALE..=MAX_SCALE).contains(&scale) {
            return message(format!(
                "desktop scale must be between {MIN_SCALE:.1} and {MAX_SCALE:.1}"
            ));
        }
        Ok(Self {
            physical_width: width,
            physical_height: height,
            scale,
        })
    }

    pub fn physical_size(self) -> (u32, u32) {
        (self.physical_width, self.physical_height)
    }

    pub fn logical_size(self) -> (u32, u32) {
        (
            ((self.physical_width as f64 / self.scale).floor() as u32).max(1),
            ((self.physical_height as f64 / self.scale).floor() as u32).max(1),
        )
    }

    /// The compositor uses `scale`; Waybar's height is already in logical
    /// pixels and must be subtracted after applying that scale.
    pub fn chrome_window_size(self) -> (u32, u32) {
        let (width, height) = self.logical_size();
        (width, height.saturating_sub(WAYBAR_HEIGHT).max(1))
    }

    pub fn scale(self) -> f64 {
        self.scale
    }
}

#[cfg(test)]
mod tests {
    use super::DisplayGeometry;

    #[test]
    fn waybar_is_subtracted_in_logical_pixels() {
        let geometry = DisplayGeometry::new(1920, 1080, 2.0).unwrap();
        assert_eq!(geometry.logical_size(), (960, 540));
        assert_eq!(geometry.chrome_window_size(), (960, 512));
    }

    #[test]
    fn fractional_pixels_are_never_rounded_above_the_output() {
        let geometry = DisplayGeometry::new(1718, 1036, 2.5).unwrap();
        assert_eq!(geometry.logical_size(), (687, 414));
        assert_eq!(geometry.chrome_window_size(), (687, 386));
    }

    #[test]
    fn invalid_geometry_is_rejected() {
        assert!(DisplayGeometry::parse("1600", 1.0).is_err());
        assert!(DisplayGeometry::parse("1600x900x2", 1.0).is_err());
        assert!(DisplayGeometry::new(639, 480, 1.0).is_err());
        assert!(DisplayGeometry::new(1600, 900, f64::NAN).is_err());
    }
}

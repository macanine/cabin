//! Reads the AppKit-drawn traffic lights in the macOS overlay title bar.

use serde::Serialize;

use crate::app_error::AppError;

/// Where the window controls are, in CSS pixels from the window's top-left.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub(crate) struct WindowChrome {
    /// Right edge of the last button (zoom): content must start after it.
    pub controls_right: f64,
    /// Vertical centre of the buttons: the title bar is centred on it.
    pub controls_center_y: f64,
}

#[cfg(any(target_os = "macos", test))]
impl WindowChrome {
    /// Converts AppKit's bottom-left window coordinates to CSS coordinates.
    fn from_frames(width: f64, height: f64, close: Rect, zoom: Rect) -> Option<Self> {
        if !width.is_finite()
            || !height.is_finite()
            || !close.fits(width, height)
            || !zoom.fits(width, height)
        {
            return None;
        }

        let controls_right = zoom.x + zoom.width;
        let controls_center_y = height - close.y - close.height / 2.0;
        (controls_right > 0.0 && controls_center_y > 0.0).then_some(Self {
            controls_right,
            controls_center_y,
        })
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[cfg(any(target_os = "macos", test))]
impl Rect {
    fn fits(self, width: f64, height: f64) -> bool {
        self.x >= 0.0
            && self.y >= 0.0
            && self.width > 0.0
            && self.height > 0.0
            && self.x + self.width <= width
            && self.y + self.height <= height
    }
}

/// The traffic lights' geometry, or `None` when the overlay has no visible controls.
#[tauri::command]
pub(crate) async fn window_chrome(
    window: tauri::WebviewWindow,
) -> Result<Option<WindowChrome>, AppError> {
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let target = window.clone();
        window
            .run_on_main_thread(move || {
                let _ = tx.send(macos::measure(&target));
            })
            .map_err(|error| AppError::internal(error.to_string()))?;
        tauri::async_runtime::spawn_blocking(move || {
            rx.recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|error| AppError::internal(error.to_string()))
        })
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2_app_kit::{NSView, NSWindow, NSWindowButton, NSWindowStyleMask};

    use super::{Rect, WindowChrome};

    /// Must run on the main thread (AppKit).
    pub(super) fn measure(window: &tauri::WebviewWindow) -> Option<WindowChrome> {
        let pointer = window.ns_window().ok()?.cast::<NSWindow>();
        // SAFETY: Tauri hands out the live NSWindow of this window. This call
        // runs on the main thread and the borrow ends here.
        let ns_window = unsafe { pointer.as_ref() }?;
        if ns_window
            .styleMask()
            .contains(NSWindowStyleMask::FullScreen)
        {
            return None;
        }

        let close = ns_window.standardWindowButton(NSWindowButton::CloseButton)?;
        let zoom = ns_window.standardWindowButton(NSWindowButton::ZoomButton)?;
        let frame = ns_window.frame();
        WindowChrome::from_frames(
            frame.size.width,
            frame.size.height,
            visible_frame(&close, ns_window)?,
            visible_frame(&zoom, ns_window)?,
        )
    }

    fn visible_frame(view: &NSView, window: &NSWindow) -> Option<Rect> {
        if view.isHiddenOrHasHiddenAncestor() || !std::ptr::eq(view.window()?.as_ref(), window) {
            return None;
        }
        let rect = view.convertRect_toView(view.bounds(), None);
        Some(Rect {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOSE: Rect = Rect {
        x: 14.0,
        y: 771.5,
        width: 14.0,
        height: 16.0,
    };
    const ZOOM: Rect = Rect { x: 54.5, ..CLOSE };

    #[test]
    fn frames_are_flipped_to_top_left_css_coordinates_in_points() {
        assert_eq!(
            WindowChrome::from_frames(1240.0, 800.0, CLOSE, ZOOM),
            Some(WindowChrome {
                controls_right: 68.5,
                controls_center_y: 20.5,
            })
        );
    }

    #[test]
    fn invalid_window_and_button_frames_are_rejected() {
        assert!(WindowChrome::from_frames(f64::INFINITY, 800.0, CLOSE, ZOOM).is_none());
        assert!(WindowChrome::from_frames(1240.0, 0.0, CLOSE, ZOOM).is_none());
        for frame in [
            Rect {
                x: f64::NAN,
                ..CLOSE
            },
            Rect {
                y: f64::INFINITY,
                ..CLOSE
            },
            Rect {
                width: 0.0,
                ..CLOSE
            },
            Rect {
                height: -1.0,
                ..CLOSE
            },
            Rect { y: 800.0, ..CLOSE },
        ] {
            assert!(WindowChrome::from_frames(1240.0, 800.0, frame, ZOOM).is_none());
            assert!(WindowChrome::from_frames(1240.0, 800.0, CLOSE, frame).is_none());
        }
    }
}

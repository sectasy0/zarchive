//! Captures and asynchronously blurs the window background behind modal dialogs.

pub const PASSWORD_WIDTH: f32 = 420.0;

use gpui::{IntoElement, ParentElement, RenderImage, Styled, div, img, prelude::*};
use std::sync::{Arc, Mutex};

#[derive(Default)]
/// Owns the current modal backdrop image and its cancellable processing state.
pub struct PasswordBlur {
    image: Option<Arc<RenderImage>>,
    task: Option<gpui::Task<()>>,
    revision: u64,
    pending: Arc<Mutex<Option<Arc<RenderImage>>>>,
}

/// Produces a bounded, low-resolution blur suitable for a modal backdrop.
fn blurred_frame(frame: &image::RgbaImage) -> image::RgbaImage {
    let scale = (480.0 / frame.width().max(frame.height()).max(1) as f32).min(0.5);
    let reduced = image::DynamicImage::ImageRgba8(frame.clone()).resize_exact(
        ((frame.width() as f32 * scale) as u32).max(1),
        ((frame.height() as f32 * scale) as u32).max(1),
        image::imageops::FilterType::Nearest,
    );
    reduced.fast_blur((8.0 * scale).max(0.5)).into_rgba8()
}

impl PasswordBlur {
    /// Reports whether a backdrop image is ready or still being processed.
    pub fn has_capture(&self) -> bool {
        self.image.is_some() || self.task.is_some()
    }

    /// Captures the current window contents and starts processing a new backdrop.
    pub fn capture(
        &mut self,
        window: &gpui::Window,
        cx: &mut gpui::Context<crate::ArchiveManager>,
    ) {
        self.clear();
        if let Ok(frame) = window.render_to_image() {
            if frame.width() == 0 || frame.height() == 0 {
                return;
            }
            self.start_frame(frame, window, cx);
        }
    }

    /// Converts a blurred RGBA frame into the renderer's image representation.
    fn render_frame(frame: &image::RgbaImage) -> Arc<RenderImage> {
        let mut blurred = blurred_frame(frame);
        for pixel in blurred.pixels_mut() {
            pixel.0.swap(0, 2);
        }
        Arc::new(RenderImage::new(vec![image::Frame::new(blurred)]))
    }

    /// Blurs a captured frame on a background worker and schedules its delivery.
    fn start_frame(
        &mut self,
        frame: image::RgbaImage,
        window: &gpui::Window,
        cx: &mut gpui::Context<crate::ArchiveManager>,
    ) {
        let pending = self.pending.clone();
        self.task = Some(cx.background_executor().spawn(async move {
            let image = Self::render_frame(&frame);
            *pending.lock().unwrap_or_else(|error| error.into_inner()) = Some(image);
        }));
        self.poll_next_frame(window, cx);
    }

    /// Checks for a completed blur on the next frame without re-entering the app cell.
    fn poll_next_frame(
        &self,
        window: &gpui::Window,
        cx: &mut gpui::Context<crate::ArchiveManager>,
    ) {
        let revision = self.revision;
        let view = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = view.update(cx, |manager, cx| {
                if manager.password_blur.revision != revision || manager.modal.is_none() {
                    return;
                }
                let image = manager
                    .password_blur
                    .pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                if let Some(image) = image {
                    manager.password_blur.image = Some(image);
                    manager.password_blur.task = None;
                    cx.notify();
                } else {
                    manager.password_blur.poll_next_frame(window, cx);
                }
            });
        });
    }

    /// Cancels pending work and discards both ready and unpublished images.
    pub fn clear(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.task = None;
        self.pending = Arc::default();
        self.image = None;
    }

    /// Renders the captured image as a full-window modal backdrop when available.
    pub fn view(&self) -> impl IntoElement {
        div()
            .id("password-blurred-background")
            .debug_selector(|| "modal-blurred-background".to_owned())
            .absolute()
            .inset_0()
            .size_full()
            .occlude()
            .when_some(self.image.clone(), |this, image| {
                this.child(img(image).size_full().object_fit(gpui::ObjectFit::Fill))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Keeps blur processing bounded for large and degenerate input dimensions.
    fn blur_working_image_is_bounded_for_retina_and_large_windows() {
        for (width, height) in [(2200, 1440), (3840, 2160), (1, 1), (1, 4096)] {
            let frame =
                image::RgbaImage::from_pixel(width, height, image::Rgba([60, 90, 120, 255]));
            let result = blurred_frame(&frame);
            assert!(result.width() > 0 && result.width() <= 480);
            assert!(result.height() > 0 && result.height() <= 480);
            assert_eq!(result.get_pixel(0, 0).0, [60, 90, 120, 255]);
        }
    }

    #[test]
    #[ignore = "manual comparison of blur processing time"]
    /// Compares the current blur path with the former full-resolution approach.
    fn compare_retina_blur_processing_time() {
        let frame = image::RgbaImage::from_pixel(2200, 1440, image::Rgba([60, 90, 120, 255]));
        let start = std::time::Instant::now();
        let previous =
            image::imageops::resize(&frame, 1100, 720, image::imageops::FilterType::Triangle);
        let previous = image::imageops::blur(&previous, 4.0);
        let previous_time = start.elapsed();
        let start = std::time::Instant::now();
        let current = blurred_frame(&frame);
        let current_time = start.elapsed();
        println!(
            "CPU blur, 2200x1440: previous={previous_time:?}, current={current_time:?}, speedup={:.1}x; working pixels {} -> {}",
            previous_time.as_secs_f64() / current_time.as_secs_f64(),
            previous.width() * previous.height(),
            current.width() * current.height()
        );
    }

    #[gpui::test]
    /// Verifies async delivery after opening a modal and cancellation when it closes.
    fn blur_runs_after_dialog_opens_and_discards_cancelled_work(cx: &mut gpui::TestAppContext) {
        let (view, cx) = crate::tests::component_window(cx, |window, cx| {
            let mut manager = crate::tests::test_manager(cx);
            manager.current_directory = None;
            manager.show_modal(crate::Modal::About, window, cx);
            manager
        });
        assert!(cx.debug_bounds("dialog-0").is_some());
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.password_blur.start_frame(
                    image::RgbaImage::from_pixel(128, 128, image::Rgba([60, 90, 120, 255])),
                    window,
                    cx,
                );
                assert!(manager.password_blur.task.is_some());
                assert!(manager.password_blur.image.is_none());
            });
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.password_blur.image.is_none());
            assert!(manager.password_blur.pending.lock().unwrap().is_some());
        });
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.password_blur.image.is_some());
            assert!(manager.password_blur.task.is_none());
            assert!(manager.modal == Some(crate::Modal::About));
        });
        cx.update(|window, cx| {
            view.update(cx, |manager, cx| {
                manager.password_blur.clear();
                manager.password_blur.start_frame(
                    image::RgbaImage::from_pixel(128, 128, image::Rgba([120, 90, 60, 255])),
                    window,
                    cx,
                );
                manager.close_modal(window, cx);
            });
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let manager = view.read(cx);
            assert!(manager.password_blur.image.is_none());
            assert!(manager.password_blur.task.is_none());
            assert!(manager.modal.is_none());
        });
    }

    #[test]
    /// Checks that blurring mixes neighboring colors and preserves channel ordering.
    fn blur_softens_content_and_preserves_pixel_channels() {
        let mut frame = image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 0, 0, 255]));
        for row in 0..64 {
            for column in 32..64 {
                frame.put_pixel(column, row, image::Rgba([0, 0, 255, 255]));
            }
        }
        let blurred = blurred_frame(&frame);
        let middle = blurred.get_pixel(15, 16);
        assert!(middle[0] > 0 && middle[0] < 255);
        assert!(middle[2] > 0 && middle[2] < 255);
        assert_eq!(middle[3], 255);
        let mut blur = PasswordBlur::default();
        blur.image = Some(PasswordBlur::render_frame(&frame));
        let image = blur.image.as_ref().unwrap();
        assert_eq!(&image.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
        blur.clear();
        assert!(blur.image.is_none());
    }
}

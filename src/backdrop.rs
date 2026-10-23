// Blurred screenshot behind the power menu and polkit dialog. The overlay stays transparent
// until the capture is in, so the capture shows only the desktop.
use std::{cell::Cell, rc::Rc, time::Duration};

use gtk::{gdk, gio, glib, prelude::*};

use crate::{ppm, util};

pub fn new() -> gtk::Overlay {
    let overlay = gtk::Overlay::new();
    let picture = gtk::Picture::new();
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.set_can_shrink(true);
    picture.add_css_class("backdrop-image");
    overlay.set_child(Some(&picture));
    let dim = gtk::Box::new(gtk::Orientation::Vertical, 0);
    dim.add_css_class("backdrop-dim");
    overlay.add_overlay(&dim);

    // Bumped per map, so a late capture or timeout from an earlier one does nothing
    let generation = Rc::new(Cell::new(0u32));
    overlay.connect_map(move |overlay| {
        let current = generation.get() + 1;
        generation.set(current);
        overlay.set_opacity(0.0);
        picture.set_paintable(None::<&gdk::Paintable>);

        let (timeout, shown, latest) = (overlay.clone(), overlay.clone(), generation.clone());
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            if latest.get() == current {
                timeout.set_opacity(1.0);
            }
        });
        let (picture, latest) = (picture.clone(), generation.clone());
        let Some(window) = overlay.root().and_downcast::<gtk::Window>() else { return };
        glib::spawn_future_local(async move {
            let texture = capture(&window).await;
            if latest.get() != current {
                return;
            }
            if let Some(texture) = texture {
                picture.set_paintable(Some(&texture));
            }
            shown.set_opacity(1.0);
        });
    });
    overlay
}

// grim -s takes ~250ms at 2560x1440, a full-size PPM ~25ms, so shrink and blur here. GTK's CSS
// blur also left the screen edges sharp.
const SHRINK: usize = 4;
const BLUR_RADIUS: usize = 3;

async fn capture(window: &gtk::Window) -> Option<gdk::Texture> {
    let mut monitor = util::window_monitor(window);
    for _ in 0..40 {
        if monitor.is_some() {
            break;
        }
        glib::timeout_future(Duration::from_millis(5)).await;
        monitor = util::window_monitor(window);
    }
    let connector = monitor?.connector()?;
    let ppm = util::output_bytes(&["grim", "-t", "ppm", "-o", &connector, "-"]).await?;
    let (width, height, rgb) = gio::spawn_blocking(move || {
        let (width, height, mut rgb) = ppm::shrink(&ppm, SHRINK)?;
        ppm::blur(width, height, &mut rgb, BLUR_RADIUS);
        Some((width, height, rgb))
    })
    .await
    .ok()??;
    let bytes = glib::Bytes::from_owned(rgb);
    let texture = gdk::MemoryTexture::new(
        width as i32,
        height as i32,
        gdk::MemoryFormat::R8g8b8,
        &bytes,
        width * 3,
    );
    Some(texture.upcast())
}

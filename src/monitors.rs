use gtk::{gdk, gio, prelude::*};

pub fn all() -> Vec<gdk::Monitor> {
    let Some(display) = gdk::Display::default() else {
        return Vec::new();
    };
    let monitors = display.monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>())
        .collect()
}

pub fn on_change(f: impl Fn() + 'static) {
    if let Some(display) = gdk::Display::default() {
        let monitors: gio::ListModel = display.monitors();
        monitors.connect_items_changed(move |_, _, _, _| f());
    }
}

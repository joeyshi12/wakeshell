// Volume popup, shown by `wakeshell volume` and `wakeshell msg osd volume`.
use std::{
    cell::RefCell,
    rc::Rc,
    time::Duration,
};

use gtk::{glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::{icon::Icon, util};

const SHADOW: i32 = 5;

struct Ui {
    window: gtk::Window,
    card: gtk::Box,
    icon: Icon,
    bar: gtk::ProgressBar,
    value: gtk::Label,
}

pub struct Osd {
    ui: Ui,
    hide: RefCell<Option<glib::SourceId>>,
}

impl Osd {
    pub fn new() -> Rc<Osd> {
        let window = util::layer_window(util::LayerOptions {
            namespace: "wakeshell-osd",
            layer: Layer::Overlay,
            anchors: [false, true, false, false],
            keyboard: KeyboardMode::None,
            monitor: None,
            exclusive_zone: -1,
        });
        window.set_margin(Edge::Bottom, 80);

        let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        card.add_css_class("card");
        card.add_css_class("osd");
        card.set_size_request(320, 56);
        card.set_margin_end(SHADOW);
        card.set_margin_bottom(SHADOW);
        let icon = Icon::new("volume-2", 22);
        icon.set_valign(gtk::Align::Center);
        card.append(&icon);
        let bar = gtk::ProgressBar::new();
        bar.set_hexpand(true);
        bar.set_valign(gtk::Align::Center);
        card.append(&bar);
        let value = util::label("", &[]);
        value.set_xalign(1.0);
        value.set_width_chars(5);
        card.append(&value);
        window.set_child(Some(&card));

        // Let clicks through to whatever is underneath
        window.connect_realize(|window| {
            if let Some(surface) = window.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
            }
        });

        Rc::new(Osd { ui: Ui { window, card, icon, bar, value }, hide: RefCell::new(None) })
    }

    pub fn volume(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let output = util::output(&["wpctl", "get-volume", "@DEFAULT_AUDIO_SINK@"]).await;
            if let (Some(osd), Some(output)) = (weak.upgrade(), output) {
                osd.show(&output);
            }
        });
    }

    fn show(self: &Rc<Self>, output: &str) {
        let volume: f64 = output.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let muted = output.contains("[MUTED]");
        let ui = &self.ui;
        ui.icon.set_icon(if muted || volume <= 0.0 {
            "volume-x"
        } else if volume < 0.34 {
            "volume"
        } else if volume < 0.67 {
            "volume-1"
        } else {
            "volume-2"
        });
        ui.bar.set_fraction(volume.min(1.0));
        ui.value.set_text(&if muted { "Muted".to_owned() } else { format!("{}%", (volume * 100.0).round()) });
        if muted {
            ui.card.add_css_class("muted-osd");
        } else {
            ui.card.remove_css_class("muted-osd");
        }
        ui.window.present();

        if let Some(id) = self.hide.take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(1600), move || {
            if let Some(osd) = weak.upgrade() {
                osd.hide.take();
                osd.ui.window.set_visible(false);
            }
        });
        self.hide.replace(Some(id));
    }
}

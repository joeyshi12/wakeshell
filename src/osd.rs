// Volume popup, shown by `wakeshell volume` and `wakeshell msg osd volume`.
use std::time::Duration;

use gtk::{glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use relm4::{RelmWidgetExt, prelude::*};

use crate::{icon::Icon, util};

const SHADOW: i32 = 5;

pub struct Osd {
    shown: bool,
    volume: f64,
    muted: bool,
    hide: Option<glib::SourceId>,
}

#[derive(Debug)]
pub enum OsdMsg {
    Volume,
    Show(String),
    Hide,
}

impl Osd {
    fn icon(&self) -> &'static str {
        if self.muted || self.volume <= 0.0 {
            "volume-x"
        } else if self.volume < 0.34 {
            "volume"
        } else if self.volume < 0.67 {
            "volume-1"
        } else {
            "volume-2"
        }
    }

    fn text(&self) -> String {
        if self.muted { "Muted".to_owned() } else { format!("{}%", (self.volume * 100.0).round()) }
    }
}

#[relm4::component(pub)]
impl SimpleComponent for Osd {
    type Init = ();
    type Input = OsdMsg;
    type Output = ();

    view! {
        util::layer_window(util::LayerOptions {
            namespace: "wakeshell-osd",
            layer: Layer::Overlay,
            anchors: [false, true, false, false],
            keyboard: KeyboardMode::None,
            monitor: None,
            exclusive_zone: -1,
        }) -> gtk::Window {
            set_margin: (Edge::Bottom, 80),
            #[watch]
            set_visible: model.shown,
            // Let clicks through to whatever is underneath
            connect_realize => |window| {
                if let Some(surface) = window.surface() {
                    surface.set_input_region(Some(&gtk::cairo::Region::create()));
                }
            },

            gtk::Box {
                set_spacing: 12,
                set_css_classes: &["card", "osd"],
                #[watch]
                set_class_active: ("muted-osd", model.muted),
                set_size_request: (320, 56),
                set_margin_end: SHADOW,
                set_margin_bottom: SHADOW,

                Icon::new("volume-2", 22) {
                    set_valign: gtk::Align::Center,
                    #[watch]
                    set_icon: model.icon(),
                },
                gtk::ProgressBar {
                    set_hexpand: true,
                    set_valign: gtk::Align::Center,
                    #[watch]
                    set_fraction: model.volume.min(1.0),
                },
                gtk::Label {
                    set_xalign: 1.0,
                    set_width_chars: 5,
                    #[watch]
                    set_label: &model.text(),
                },
            },
        }
    }

    fn init(_: (), root: Self::Root, _sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let model = Osd { shown: false, volume: 0.0, muted: false, hide: None };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: OsdMsg, sender: ComponentSender<Self>) {
        match msg {
            OsdMsg::Volume => {
                let sender = sender.clone();
                glib::spawn_future_local(async move {
                    if let Some(output) = util::output(&["wpctl", "get-volume", "@DEFAULT_AUDIO_SINK@"]).await {
                        sender.input(OsdMsg::Show(output));
                    }
                });
            }
            OsdMsg::Show(output) => {
                self.volume = output.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0.0);
                self.muted = output.contains("[MUTED]");
                self.shown = true;
                if let Some(id) = self.hide.take() {
                    id.remove();
                }
                let sender = sender.clone();
                self.hide = Some(glib::timeout_add_local_once(Duration::from_millis(1600), move || {
                    sender.input(OsdMsg::Hide)
                }));
            }
            OsdMsg::Hide => {
                self.hide = None;
                self.shown = false;
            }
        }
    }
}

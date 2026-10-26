// Wallpaper on every monitor, over the background colour. `wallpaper reload` reads the file again.
use std::{rc::Rc, time::Duration};

use gtk::{gdk, glib, prelude::*};
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
use relm4::prelude::*;

use crate::{config::Config, monitors, util};

const FADE_MS: u32 = 400;

pub struct Wallpaper {
    config: Rc<Config>,
    screens: Vec<Controller<Screen>>,
    texture: Option<gdk::Texture>,
}

#[derive(Debug)]
pub enum WallpaperMsg {
    Reload,
    MonitorsChanged,
    Loaded(gdk::Texture),
}

impl Component for Wallpaper {
    type Init = Rc<Config>;
    type Input = WallpaperMsg;
    type Output = ();
    type CommandOutput = ();
    type Root = ();
    type Widgets = ();

    fn init_root() {}

    fn init(config: Rc<Config>, _: (), sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let mut model = Wallpaper { config, screens: Vec::new(), texture: None };
        model.build();
        let monitors = sender.clone();
        monitors::on_change(move || monitors.input(WallpaperMsg::MonitorsChanged));
        sender.input(WallpaperMsg::Reload);
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, msg: WallpaperMsg, sender: ComponentSender<Self>, _: &()) {
        match msg {
            WallpaperMsg::MonitorsChanged => self.build(),
            WallpaperMsg::Reload => self.reload(sender),
            WallpaperMsg::Loaded(texture) => {
                self.texture = Some(texture.clone());
                for screen in &self.screens {
                    screen.emit(ScreenMsg::Show(texture.clone()));
                }
            }
        }
    }
}

impl Wallpaper {
    fn build(&mut self) {
        self.screens.clear();
        for monitor in monitors::all() {
            let screen = Screen::builder().launch((monitor, self.texture.clone())).detach();
            self.screens.push(screen);
        }
    }

    fn reload(&self, sender: ComponentSender<Self>) {
        let Some(path) = self.config.wallpaper.clone() else {
            return;
        };
        let (texture_sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || match gdk::Texture::from_filename(&path) {
            Ok(texture) => {
                let _ = texture_sender.send_blocking(texture);
            }
            Err(e) => eprintln!("wakeshell: cannot load the wallpaper {}: {e}", path.display()),
        });
        glib::spawn_future_local(async move {
            if let Ok(texture) = receiver.recv().await {
                sender.input(WallpaperMsg::Loaded(texture));
            }
        });
    }
}

struct Screen {
    window: gtk::Window,
    pictures: [Option<gdk::Texture>; 2],
    front: usize,
    /// Bumped per fade so an earlier fade does not clear the new back picture
    fade: u32,
}

#[derive(Debug)]
enum ScreenMsg {
    Show(gdk::Texture),
    ClearBack(u32),
}

#[relm4::component]
impl SimpleComponent for Screen {
    type Init = (gdk::Monitor, Option<gdk::Texture>);
    type Input = ScreenMsg;
    type Output = ();

    view! {
        util::layer_window(util::LayerOptions {
            namespace: "wakeshell-wallpaper",
            layer: Layer::Background,
            anchors: [true; 4],
            keyboard: KeyboardMode::None,
            monitor: None,
            exclusive_zone: -1,
        }) -> gtk::Window {
            add_css_class: "wallpaper",

            gtk::Stack {
                set_transition_type: gtk::StackTransitionType::Crossfade,
                set_transition_duration: FADE_MS,
                add_named[Some("first")] = &gtk::Picture {
                    set_content_fit: gtk::ContentFit::Cover,
                    set_can_shrink: true,
                    #[watch]
                    set_paintable: model.pictures[0].as_ref(),
                },
                add_named[Some("second")] = &gtk::Picture {
                    set_content_fit: gtk::ContentFit::Cover,
                    set_can_shrink: true,
                    #[watch]
                    set_paintable: model.pictures[1].as_ref(),
                },
                // After the children, which it names
                #[watch]
                set_visible_child_name: if model.front == 0 { "first" } else { "second" },
            },
        }
    }

    fn init(
        (monitor, texture): Self::Init,
        root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = Screen { window: root.clone(), pictures: [texture, None], front: 0, fade: 0 };
        let widgets = view_output!();
        root.set_monitor(Some(&monitor));
        root.present();
        ComponentParts { model, widgets }
    }

    // After the last update, so nothing touches the window once it is gone
    fn shutdown(&mut self, _: &mut Self::Widgets, _: relm4::Sender<Self::Output>) {
        self.window.destroy();
    }

    fn update(&mut self, msg: ScreenMsg, sender: ComponentSender<Self>) {
        match msg {
            ScreenMsg::Show(texture) if self.pictures[self.front].is_none() => {
                self.pictures[self.front] = Some(texture);
            }
            ScreenMsg::Show(texture) => {
                self.front = 1 - self.front;
                self.pictures[self.front] = Some(texture);
                self.fade += 1;
                let fade = self.fade;
                glib::timeout_add_local_once(Duration::from_millis(FADE_MS as u64 + 100), move || {
                    // The monitor may be gone by now
                    let _ = sender.input_sender().send(ScreenMsg::ClearBack(fade));
                });
            }
            ScreenMsg::ClearBack(fade) if fade == self.fade => self.pictures[1 - self.front] = None,
            ScreenMsg::ClearBack(_) => {}
        }
    }
}

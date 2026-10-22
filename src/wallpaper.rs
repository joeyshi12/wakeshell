// Wallpaper on every monitor. `wallpaper reload` fades to the image the link points at now.
use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Duration};

use gtk::{gdk, glib, prelude::*};
use gtk4_layer_shell::{KeyboardMode, Layer};

use crate::{config::Config, monitors, util};

const FADE_MS: u32 = 400;

struct Screen {
    window: gtk::Window,
    stack: gtk::Stack,
    pictures: [gtk::Picture; 2],
}

pub struct Wallpaper {
    config: Rc<Config>,
    screens: RefCell<Vec<Screen>>,
    texture: RefCell<Option<gdk::Texture>>,
}

impl Wallpaper {
    pub fn new(config: Rc<Config>) -> Rc<Wallpaper> {
        let wallpaper = Rc::new(Wallpaper {
            config,
            screens: RefCell::new(Vec::new()),
            texture: RefCell::new(None),
        });
        wallpaper.build();
        let weak = Rc::downgrade(&wallpaper);
        monitors::on_change(move || {
            if let Some(wallpaper) = weak.upgrade() {
                wallpaper.build();
            }
        });
        wallpaper.reload();
        wallpaper
    }

    fn build(&self) {
        for screen in self.screens.borrow_mut().drain(..) {
            screen.window.destroy();
        }
        let texture = self.texture.borrow().clone();
        for monitor in monitors::all() {
            let window = util::layer_window(util::LayerOptions {
                namespace: "wakeshell-wallpaper",
                layer: Layer::Background,
                anchors: [true; 4],
                keyboard: KeyboardMode::None,
                monitor: Some(&monitor),
                exclusive_zone: -1,
            });
            window.add_css_class("wallpaper");
            let stack = gtk::Stack::new();
            stack.set_transition_type(gtk::StackTransitionType::Crossfade);
            stack.set_transition_duration(FADE_MS);
            let pictures = [gtk::Picture::new(), gtk::Picture::new()];
            for picture in &pictures {
                picture.set_content_fit(gtk::ContentFit::Cover);
                picture.set_can_shrink(true);
                stack.add_child(picture);
            }
            pictures[0].set_paintable(texture.as_ref());
            window.set_child(Some(&stack));
            window.present();
            self.screens.borrow_mut().push(Screen { window, stack, pictures });
        }
    }

    pub fn reload(self: &Rc<Self>) {
        let paths: Vec<PathBuf> =
            std::iter::once(self.config.wallpaper.clone()).chain(self.config.wallpaper_fallback.clone()).collect();
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let mut texture = None;
            for path in &paths {
                match gdk::Texture::from_filename(path) {
                    Ok(t) => {
                        texture = Some(t);
                        break;
                    }
                    Err(e) => eprintln!("wakeshell: cannot load the wallpaper {}: {e}", path.display()),
                }
            }
            let _ = sender.send_blocking(texture);
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(Some(texture)) = receiver.recv().await else {
                return;
            };
            if let Some(wallpaper) = weak.upgrade() {
                wallpaper.show(texture);
            }
        });
    }

    fn show(&self, texture: gdk::Texture) {
        let first = self.texture.replace(Some(texture.clone())).is_none();
        for screen in self.screens.borrow().iter() {
            if first {
                screen.pictures[0].set_paintable(Some(&texture));
                continue;
            }
            let visible = screen.stack.visible_child();
            let back = if visible.as_ref() == Some(screen.pictures[0].upcast_ref()) {
                &screen.pictures[1]
            } else {
                &screen.pictures[0]
            };
            back.set_paintable(Some(&texture));
            screen.stack.set_visible_child(back);
            let front = back.clone();
            let old = if front == screen.pictures[0] { screen.pictures[1].clone() } else { screen.pictures[0].clone() };
            let stack = screen.stack.clone();
            glib::timeout_add_local_once(Duration::from_millis(FADE_MS as u64 + 100), move || {
                if stack.visible_child().as_ref() == Some(front.upcast_ref()) {
                    old.set_paintable(None::<&gdk::Paintable>);
                }
            });
        }
    }
}

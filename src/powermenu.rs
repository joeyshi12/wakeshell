// Power menu over a blurred copy of the screen.
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gtk::{gdk, prelude::*};

use crate::{backdrop, config::Config, icon::Icon, util};

struct Ui {
    window: gtk::Window,
    tiles: Vec<gtk::Box>,
    current: Cell<Option<usize>>,
}

pub struct PowerMenu {
    options: Vec<(&'static str, &'static str, Vec<String>)>,
    on_open: Rc<dyn Fn()>,
    ui: RefCell<Option<Rc<Ui>>>,
}

impl PowerMenu {
    pub fn new(config: Rc<Config>, on_open: Rc<dyn Fn()>) -> Rc<PowerMenu> {
        let options = vec![
            ("lock", "Lock", config.lock.clone()),
            ("log-out", "Log out", config.logout.clone()),
            ("moon", "Suspend", config.suspend.clone()),
            ("rotate-cw", "Reboot", config.reboot.clone()),
            ("power", "Shut down", config.poweroff.clone()),
        ];
        Rc::new(PowerMenu { options, on_open, ui: RefCell::new(None) })
    }

    pub fn shown(&self) -> bool {
        self.ui.borrow().is_some()
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.shown() {
            self.close();
        } else {
            self.open();
        }
    }

    pub fn close(&self) {
        if let Some(ui) = self.ui.take() {
            ui.window.destroy();
        }
    }

    fn run(&self, index: usize) {
        self.close();
        util::spawn(&self.options[index].2);
    }

    fn set_current(&self, index: Option<usize>) {
        let Some(ui) = self.ui.borrow().clone() else { return };
        for (i, tile) in ui.tiles.iter().enumerate() {
            if Some(i) == index {
                tile.add_css_class("current");
            } else {
                tile.remove_css_class("current");
            }
        }
        ui.current.set(index);
    }

    pub fn open(self: &Rc<Self>) {
        if self.shown() {
            return;
        }
        (self.on_open)();
        let window = util::overlay_window("wakeshell-powermenu");

        let content = gtk::Box::new(gtk::Orientation::Vertical, 32);
        content.set_halign(gtk::Align::Center);
        content.set_valign(gtk::Align::Center);
        content.set_opacity(0.0);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 28);
        content.append(&row);
        let mut tiles = Vec::new();
        for (i, (icon, label, _)) in self.options.iter().enumerate() {
            let tile = gtk::Box::new(gtk::Orientation::Vertical, 10);
            tile.add_css_class("card");
            tile.add_css_class("power-tile");
            let inner = gtk::Box::new(gtk::Orientation::Vertical, 10);
            inner.set_valign(gtk::Align::Center);
            inner.set_vexpand(true);
            let icon = Icon::new(icon, 44);
            icon.set_halign(gtk::Align::Center);
            inner.append(&icon);
            inner.append(&util::label(label, &["small", "bold"]));
            tile.append(&inner);
            util::hand_cursor(&tile);

            let weak = Rc::downgrade(self);
            util::on_pointer_moved(&tile, move |_, _| {
                if let Some(menu) = weak.upgrade() {
                    menu.set_current(Some(i));
                }
            });
            let leave = gtk::EventControllerMotion::new();
            let weak = Rc::downgrade(self);
            leave.connect_leave(move |_| {
                let Some(menu) = weak.upgrade() else { return };
                let current = menu.ui.borrow().as_ref().and_then(|ui| ui.current.get());
                if current == Some(i) {
                    menu.set_current(None);
                }
            });
            tile.add_controller(leave);
            let weak = Rc::downgrade(self);
            util::on_click(&tile, move |button| {
                if button == 1
                    && let Some(menu) = weak.upgrade() {
                        menu.run(i);
                    }
            });
            row.append(&tile);
            tiles.push(tile);
        }

        let seconds = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .unwrap_or(0.0) as u64;
        let uptime = format!("Up {}h {}m. Esc to cancel.", seconds / 3600, seconds % 3600 / 60);
        content.append(&util::label(&uptime, &["muted"]));

        let shown = content.clone();
        let root = backdrop::wrap(&window, &content, move || shown.set_opacity(1.0));
        window.set_child(Some(&root));

        let weak = Rc::downgrade(self);
        let inside: Vec<gtk::Widget> = tiles.iter().map(|t| t.clone().upcast()).collect();
        util::on_click_outside(&root, &inside, move || {
            if let Some(menu) = weak.upgrade() {
                menu.close();
            }
        });

        let weak = Rc::downgrade(self);
        let n = self.options.len();
        util::on_key(&window, move |key, _| {
            let Some(menu) = weak.upgrade() else { return false };
            let current = menu.ui.borrow().as_ref().and_then(|ui| ui.current.get());
            match key {
                gdk::Key::Escape => menu.close(),
                gdk::Key::Right | gdk::Key::Tab | gdk::Key::l => {
                    menu.set_current(Some(current.map_or(0, |c| (c + 1) % n)))
                }
                gdk::Key::Left | gdk::Key::ISO_Left_Tab | gdk::Key::h => {
                    menu.set_current(Some(current.map_or(n - 1, |c| (c + n - 1) % n)))
                }
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    if let Some(current) = current {
                        menu.run(current);
                    }
                }
                _ => return false,
            }
            true
        });

        self.ui.replace(Some(Rc::new(Ui { window: window.clone(), tiles, current: Cell::new(None) })));
        window.present();
    }
}

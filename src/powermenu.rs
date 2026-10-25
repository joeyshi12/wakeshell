// Power menu over a blurred copy of the screen.
use std::rc::Rc;

use gtk::{gdk, prelude::*};
use relm4::{RelmWidgetExt, factory::FactoryVecDeque, prelude::*};

use crate::{
    anim::Fade,
    backdrop,
    config::Config,
    icon::Icon,
    util::{self, Events},
};

pub struct PowerMenu {
    commands: Vec<Vec<String>>,
    shown: bool,
    tiles: FactoryVecDeque<Tile>,
    current: Option<usize>,
    uptime: String,
    fade: Option<Fade>,
    /// Picking an action closes at once; Esc fades out
    instant: bool,
}

#[derive(Debug)]
pub enum PowerMenuMsg {
    Open,
    Toggle,
    Close,
    Hover(usize),
    Leave(usize),
    Next,
    Previous,
    Activate,
    Run(usize),
}

#[derive(Debug)]
pub struct Shown(pub bool);

fn key_action(key: gdk::Key) -> Option<PowerMenuMsg> {
    Some(match key {
        gdk::Key::Escape => PowerMenuMsg::Close,
        gdk::Key::Right | gdk::Key::Tab | gdk::Key::l => PowerMenuMsg::Next,
        gdk::Key::Left | gdk::Key::ISO_Left_Tab | gdk::Key::h => PowerMenuMsg::Previous,
        gdk::Key::Return | gdk::Key::KP_Enter => PowerMenuMsg::Activate,
        _ => return None,
    })
}

#[relm4::component(pub)]
impl SimpleComponent for PowerMenu {
    type Init = Rc<Config>;
    type Input = PowerMenuMsg;
    type Output = Shown;

    view! {
        util::overlay_window("wakeshell-powermenu") -> gtk::Window {
            set_visible: false,
            on_key[sender] => move |key, _| {
                let action = key_action(key);
                let handled = action.is_some();
                if let Some(action) = action {
                    sender.input(action);
                }
                handled
            },

            #[name = "backdrop"]
            #[wrap(Some)]
            set_child = &backdrop::new() -> gtk::Overlay {
                add_overlay = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 32,
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Center,

                    #[local_ref]
                    tiles -> gtk::Box {
                        set_spacing: 28,
                    },
                    gtk::Label {
                        add_css_class: "muted",
                        #[watch]
                        set_label: &model.uptime,
                    },
                },
            },
        }
    }

    fn init(config: Rc<Config>, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let options = [
            ("lock", "Lock", &config.lock),
            ("log-out", "Log out", &config.logout),
            ("moon", "Suspend", &config.suspend),
            ("rotate-cw", "Reboot", &config.reboot),
            ("power", "Shut down", &config.poweroff),
        ];
        let mut tiles = FactoryVecDeque::builder().launch(gtk::Box::default()).forward(sender.input_sender(), |msg| msg);
        for (i, (icon, label, _)) in options.iter().enumerate() {
            tiles.guard().push_back((i, *icon, *label));
        }
        let commands = options.iter().map(|(_, _, command)| (*command).clone()).collect();
        let mut model = PowerMenu {
            commands,
            shown: false,
            tiles,
            current: None,
            uptime: String::new(),
            fade: None,
            instant: false,
        };
        let tiles = model.tiles.widget();
        let widgets = view_output!();

        let mut inside = Vec::new();
        let mut tile = tiles.first_child();
        while let Some(widget) = tile {
            tile = widget.next_sibling();
            inside.push(widget);
        }
        let close = sender.clone();
        widgets.backdrop.on_click_outside(&inside, move || close.input(PowerMenuMsg::Close));
        model.fade = Some(Fade::new(&root, &widgets.backdrop, false, 80));
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: PowerMenuMsg, sender: ComponentSender<Self>) {
        let n = self.commands.len();
        let was_shown = self.shown;
        // Keys still reach it while it fades out, and Enter must not run an action then
        if !self.shown
            && matches!(
                msg,
                PowerMenuMsg::Hover(_)
                    | PowerMenuMsg::Next
                    | PowerMenuMsg::Previous
                    | PowerMenuMsg::Activate
                    | PowerMenuMsg::Run(_)
            )
        {
            return;
        }
        match msg {
            PowerMenuMsg::Open => self.open(),
            PowerMenuMsg::Toggle if self.shown => self.shown = false,
            PowerMenuMsg::Toggle => self.open(),
            PowerMenuMsg::Close => self.shown = false,
            PowerMenuMsg::Hover(i) => self.set_current(Some(i)),
            PowerMenuMsg::Leave(i) if self.current == Some(i) => self.set_current(None),
            PowerMenuMsg::Leave(_) => {}
            PowerMenuMsg::Next => self.set_current(Some(self.current.map_or(0, |c| (c + 1) % n))),
            PowerMenuMsg::Previous => self.set_current(Some(self.current.map_or(n - 1, |c| (c + n - 1) % n))),
            PowerMenuMsg::Activate => {
                if let Some(current) = self.current {
                    self.run(current);
                }
            }
            PowerMenuMsg::Run(i) => self.run(i),
        }
        if self.shown != was_shown {
            if let Some(fade) = &self.fade {
                match (self.shown, std::mem::take(&mut self.instant)) {
                    (true, _) => fade.show(),
                    (false, true) => fade.hide_now(),
                    (false, false) => fade.hide(),
                }
            }
            let _ = sender.output(Shown(self.shown));
        }
    }
}

impl PowerMenu {
    fn open(&mut self) {
        if self.shown {
            return;
        }
        let seconds = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .unwrap_or(0.0) as u64;
        self.uptime = format!("Up {}h {}m. Esc to cancel.", seconds / 3600, seconds % 3600 / 60);
        self.set_current(None);
        self.shown = true;
    }

    fn run(&mut self, index: usize) {
        self.shown = false;
        self.instant = true;
        util::spawn(&self.commands[index]);
    }

    fn set_current(&mut self, index: Option<usize>) {
        for (i, tile) in self.tiles.guard().iter_mut().enumerate() {
            tile.current = Some(i) == index;
        }
        self.current = index;
    }
}

struct Tile {
    index: usize,
    icon: &'static str,
    label: &'static str,
    current: bool,
}

#[relm4::factory]
impl FactoryComponent for Tile {
    type Init = (usize, &'static str, &'static str);
    type Input = ();
    type Output = PowerMenuMsg;
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_spacing: 10,
            set_css_classes: &["card", "power-tile"],
            #[watch]
            set_class_active: ("current", self.current),
            set_cursor_from_name: Some("pointer"),
            on_pointer_moved[sender, index = self.index] => move |_, _| {
                let _ = sender.output(PowerMenuMsg::Hover(index));
            },
            on_hover[sender, index = self.index] => move |inside| {
                if !inside {
                    let _ = sender.output(PowerMenuMsg::Leave(index));
                }
            },
            on_left_click[sender, index = self.index] => move || {
                let _ = sender.output(PowerMenuMsg::Run(index));
            },

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 10,
                set_valign: gtk::Align::Center,
                set_vexpand: true,

                Icon::new(self.icon, 44) {
                    set_halign: gtk::Align::Center,
                },
                gtk::Label {
                    set_label: self.label,
                    set_css_classes: &["small", "bold"],
                },
            },
        }
    }

    fn init_model((index, icon, label): Self::Init, _: &DynamicIndex, _: FactorySender<Self>) -> Self {
        Tile { index, icon, label, current: false }
    }
}

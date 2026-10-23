use std::{cell::Cell, rc::Rc};

use gtk::{gdk, gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

pub struct LayerOptions<'a> {
    pub namespace: &'a str,
    pub layer: Layer,
    /// Top, bottom, left, right
    pub anchors: [bool; 4],
    pub keyboard: KeyboardMode,
    pub monitor: Option<&'a gdk::Monitor>,
    pub exclusive_zone: i32,
}

pub fn layer_window(options: LayerOptions) -> gtk::Window {
    let window = gtk::Window::new();
    window.add_css_class("wakeshell");
    window.set_decorated(false);
    window.init_layer_shell();
    window.set_namespace(Some(options.namespace));
    window.set_layer(options.layer);
    for (edge, anchored) in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right]
        .into_iter()
        .zip(options.anchors)
    {
        window.set_anchor(edge, anchored);
    }
    window.set_keyboard_mode(options.keyboard);
    window.set_exclusive_zone(options.exclusive_zone);
    if let Some(monitor) = options.monitor {
        window.set_monitor(Some(monitor));
    }
    window
}

pub fn overlay_window(namespace: &str) -> gtk::Window {
    layer_window(LayerOptions {
        namespace,
        layer: Layer::Overlay,
        anchors: [true; 4],
        keyboard: KeyboardMode::Exclusive,
        monitor: None,
        exclusive_zone: -1,
    })
}

/// Event controllers as methods, so `view!` can attach them like signal handlers
pub trait Events: IsA<gtk::Widget> {
    fn on_click(&self, f: impl Fn(u32) + 'static) {
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.connect_released(move |gesture, _, x, y| {
            let widget = gesture.widget().unwrap();
            if widget.contains(x, y) {
                f(gesture.current_button());
            }
        });
        self.add_controller(click);
    }

    fn on_left_click(&self, f: impl Fn() + 'static) {
        self.on_click(move |button| {
            if button == 1 {
                f()
            }
        });
    }

    /// A click in the widget outside `inside`
    fn on_click_outside(&self, inside: &[gtk::Widget], f: impl Fn() + 'static) {
        let inside = inside.to_vec();
        let click = gtk::GestureClick::new();
        click.connect_released(move |gesture, _, x, y| {
            let root = gesture.widget().unwrap();
            let within = root.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|picked| {
                inside.iter().any(|w| &picked == w || picked.is_ancestor(w))
            });
            if !within {
                f();
            }
        });
        self.add_controller(click);
    }

    /// Skips the first event after entering, so a resting pointer does not pick anything
    fn on_pointer_moved(&self, f: impl Fn(f64, f64) + 'static) {
        let motion = gtk::EventControllerMotion::new();
        let last: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));
        let enter_last = last.clone();
        motion.connect_enter(move |_, x, y| {
            if enter_last.get().is_none() {
                enter_last.set(Some((x, y)));
            }
        });
        motion.connect_motion(move |_, x, y| {
            let previous = last.replace(Some((x, y)));
            if previous.is_some_and(|p| p != (x, y)) {
                f(x, y);
            }
        });
        self.add_controller(motion);
    }

    fn on_hover(&self, f: impl Fn(bool) + 'static) {
        let motion = gtk::EventControllerMotion::new();
        let f = Rc::new(f);
        let enter = f.clone();
        motion.connect_enter(move |_, _, _| enter(true));
        motion.connect_leave(move |_| f(false));
        self.add_controller(motion);
    }

    /// Capture phase, so it sees Tab and Return before text fields do
    fn on_key(&self, f: impl Fn(gdk::Key, gdk::ModifierType) -> bool + 'static) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if f(key, modifiers) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.add_controller(keys);
    }
}

impl<W: IsA<gtk::Widget>> Events for W {}

pub fn spawn(argv: &[String]) {
    if argv.is_empty() {
        return;
    }
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_ref()).collect();
    match gio::Subprocess::newv(&args, gio::SubprocessFlags::NONE) {
        Ok(process) => {
            glib::spawn_future_local(async move {
                let _ = process.wait_future().await;
            });
        }
        Err(e) => eprintln!("wakeshell: cannot run {}: {e}", argv[0]),
    }
}

pub async fn run(argv: &[&str]) -> bool {
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_ref()).collect();
    let flags = gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE;
    match gio::Subprocess::newv(&args, flags) {
        Ok(process) => process.wait_check_future().await.is_ok(),
        Err(_) => false,
    }
}

pub async fn output(argv: &[&str]) -> Option<String> {
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_ref()).collect();
    let process = gio::Subprocess::newv(
        &args,
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    )
    .ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    process.wait_check_future().await.ok()?;
    stdout.map(|s| s.to_string())
}

pub async fn output_bytes(argv: &[&str]) -> Option<Vec<u8>> {
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_ref()).collect();
    let process = gio::Subprocess::newv(
        &args,
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    )
    .ok()?;
    let (stdout, _) = process.communicate_future(None).await.ok()?;
    process.wait_check_future().await.ok()?;
    stdout.map(|b| b.to_vec())
}

pub fn window_monitor(window: &gtk::Window) -> Option<gdk::Monitor> {
    let surface = window.surface()?;
    WidgetExt::display(window).monitor_at_surface(&surface)
}

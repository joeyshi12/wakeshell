// wakeshell: the desktop shell for dwl. The lock screen is the separate wakelock.
mod anim;
mod backdrop;
mod bar;
mod config;
mod dwl;
mod icon;
mod ipc;
mod launcher;
mod mixer;
mod monitors;
mod notifications;
mod osd;
mod pipewire;
mod polkit;
mod powermenu;
mod ppm;
mod sharepicker;
mod style;
mod util;
mod wallpaper;

use std::{collections::HashSet, io::Read, process::ExitCode, rc::Rc};

use gtk::glib;
use relm4::prelude::*;

use config::Config;
use launcher::LauncherMsg;
use powermenu::PowerMenuMsg;

const USAGE: &str = "usage: wakeshell [msg COMMAND... | volume up|down|mute | share-chooser]

  volume up|down|mute            change the default sink's volume by 5% and show the popup

Commands for msg:
  launcher toggle|open|close     the app launcher
  powermenu toggle|open|close    the power menu
  mixer toggle|open|close        the volume mixer, under the bar's volume
  osd volume                     show the default sink's volume for a moment
  wallpaper reload               fade to the image the wallpaper link points at now";

pub struct Shell {
    launcher: Controller<launcher::Launcher>,
    powermenu: Controller<powermenu::PowerMenu>,
    osd: Controller<osd::Osd>,
    wallpaper: Controller<wallpaper::Wallpaper>,
    sharepicker: Controller<sharepicker::SharePicker>,
    mixer: Controller<mixer::Mixer>,
    bar: Controller<bar::Bar>,
    _notifications: Controller<notifications::Notifications>,
    _polkit: Controller<polkit::Polkit>,
    overlays: HashSet<Overlay>,
    popups: HashSet<Popup>,
}

/// Full-screen surfaces that take over until they are done
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Overlay {
    PowerMenu,
    Polkit,
    SharePicker,
}

/// Lighter surfaces that close when anything else opens
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Popup {
    Launcher,
    Mixer,
}

#[derive(Debug)]
pub enum ShellMsg {
    Request(ipc::Request),
    Overlay(Overlay, bool),
    Popup(Popup, bool),
    Bar(bar::BarEvent),
    Audio(pipewire::Audio),
}

impl Component for Shell {
    type Init = async_channel::Receiver<ipc::Request>;
    type Input = ShellMsg;
    type Output = ();
    type CommandOutput = ();
    type Root = ();
    type Widgets = ();

    fn init_root() {}

    fn init(requests: Self::Init, _: (), sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let config = Rc::new(Config::load());
        let overlay = |which| move |shown| ShellMsg::Overlay(which, shown);
        let popup = |which| move |shown| ShellMsg::Popup(which, shown);
        let input = sender.input_sender();
        let shell = Shell {
            launcher: launcher::Launcher::builder()
                .launch(config.clone())
                .forward(input, move |launcher::Shown(shown)| popup(Popup::Launcher)(shown)),
            powermenu: powermenu::PowerMenu::builder()
                .launch(config.clone())
                .forward(input, move |powermenu::Shown(shown)| overlay(Overlay::PowerMenu)(shown)),
            osd: osd::Osd::builder().launch(()).detach(),
            wallpaper: wallpaper::Wallpaper::builder().launch(config.clone()).detach(),
            sharepicker: sharepicker::SharePicker::builder()
                .launch(())
                .forward(input, move |sharepicker::Shown(shown)| overlay(Overlay::SharePicker)(shown)),
            mixer: mixer::Mixer::builder()
                .launch(())
                .forward(input, move |mixer::Shown(shown)| popup(Popup::Mixer)(shown)),
            bar: bar::Bar::builder().launch(config).forward(input, ShellMsg::Bar),
            _notifications: notifications::Notifications::builder().launch(()).detach(),
            _polkit: polkit::Polkit::builder()
                .launch(())
                .forward(input, move |polkit::Shown(shown)| overlay(Overlay::Polkit)(shown)),
            overlays: HashSet::new(),
            popups: HashSet::new(),
        };
        let audio = sender.clone();
        pipewire::watch(move |state| audio.input(ShellMsg::Audio(state)));
        glib::spawn_future_local(async move {
            while let Ok(request) = requests.recv().await {
                sender.input(ShellMsg::Request(request));
            }
        });
        ComponentParts { model: shell, widgets: () }
    }

    fn update(&mut self, msg: ShellMsg, _: ComponentSender<Self>, _: &()) {
        match msg {
            // Overlays never stack, and close any popup
            ShellMsg::Overlay(which, true) => {
                self.overlays.insert(which);
                self.close_popups(None);
            }
            ShellMsg::Overlay(which, false) => {
                self.overlays.remove(&which);
            }
            ShellMsg::Popup(which, true) => {
                self.popups.insert(which);
                self.close_popups(Some(which));
                if which == Popup::Mixer {
                    self.osd.emit(osd::OsdMsg::HideNow);
                }
            }
            ShellMsg::Popup(which, false) => {
                self.popups.remove(&which);
            }
            ShellMsg::Bar(event) => self.bar_event(event),
            ShellMsg::Audio(audio) => {
                self.mixer.emit(mixer::MixerMsg::Audio(audio.clone()));
                self.bar.emit(bar::BarMsg::Audio(audio));
            }
            ShellMsg::Request(request) => self.handle(request),
        }
    }
}

impl Shell {
    /// Every popup but `except`
    fn close_popups(&self, except: Option<Popup>) {
        for popup in [Popup::Launcher, Popup::Mixer] {
            if Some(popup) == except {
                continue;
            }
            match popup {
                Popup::Launcher => self.launcher.emit(LauncherMsg::Close),
                Popup::Mixer => self.mixer.emit(mixer::MixerMsg::Close),
            }
        }
    }

    fn bar_event(&self, event: bar::BarEvent) {
        let blocked = !self.overlays.is_empty();
        match event {
            // The bar on another monitor stays clickable under an overlay
            bar::BarEvent::Popup { .. } if blocked => {}
            bar::BarEvent::Popup { which: bar::Popup::Mixer, output, right } => {
                self.mixer.emit(mixer::MixerMsg::Toggle { output: Some(output), right: Some(right) })
            }
            bar::BarEvent::Volume(change) => {
                let Some(steps) = pipewire::volume_steps(change) else { return };
                let (osd, mixer_shown) = (self.osd.sender().clone(), self.popups.contains(&Popup::Mixer));
                glib::spawn_future_local(async move {
                    for step in steps {
                        let mut argv = vec!["wpctl"];
                        argv.extend_from_slice(step);
                        if !util::run(&argv).await {
                            return;
                        }
                    }
                    if !mixer_shown {
                        osd.emit(osd::OsdMsg::Volume);
                    }
                });
            }
        }
    }

    fn handle(&self, request: ipc::Request) {
        let args: Vec<&str> = request.args.iter().map(String::as_str).collect();
        let blocked = !self.overlays.is_empty();
        let mixer_shown = self.popups.contains(&Popup::Mixer);
        match args.as_slice() {
            ["launcher", "toggle"] => self.launcher.emit(LauncherMsg::Toggle { blocked }),
            ["launcher", "open"] => self.launcher.emit(LauncherMsg::Open { blocked }),
            ["launcher", "close"] => self.launcher.emit(LauncherMsg::Close),
            ["powermenu", "toggle"] => self.powermenu.emit(PowerMenuMsg::Toggle),
            ["powermenu", "open"] => self.powermenu.emit(PowerMenuMsg::Open),
            ["powermenu", "close"] => self.powermenu.emit(PowerMenuMsg::Close),
            ["mixer", "toggle"] if mixer_shown => self.mixer.emit(mixer::MixerMsg::Close),
            ["mixer", "toggle" | "open"] if !blocked && !mixer_shown => self.bar.emit(bar::BarMsg::Open(bar::Popup::Mixer)),
            ["mixer", "toggle" | "open"] => {}
            ["mixer", "close"] => self.mixer.emit(mixer::MixerMsg::Close),
            // The mixer shows the volume itself
            ["osd", "volume"] if mixer_shown => {}
            ["osd", "volume"] => self.osd.emit(osd::OsdMsg::Volume),
            ["wallpaper", "reload"] => self.wallpaper.emit(wallpaper::WallpaperMsg::Reload),
            ["sharepicker", sources] => {
                let sources = sources.to_string();
                return self.sharepicker.emit(sharepicker::SharePickerMsg::Pick(sources, request.reply));
            }
            _ => {
                let error = format!("error: unknown command {:?}. {USAGE}", request.args.join(" "));
                return request.reply.send(error);
            }
        }
        request.reply.send("ok");
    }
}

fn run() -> ExitCode {
    if let Err(e) = gtk::init() {
        eprintln!("wakeshell: {e}");
        return ExitCode::FAILURE;
    }
    if !gtk4_layer_shell::is_supported() {
        eprintln!("wakeshell: the compositor does not support the layer shell protocol");
        return ExitCode::FAILURE;
    }
    let requests = match ipc::listen() {
        Ok(requests) => requests,
        Err(e) => {
            eprintln!("wakeshell: {e}");
            return ExitCode::FAILURE;
        }
    };
    style::load();
    let _shell = Shell::builder().launch(requests).detach();

    let main_loop = glib::MainLoop::new(None, false);
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let main_loop = main_loop.clone();
        glib_unix::unix_signal_add_local(signal, move || {
            main_loop.quit();
            glib::ControlFlow::Break
        });
    }
    main_loop.run();
    let _ = std::fs::remove_file(ipc::socket_path());
    ExitCode::SUCCESS
}

fn msg(args: &[String]) -> ExitCode {
    if args.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    }
    match ipc::call(args) {
        Ok(reply) if reply.starts_with("error: ") => {
            eprintln!("wakeshell: {}", &reply["error: ".len()..]);
            ExitCode::FAILURE
        }
        Ok(reply) => {
            if reply != "ok" {
                println!("{reply}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("wakeshell: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Runs in this process rather than the shell, so the volume keys work while the shell is down
fn volume(change: Option<&str>) -> ExitCode {
    let Some(steps) = change.and_then(pipewire::volume_steps) else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    for args in steps {
        match std::process::Command::new("wpctl").args(*args).status() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!("wakeshell: wpctl {}: {status}", args.join(" "));
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("wakeshell: cannot run wpctl: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let _ = ipc::call(&["osd".into(), "volume".into()]);
    ExitCode::SUCCESS
}

/// xdg-desktop-portal-wlr chooser_cmd (dmenu type): sources on stdin, the choice on stdout
fn share_chooser() -> ExitCode {
    let mut sources = String::new();
    if std::io::stdin().read_to_string(&mut sources).is_err() {
        return ExitCode::FAILURE;
    }
    match ipc::call(&["sharepicker".to_owned(), sources]) {
        Ok(choice) if choice.starts_with("error: ") => {
            eprintln!("wakeshell: {}", &choice["error: ".len()..]);
            ExitCode::FAILURE
        }
        Ok(choice) => {
            if !choice.is_empty() {
                println!("{choice}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("wakeshell: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => run(),
        Some("msg") => msg(&args[1..]),
        Some("volume") => volume(args.get(1).map(String::as_str)),
        Some("share-chooser") => share_chooser(),
        Some("-V" | "--version") => {
            println!("wakeshell {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(_) => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

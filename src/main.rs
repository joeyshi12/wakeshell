// wakeshell: the desktop shell for dwl. The lock screen is the separate wakelock.
mod backdrop;
mod bar;
mod config;
mod dwl;
mod icon;
mod ipc;
mod launcher;
mod monitors;
mod notifications;
mod osd;
mod polkit;
mod powermenu;
mod ppm;
mod sharepicker;
mod style;
mod util;
mod wallpaper;

use std::{io::Read, process::ExitCode, rc::Rc};

use gtk::glib;

use config::Config;

const USAGE: &str = "usage: wakeshell [msg COMMAND... | volume up|down|mute | share-chooser]

  volume up|down|mute            change the default sink's volume by 5% and show the popup

Commands for msg:
  launcher toggle|open|close     the app launcher
  powermenu toggle|open|close    the power menu
  osd volume                     show the default sink's volume for a moment
  wallpaper reload               fade to the image the wallpaper link points at now";

pub struct Shell {
    launcher: Rc<launcher::Launcher>,
    powermenu: Rc<powermenu::PowerMenu>,
    osd: Rc<osd::Osd>,
    wallpaper: Rc<wallpaper::Wallpaper>,
    sharepicker: Rc<sharepicker::SharePicker>,
    _bar: Rc<bar::Bar>,
    _notifications: Rc<notifications::Notifications>,
    _polkit: Rc<polkit::Polkit>,
}

impl Shell {
    fn new() -> Rc<Shell> {
        let config = Rc::new(Config::load());
        Rc::new_cyclic(|shell: &std::rc::Weak<Shell>| {
            // Overlays never stack
            let close_launcher: Rc<dyn Fn()> = {
                let shell = shell.clone();
                Rc::new(move || {
                    if let Some(shell) = shell.upgrade() {
                        shell.launcher.close();
                    }
                })
            };
            let blocked: Rc<dyn Fn() -> bool> = {
                let shell = shell.clone();
                Rc::new(move || {
                    shell.upgrade().is_some_and(|shell| {
                        shell.powermenu.shown() || shell._polkit.active() || shell.sharepicker.shown()
                    })
                })
            };
            Shell {
                launcher: launcher::Launcher::new(config.clone(), blocked),
                powermenu: powermenu::PowerMenu::new(config.clone(), close_launcher.clone()),
                osd: osd::Osd::new(),
                wallpaper: wallpaper::Wallpaper::new(config.clone()),
                sharepicker: sharepicker::SharePicker::new(close_launcher.clone()),
                _bar: bar::Bar::new(config.clone()),
                _notifications: notifications::Notifications::new(),
                _polkit: polkit::Polkit::new(close_launcher),
            }
        })
    }

    fn handle(&self, request: ipc::Request) {
        let args: Vec<&str> = request.args.iter().map(String::as_str).collect();
        let ok = || "ok".to_owned();
        let reply = match args.as_slice() {
            ["launcher", "toggle"] => {
                self.launcher.toggle();
                ok()
            }
            ["launcher", "open"] => {
                self.launcher.open();
                ok()
            }
            ["launcher", "close"] => {
                self.launcher.close();
                ok()
            }
            ["powermenu", "toggle"] => {
                self.powermenu.toggle();
                ok()
            }
            ["powermenu", "open"] => {
                self.powermenu.open();
                ok()
            }
            ["powermenu", "close"] => {
                self.powermenu.close();
                ok()
            }
            ["osd", "volume"] => {
                self.osd.volume();
                ok()
            }
            ["wallpaper", "reload"] => {
                self.wallpaper.reload();
                ok()
            }
            ["sharepicker", sources] => {
                self.sharepicker.pick(sources, request.reply);
                return;
            }
            _ => format!("error: unknown command {:?}. {USAGE}", request.args.join(" ")),
        };
        request.reply.send(reply);
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
    let shell = Shell::new();
    glib::spawn_future_local(async move {
        while let Ok(request) = requests.recv().await {
            shell.handle(request);
        }
    });

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
    const SINK: &str = "@DEFAULT_AUDIO_SINK@";
    let steps: &[&[&str]] = match change {
        Some("up") => &[&["set-mute", SINK, "0"], &["set-volume", SINK, "5%+", "--limit", "1"]],
        Some("down") => &[&["set-mute", SINK, "0"], &["set-volume", SINK, "5%-"]],
        Some("mute") => &[&["set-mute", SINK, "toggle"]],
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
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

// $XDG_CONFIG_HOME/wakeshell/wakeshell.conf: `key = value` lines, # comments, commands split on
// whitespace (no quoting).
use std::{collections::HashMap, fs, path::PathBuf};

pub struct Config {
    pub wallpaper: PathBuf,
    pub wallpaper_fallback: Option<PathBuf>,
    /// One icon per dwl tag
    pub tags: Vec<String>,
    /// Index in dwl's layouts[]
    pub monocle_layout: u32,
    pub terminal: Vec<String>,
    pub launch_prefix: Vec<String>,
    pub lock: Vec<String>,
    pub logout: Vec<String>,
    pub suspend: Vec<String>,
    pub reboot: Vec<String>,
    pub poweroff: Vec<String>,
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
        .join("wakeshell")
}

pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local/state"))
        .join("wakeshell")
}

pub fn runtime_dir() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("wakeshell");
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir);
    dir
}

use std::os::unix::fs::DirBuilderExt;

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

impl Config {
    pub fn load() -> Config {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home().join(".local/share"));
        let mut config = Config {
            wallpaper: data_home.join("wallpaper"),
            wallpaper_fallback: None,
            tags: words("globe terminal palette message-circle key-round"),
            monocle_layout: 2,
            terminal: words("uwsm-app -- ghostty"),
            launch_prefix: words("uwsm-app --"),
            lock: words("loginctl lock-session"),
            logout: words("uwsm stop"),
            suspend: words("systemctl suspend"),
            reboot: words("systemctl reboot"),
            poweroff: words("systemctl poweroff"),
        };

        let path = config_dir().join("wakeshell.conf");
        let Ok(text) = fs::read_to_string(&path) else {
            return config;
        };
        let mut values = HashMap::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match line.split_once('=') {
                Some((key, value)) => {
                    values.insert(key.trim().to_owned(), value.trim().to_owned());
                }
                None => eprintln!("wakeshell: {}:{}: expected key = value", path.display(), n + 1),
            }
        }
        for (key, value) in values {
            match key.as_str() {
                "wallpaper" => config.wallpaper = expand(&value),
                "wallpaper_fallback" => {
                    config.wallpaper_fallback = (!value.is_empty()).then(|| expand(&value))
                }
                "tags" => config.tags = words(&value),
                "monocle_layout" => match value.parse() {
                    Ok(n) => config.monocle_layout = n,
                    Err(_) => eprintln!("wakeshell: monocle_layout must be a number"),
                },
                "terminal" => config.terminal = words(&value),
                "launch_prefix" => config.launch_prefix = words(&value),
                "lock" => config.lock = words(&value),
                "logout" => config.logout = words(&value),
                "suspend" => config.suspend = words(&value),
                "reboot" => config.reboot = words(&value),
                "poweroff" => config.poweroff = words(&value),
                _ => eprintln!("wakeshell: {}: unknown key {key}", path.display()),
            }
        }
        config
    }
}

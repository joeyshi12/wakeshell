// PipeWire state from a long-running `pw-dump --monitor`: audio devices and streams for the bar
// and the mixer. Changes go through wpctl.
use std::{
    collections::HashMap,
    io::BufReader,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::Duration,
};

use gtk::glib;
use serde_json::Value;

use crate::util;

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: u32,
    pub description: String,
    pub icon: &'static str,
    pub volume: f64,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub id: u32,
    pub app: String,
    pub icon: String,
    pub volume: f64,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Audio {
    pub sinks: Vec<Device>,
    pub sources: Vec<Device>,
    pub streams: Vec<Stream>,
    pub default_sink: Option<u32>,
    pub default_source: Option<u32>,
}

impl Audio {
    pub fn sink(&self) -> Option<&Device> {
        self.sinks.iter().find(|d| Some(d.id) == self.default_sink)
    }

    pub fn source(&self) -> Option<&Device> {
        self.sources.iter().find(|d| Some(d.id) == self.default_source)
    }
}

/// Calls `f` on the main loop whenever the state changes, restarting pw-dump if it exits
pub fn watch(f: impl Fn(Audio) + 'static) {
    let (sender, receiver) = async_channel::unbounded();
    std::thread::spawn(move || {
        let mut reported = false;
        loop {
            let error = match run(&sender) {
                End::Closed => return,
                End::Failed(e) => e,
            };
            if !reported {
                eprintln!("wakeshell: pw-dump: {error}; trying again every 2 seconds");
                reported = true;
            }
            if sender.send_blocking(Audio::default()).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
    glib::spawn_future_local(async move {
        let mut last = None;
        while let Ok(mut audio) = receiver.recv().await {
            // Only the latest of a burst matters
            while let Ok(newer) = receiver.try_recv() {
                audio = newer;
            }
            if last.as_ref() != Some(&audio) {
                f(audio.clone());
                last = Some(audio);
            }
        }
    });
}

enum End {
    /// The shell stopped listening
    Closed,
    Failed(String),
}

fn run(sender: &async_channel::Sender<Audio>) -> End {
    let mut command = Command::new("pw-dump");
    command.args(["--monitor", "--no-colors"]).stdout(Stdio::piped()).stderr(Stdio::null());
    // Exits with the shell. The flag is per thread, and this thread lives as long as pw-dump.
    unsafe {
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return End::Failed(e.to_string()),
    };
    let Some(stdout) = child.stdout.take() else { return End::Failed("no stdout".into()) };
    let mut objects = HashMap::new();
    let batches = serde_json::Deserializer::from_reader(BufReader::new(stdout)).into_iter::<Vec<Value>>();
    let mut end = End::Failed("pw-dump exited".into());
    for batch in batches {
        let batch = match batch {
            Ok(batch) => batch,
            Err(e) => {
                end = End::Failed(e.to_string());
                break;
            }
        };
        merge(&mut objects, batch);
        if sender.send_blocking(state(&objects)).is_err() {
            end = End::Closed;
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    end
}

/// pw-dump prints whole objects when they change and `"info": null` when they go, but only the
/// changed entries of metadata, with a null value for a removed one
fn merge(objects: &mut HashMap<u64, Value>, batch: Vec<Value>) {
    for object in batch {
        let Some(id) = object["id"].as_u64() else { continue };
        let removed = object.get("info").is_some_and(Value::is_null) && object.get("metadata").is_none_or(Value::is_null);
        if removed {
            objects.remove(&id);
            continue;
        }
        let entries = object["metadata"].as_array().cloned();
        let Some(entries) = entries.filter(|_| objects.contains_key(&id)) else {
            objects.insert(id, object);
            continue;
        };
        let old = objects.insert(id, object).unwrap_or_default();
        let key = |e: &Value| (e["subject"].as_u64(), e["key"].as_str().map(str::to_owned));
        let mut merged: Vec<Value> = old["metadata"].as_array().cloned().unwrap_or_default();
        for entry in entries {
            merged.retain(|e| key(e) != key(&entry));
            if !entry["value"].is_null() {
                merged.push(entry);
            }
        }
        objects.get_mut(&id).unwrap()["metadata"] = Value::Array(merged);
    }
}

fn state(objects: &HashMap<u64, Value>) -> Audio {
    let mut audio = Audio::default();
    let mut ids: Vec<&u64> = objects.keys().collect();
    ids.sort();
    let mut names = HashMap::new();
    for &id in &ids {
        let object = &objects[id];
        let id = *id as u32;
        let props = &object["info"]["props"];
        let prop = |key: &str| props[key].as_str().unwrap_or("").to_owned();
        if !object["type"].as_str().is_some_and(|t| t.ends_with(":Node")) {
            continue;
        }
        names.insert(prop("node.name"), id);
        let (volume, muted) = volume(object);
        match props["media.class"].as_str().unwrap_or("") {
            class @ ("Audio/Sink" | "Audio/Source") => {
                let description = [prop("node.description"), prop("node.nick"), prop("node.name")]
                    .into_iter()
                    .find(|s| !s.is_empty())
                    .unwrap_or_default();
                let device = Device { id, icon: device_icon(class, &prop("node.name"), &description), description, volume, muted };
                if class == "Audio/Sink" { audio.sinks.push(device) } else { audio.sources.push(device) }
            }
            "Stream/Output/Audio" => {
                let app = [prop("application.name"), prop("node.name")].into_iter().find(|s| !s.is_empty()).unwrap_or_default();
                let icon = [prop("application.icon-name"), prop("application.process.binary")]
                    .into_iter()
                    .find(|s| !s.is_empty())
                    .unwrap_or_default();
                audio.streams.push(Stream { id, app, icon, volume, muted });
            }
            _ => {}
        }
    }
    for object in objects.values() {
        if object["props"]["metadata.name"].as_str() != Some("default") {
            continue;
        }
        for entry in object["metadata"].as_array().into_iter().flatten() {
            let name = entry["value"]["name"].as_str().and_then(|n| names.get(n)).copied();
            match entry["key"].as_str() {
                Some("default.audio.sink") => audio.default_sink = name,
                Some("default.audio.source") => audio.default_source = name,
                _ => {}
            }
        }
    }
    audio
}

/// On wpctl's scale: the cube root of the channels' linear volume
fn volume(node: &Value) -> (f64, bool) {
    let props = node["info"]["params"]["Props"].as_array().and_then(|p| p.iter().find(|p| p.get("channelVolumes").is_some()));
    let Some(props) = props else { return (0.0, false) };
    let channels: Vec<f64> = props["channelVolumes"].as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
    let linear = if channels.is_empty() { 0.0 } else { channels.iter().sum::<f64>() / channels.len() as f64 };
    (linear.cbrt(), props["mute"].as_bool().unwrap_or(false))
}

fn device_icon(class: &str, name: &str, description: &str) -> &'static str {
    let text = format!("{name} {description}").to_lowercase();
    if class == "Audio/Source" {
        "mic"
    } else if ["bluez", "headphone", "headset", "usb"].iter().any(|w| text.contains(w)) {
        "headphones"
    } else if ["hdmi", "displayport", "dp-"].iter().any(|w| text.contains(w)) {
        "monitor"
    } else {
        "speaker"
    }
}

pub async fn set_volume(id: u32, volume: f64) {
    let volume = format!("{:.2}", volume.clamp(0.0, 1.0));
    util::run(&["wpctl", "set-volume", &id.to_string(), &volume]).await;
}

pub async fn set_mute(id: u32, muted: bool) {
    util::run(&["wpctl", "set-mute", &id.to_string(), if muted { "1" } else { "0" }]).await;
}

pub async fn set_default(id: u32) {
    util::run(&["wpctl", "set-default", &id.to_string()]).await;
}

/// The icon for a volume on wpctl's scale
pub fn volume_icon(volume: f64, muted: bool) -> &'static str {
    if muted || volume <= 0.0 {
        "volume-x"
    } else if volume < 0.34 {
        "volume"
    } else if volume < 0.67 {
        "volume-1"
    } else {
        "volume-2"
    }
}

/// The wpctl calls behind `wakeshell volume up|down|mute`
pub fn volume_steps(change: &str) -> Option<&'static [&'static [&'static str]]> {
    const SINK: &str = "@DEFAULT_AUDIO_SINK@";
    Some(match change {
        "up" => &[&["set-mute", SINK, "0"], &["set-volume", SINK, "5%+", "--limit", "1"]],
        "down" => &[&["set-mute", SINK, "0"], &["set-volume", SINK, "5%-"]],
        "mute" => &[&["set-mute", SINK, "toggle"]],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: u64, class: &str, props: Value, volumes: &[f64], mute: bool) -> Value {
        let mut props = props;
        props["media.class"] = json!(class);
        json!({
            "id": id, "type": "PipeWire:Interface:Node",
            "info": { "props": props, "params": { "Props": [ { "volume": 1.0 }, { "channelVolumes": volumes, "mute": mute } ] } }
        })
    }

    fn dump() -> Vec<Value> {
        vec![
            node(57, "Audio/Sink", json!({ "node.name": "alsa_output.analog", "node.description": "Built-in Audio" }), &[0.042875, 0.042875], false),
            node(54, "Audio/Sink", json!({ "node.name": "alsa_output.hdmi-stereo", "node.description": "HDMI Audio" }), &[1.0], true),
            node(56, "Audio/Source", json!({ "node.name": "alsa_input.usb", "node.description": "Microphone" }), &[0.729], false),
            node(97, "Stream/Output/Audio", json!({ "node.name": "music-player", "application.name": "Music Player", "application.icon-name": "music-player" }), &[1.0, 1.0], false),
            node(78, "Stream/Input/Audio", json!({ "node.name": "voice-chat", "application.name": "Voice Chat" }), &[1.0], false),
            json!({ "id": 40, "type": "PipeWire:Interface:Metadata", "props": { "metadata.name": "default" }, "metadata": [
                { "subject": 0, "key": "default.audio.sink", "type": "Spa:String:JSON", "value": { "name": "alsa_output.analog" } },
                { "subject": 0, "key": "default.audio.source", "type": "Spa:String:JSON", "value": { "name": "alsa_input.usb" } }
            ] }),
        ]
    }

    #[test]
    fn reads_devices_streams_and_defaults() {
        let mut objects = HashMap::new();
        merge(&mut objects, dump());
        let audio = state(&objects);
        let sink = audio.sink().unwrap();
        assert_eq!((sink.id, sink.description.as_str(), sink.icon, sink.muted), (57, "Built-in Audio", "speaker", false));
        assert!((sink.volume - 0.35).abs() < 1e-6, "{}", sink.volume);
        assert_eq!(audio.sinks.iter().find(|d| d.id == 54).map(|d| (d.icon, d.muted)), Some(("monitor", true)));
        let source = audio.source().unwrap();
        assert_eq!((source.id, source.icon), (56, "mic"));
        assert!((source.volume - 0.9).abs() < 1e-6);
        assert_eq!(audio.streams.len(), 1);
        assert_eq!((audio.streams[0].app.as_str(), audio.streams[0].icon.as_str()), ("Music Player", "music-player"));
    }

    #[test]
    fn follows_changes_and_removals() {
        let mut objects = HashMap::new();
        merge(&mut objects, dump());
        merge(&mut objects, vec![
            json!({ "id": 97, "info": null }),
            node(57, "Audio/Sink", json!({ "node.name": "alsa_output.analog", "node.description": "Built-in Audio" }), &[0.125], true),
        ]);
        let audio = state(&objects);
        assert!(audio.streams.is_empty());
        let sink = audio.sink().unwrap();
        assert!((sink.volume - 0.5).abs() < 1e-6 && sink.muted);
    }

    #[test]
    fn keeps_metadata_entries_that_did_not_change() {
        let mut objects = HashMap::new();
        merge(&mut objects, dump());
        let update = |key: &str, value: Value| {
            json!({ "id": 40, "type": "PipeWire:Interface:Metadata", "props": { "metadata.name": "default" },
                "metadata": [ { "subject": 0, "key": key, "type": "Spa:String:JSON", "value": value } ] })
        };
        merge(&mut objects, vec![update("default.audio.sink", json!({ "name": "alsa_output.hdmi-stereo" }))]);
        let audio = state(&objects);
        assert_eq!((audio.default_sink, audio.default_source), (Some(54), Some(56)));
        merge(&mut objects, vec![update("default.audio.source", Value::Null)]);
        let audio = state(&objects);
        assert_eq!((audio.default_sink, audio.default_source), (Some(54), None));
    }
}

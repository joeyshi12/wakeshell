// Unix socket at $XDG_RUNTIME_DIR/wakeshell-$WAYLAND_DISPLAY.sock: a JSON array of words per
// request, one reply line back ("error: ..." on failure).
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
};

pub struct Reply(async_channel::Sender<String>);

impl Reply {
    pub fn send(self, text: impl Into<String>) {
        let _ = self.0.try_send(text.into());
    }
}

pub struct Request {
    pub args: Vec<String>,
    pub reply: Reply,
}

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    let display = display.rsplit('/').next().unwrap_or("wayland-0").to_owned();
    runtime.join(format!("wakeshell-{display}.sock"))
}

pub fn listen() -> Result<async_channel::Receiver<Request>, String> {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        return Err(format!("wakeshell is already running ({})", path.display()));
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|e| format!("cannot listen on {}: {e}", path.display()))?;
    let (sender, receiver) = async_channel::unbounded();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let sender = sender.clone();
            std::thread::spawn(move || serve(stream, sender));
        }
    });
    Ok(receiver)
}

fn serve(stream: UnixStream, requests: async_channel::Sender<Request>) {
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() {
        return;
    }
    let reply = match serde_json::from_str::<Vec<String>>(&line) {
        Ok(args) => {
            let (sender, receiver) = async_channel::bounded(1);
            if requests.send_blocking(Request { args, reply: Reply(sender) }).is_err() {
                return;
            }
            receiver.recv_blocking().unwrap_or_default()
        }
        Err(_) => "error: expected a JSON array of strings".to_owned(),
    };
    let mut stream = stream;
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.write_all(b"\n");
}

pub fn call(args: &[String]) -> Result<String, String> {
    let path = socket_path();
    let mut stream =
        UnixStream::connect(&path).map_err(|e| format!("wakeshell is not running ({}: {e})", path.display()))?;
    let mut message = serde_json::to_string(args).unwrap();
    message.push('\n');
    stream.write_all(message.as_bytes()).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).map_err(|e| e.to_string())?;
    match reply.strip_suffix('\n') {
        Some(reply) => Ok(reply.to_owned()),
        None => Err("wakeshell exited before replying".into()),
    }
}

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum MpvCommand {
    LoadUrl { url: String },
    TogglePause,
    SetPause(bool),
    Stop,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum MpvEvent {
    Ready,
    MediaTitle(Option<String>),
    Pause(bool),
    Crashed(String),
}

#[derive(Debug)]
pub struct MpvProcess {
    cmd_tx: mpsc::UnboundedSender<MpvCommand>,
}

impl MpvProcess {
    pub async fn spawn(socket_path: PathBuf) -> Result<(Self, mpsc::UnboundedReceiver<MpvEvent>)> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (evt_tx, evt_rx) = mpsc::unbounded_channel();

        tokio::spawn(run_mpv(socket_path.clone(), cmd_rx, evt_tx));

        Ok((Self { cmd_tx }, evt_rx))
    }

    pub fn command(&self, cmd: MpvCommand) -> Result<()> {
        self.cmd_tx.send(cmd).map_err(|_| anyhow!("mpv task is not running"))
    }
}

async fn run_mpv(
    socket_path: PathBuf,
    mut cmd_rx: mpsc::UnboundedReceiver<MpvCommand>,
    evt_tx: mpsc::UnboundedSender<MpvEvent>,
) {
    let mut backoff = Duration::from_millis(200);
    loop {
        if cmd_rx.is_closed() {
            return;
        }
        match spawn_and_connect(&socket_path).await {
            Ok((mut child, mut stream)) => {
                backoff = Duration::from_millis(200);
                let _ = send_observers(&mut stream).await;
                let _ = evt_tx.send(MpvEvent::Ready);
                match io_loop(&mut child, stream, &mut cmd_rx, &evt_tx).await {
                    Ok(()) => return,
                    Err(e) => {
                        let _ = evt_tx.send(MpvEvent::Crashed(e.to_string()));
                    }
                }
            }
            Err(e) => {
                let _ = evt_tx.send(MpvEvent::Crashed(e.to_string()));
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, Duration::from_secs(5));
            }
        }
    }
}

async fn spawn_and_connect(socket_path: &Path) -> Result<(Child, UnixStream)> {
    let _ = tokio::fs::remove_file(socket_path).await;

    let mut child = Command::new("mpv")
        .arg("--idle=yes")
        .arg("--no-terminal")
        .arg("--no-video")
        .arg("--force-window=no")
        .arg("--keep-open=yes")
        .arg(format!(
            "--input-ipc-server={}",
            socket_path
                .to_str()
                .ok_or_else(|| anyhow!("Invalid socket path"))?
        ))
        .spawn()
        .context("Failed to spawn mpv")?;

    let start = tokio::time::Instant::now();
    let stream = loop {
        match UnixStream::connect(socket_path).await {
            Ok(s) => break s,
            Err(e) => {
                if start.elapsed() > Duration::from_secs(3) {
                    let _ = child.kill().await;
                    return Err(e).context("Timed out connecting to mpv IPC socket");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };

    Ok((child, stream))
}

async fn send_observers(stream: &mut UnixStream) -> Result<()> {
    send_json(stream, mpv_cmd(vec![
        serde_json::json!("observe_property"),
        serde_json::json!(1),
        serde_json::json!("media-title"),
    ]))
    .await?;
    send_json(stream, mpv_cmd(vec![
        serde_json::json!("observe_property"),
        serde_json::json!(2),
        serde_json::json!("pause"),
    ]))
    .await?;
    Ok(())
}

async fn io_loop(
    child: &mut Child,
    stream: UnixStream,
    cmd_rx: &mut mpsc::UnboundedReceiver<MpvCommand>,
    evt_tx: &mpsc::UnboundedSender<MpvEvent>,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half).lines();

    loop {
        tokio::select! {
            status = child.wait() => {
                let status = status.context("mpv wait failed")?;
                return Err(anyhow!("mpv exited: {status}"));
            }
            maybe_line = reader.next_line() => {
                let line = maybe_line.context("mpv IPC read error")?;
                let Some(line) = line else {
                    return Err(anyhow!("mpv IPC closed"));
                };
                if let Ok(ev) = parse_event(&line) {
                    let _ = evt_tx.send(ev);
                }
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { return Ok(()); };
                match cmd {
                    MpvCommand::LoadUrl { url } => {
                        send_json_half(&mut write_half, mpv_cmd(vec![
                            serde_json::json!("loadfile"),
                            serde_json::json!(url),
                            serde_json::json!("replace"),
                        ])).await?;
                    }
                    MpvCommand::TogglePause => {
                        send_json_half(&mut write_half, mpv_cmd(vec![
                            serde_json::json!("cycle"),
                            serde_json::json!("pause"),
                        ])).await?;
                    }
                    MpvCommand::SetPause(p) => {
                        send_json_half(&mut write_half, mpv_cmd(vec![
                            serde_json::json!("set_property"),
                            serde_json::json!("pause"),
                            serde_json::json!(p),
                        ])).await?;
                    }
                    MpvCommand::Stop => {
                        send_json_half(&mut write_half, mpv_cmd(vec![serde_json::json!("stop")])).await?;
                    }
                    MpvCommand::Shutdown => {
                        let _ = child.kill().await;
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn mpv_cmd(command: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "command": command })
}

async fn send_json(stream: &mut UnixStream, v: serde_json::Value) -> Result<()> {
    let mut buf = serde_json::to_vec(&v).context("Serialize mpv IPC request")?;
    buf.push(b'\n');
    stream.write_all(&buf).await.context("Write mpv IPC request")?;
    Ok(())
}

async fn send_json_half(
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    v: serde_json::Value,
) -> Result<()> {
    let mut buf = serde_json::to_vec(&v).context("Serialize mpv IPC request")?;
    buf.push(b'\n');
    write_half.write_all(&buf).await.context("Write mpv IPC request")?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct MpvIncoming {
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

fn parse_event(line: &str) -> Result<MpvEvent> {
    let incoming: MpvIncoming = serde_json::from_str(line).context("Invalid mpv IPC JSON")?;
    if incoming.event.as_deref() != Some("property-change") {
        return Err(anyhow!("Not a property-change event"));
    }
    match incoming.name.as_deref() {
        Some("media-title") => {
            let title = incoming
                .data
                .and_then(|v| v.as_str().map(|s| s.to_string()));
            Ok(MpvEvent::MediaTitle(title))
        }
        Some("pause") => {
            let paused = incoming
                .data
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(MpvEvent::Pause(paused))
        }
        _ => Err(anyhow!("Unrecognized property-change")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_media_title() {
        let line = r#"{"event":"property-change","name":"media-title","data":"Song Title"}"#;
        let ev = parse_event(line).unwrap();
        match ev {
            MpvEvent::MediaTitle(Some(t)) => assert_eq!(t, "Song Title"),
            _ => panic!("unexpected event"),
        }
    }

    #[test]
    fn parses_pause() {
        let line = r#"{"event":"property-change","name":"pause","data":true}"#;
        let ev = parse_event(line).unwrap();
        match ev {
            MpvEvent::Pause(true) => {}
            _ => panic!("unexpected event"),
        }
    }
}

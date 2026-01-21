use crate::config::AppConfig;
use crate::models::{Station, StationRef};
use crate::mpv::{MpvCommand, MpvEvent, MpvProcess};
use crate::radio_browser::RadioBrowserClient;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Duration;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};
use tracing::{info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackPhase {
    NotConfigured,
    Idle,
    Playing,
    Paused,
    Error,
}

#[derive(Debug, Clone)]
pub struct ControllerState {
    pub phase: PlaybackPhase,
    pub station: Option<StationRef>,
    pub media_title: Option<String>,
    pub error: Option<String>,
    pub search_query: String,
    pub search_loading: bool,
    pub search_results: Vec<Station>,
    pub favorites: Vec<StationRef>,
}

impl ControllerState {
    pub fn label_text(&self) -> String {
        if let Some(st) = &self.station {
            let name = st.name.trim();
            if !name.is_empty() {
                return name.to_string();
            }
        }

        if let Some(t) = self.media_title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            return t.to_string();
        }

        "radio".to_string()
    }
}

#[derive(Debug, Clone)]
pub enum UiCommand {
    Search(String),
    Play(StationRef),
    TogglePause,
    Stop,
    ToggleFavorite(StationRef),
    Shutdown,
}

pub struct ControllerHandle {
    pub cmd_tx: mpsc::UnboundedSender<UiCommand>,
    pub state_rx: watch::Receiver<ControllerState>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ControllerHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(UiCommand::Shutdown);
        if let Some(t) = self._thread.take() {
            let _ = t.join();
        }
    }
}

pub fn start_controller() -> ControllerHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (state_tx, state_rx) = watch::channel(ControllerState {
        phase: PlaybackPhase::NotConfigured,
        station: None,
        media_title: None,
        error: None,
        search_query: String::new(),
        search_loading: false,
        search_results: vec![],
        favorites: vec![],
    });

    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async move {
            if let Err(e) = controller_main(cmd_rx, state_tx).await {
                warn!(error = ?e, "controller exited with error");
            }
        });
    });

    ControllerHandle {
        cmd_tx,
        state_rx,
        _thread: Some(thread),
    }
}

async fn controller_main(
    mut cmd_rx: mpsc::UnboundedReceiver<UiCommand>,
    state_tx: watch::Sender<ControllerState>,
) -> Result<()> {
    let mut config = tokio::task::spawn_blocking(AppConfig::load)
        .await
        .context("Join config load task")?
        .context("Failed to load config")?;
    let mut state = state_tx.borrow().clone();
    state.favorites = config.favorites.clone();
    state.station = config.last_station.clone();
    state.phase = if state.station.is_some() {
        PlaybackPhase::Idle
    } else {
        PlaybackPhase::NotConfigured
    };
    let _ = state_tx.send(state.clone());

    let socket_path = mpv_socket_path()?;
    let (mpv, mut mpv_events) = MpvProcess::spawn(socket_path).await?;

    let rb = Arc::new(Mutex::new(RadioBrowserClient::new(config.last_server.clone())?));
    let (internal_tx, mut internal_rx) = mpsc::unbounded_channel::<InternalMsg>();
    let mut current_url: Option<String> = None;
    let mut want_paused = false;

    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    UiCommand::Search(q) => {
                        state.search_query = q;
                        state.search_loading = true;
                        state.error = None;
                        let _ = state_tx.send(state.clone());

                        let q = state.search_query.clone();
                        let rb = rb.clone();
                        let tx = internal_tx.clone();
                        tokio::spawn(async move {
                            let res = {
                                let mut client = rb.lock().await;
                                client.search(&q, 25).await
                            };
                            let _ = tx.send(InternalMsg::SearchDone { query: q, res });
                        });
                    }
                    UiCommand::Play(station) => {
                        state.error = None;
                        state.media_title = None;
                        state.station = Some(station.clone());
                        state.phase = PlaybackPhase::Idle;
                        want_paused = false;
                        let _ = state_tx.send(state.clone());
                        let _ = mpv.command(MpvCommand::SetTitle(station.name.clone()));
                        let rb = rb.clone();
                        let tx = internal_tx.clone();
                        tokio::spawn(async move {
                            let res = {
                                let mut client = rb.lock().await;
                                client.resolve_station_url(&station.stationuuid).await
                            };
                            let _ = tx.send(InternalMsg::ResolveDone { station, res: res.map(|u| u.to_string()) });
                        });
                    }
                    UiCommand::TogglePause => {
                        state.error = None;
                        let _ = mpv.command(MpvCommand::TogglePause);
                    }
                    UiCommand::Stop => {
                        state.error = None;
                        let _ = mpv.command(MpvCommand::Stop);
                        let _ = mpv.command(MpvCommand::SetTitle(String::new()));

                        current_url = None;
                        want_paused = false;

                        // Stop forgets the current station
                        state.station = None;
                        state.media_title = None;
                        state.phase = PlaybackPhase::NotConfigured;

                        let _ = state_tx.send(state.clone());

                        // Clear persisted last station too
                        config.last_station = None;
                        let cfg = config.clone();
                        tokio::spawn(async move {
                            let _ = tokio::task::spawn_blocking(move || cfg.save_atomic()).await;
                        });
                    }

                    UiCommand::ToggleFavorite(station) => {
                        config.toggle_favorite(station);
                        state.favorites = config.favorites.clone();
                        let _ = state_tx.send(state.clone());
                        let cfg = config.clone();
                        tokio::spawn(async move {
                            let _ = tokio::task::spawn_blocking(move || cfg.save_atomic()).await;
                        });
                    }
                    UiCommand::Shutdown => {
                        let _ = mpv.command(MpvCommand::Shutdown);
                        return Ok(());
                    }
                }
            }
            ev = mpv_events.recv() => {
                let Some(ev) = ev else {
                    state.phase = PlaybackPhase::Error;
                    state.error = Some("mpv controller stopped".to_string());
                    let _ = state_tx.send(state.clone());
                    return Ok(());
                };
                match ev {
                    MpvEvent::Ready => {
                        if let Some(url) = current_url.clone() {
                            let _ = mpv.command(MpvCommand::LoadUrl { url });
                            let _ = mpv.command(MpvCommand::SetPause(want_paused));
                            state.phase = if want_paused { PlaybackPhase::Paused } else { PlaybackPhase::Playing };
                            state.error = None;
                            let _ = state_tx.send(state.clone());
                        }
                    }
                    MpvEvent::MediaTitle(t) => {
                        state.media_title = t;
                        let _ = state_tx.send(state.clone());
                    }
                    MpvEvent::Pause(p) => {
                        want_paused = p;
                        state.phase = if p { PlaybackPhase::Paused } else { PlaybackPhase::Playing };
                        let _ = state_tx.send(state.clone());
                    }
                    MpvEvent::Crashed(e) => {
                        warn!(error = %e, "mpv crashed/restarting");
                        state.phase = PlaybackPhase::Error;
                        state.error = Some(format!("mpv error: {e}"));
                        let _ = state_tx.send(state.clone());
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                }
            }
            Some(msg) = internal_rx.recv() => {
                match msg {
                    InternalMsg::SearchDone { query, res } => {
                        if query != state.search_query {
                            continue;
                        }
                        match res {
                            Ok(results) => {
                                state.search_results = results;
                                state.search_loading = false;
                                state.error = None;
                            }
                            Err(e) => {
                                state.search_loading = false;
                                state.error = Some(e.to_string());
                            }
                        }
                        let _ = state_tx.send(state.clone());
                    }
                    InternalMsg::ResolveDone { station, res } => {
                        if state.station.as_ref().map(|s| &s.stationuuid) != Some(&station.stationuuid) {
                            continue;
                        }
                        match res {
                            Ok(url) => {
                                info!(stationuuid = %station.stationuuid, "starting playback");
                                current_url = Some(url.clone());
                                let _ = mpv.command(MpvCommand::LoadUrl { url });
                                state.phase = PlaybackPhase::Playing;
                                state.error = None;
                                let _ = state_tx.send(state.clone());

                                config.last_station = Some(station);
                                if let Some(s) = rb.lock().await.last_server().map(|s| s.to_string()) {
                                    config.last_server = Some(s);
                                }
                                let cfg = config.clone();
                                tokio::spawn(async move {
                                    let _ = tokio::task::spawn_blocking(move || cfg.save_atomic()).await;
                                });
                            }
                            Err(e) => {
                                state.phase = PlaybackPhase::Error;
                                state.error = Some(e.to_string());
                                let _ = state_tx.send(state.clone());
                            }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
enum InternalMsg {
    SearchDone { query: String, res: Result<Vec<Station>> },
    ResolveDone { station: StationRef, res: Result<String> },
}

fn mpv_socket_path() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .context("XDG_RUNTIME_DIR not set")?;

    let dir = runtime.join("radiowidget");
    std::fs::create_dir_all(&dir).with_context(|| format!("Create runtime dir: {dir:?}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 700 runtime dir: {dir:?}"))?;
    }
    Ok(dir.join("mpv.sock"))
}

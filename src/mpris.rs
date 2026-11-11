//! MPRIS (org.mpris.MediaPlayer2.crate) so media keys, playerctl and the
//! Omarchy bar can see and drive Crate. mpris-server's Player is !Send, so it
//! lives on its own thread with a current-thread tokio runtime.

use mpris_server::{LoopStatus, Metadata, PlaybackStatus, Player, Time};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy)]
pub enum Transport {
    PlayPause,
    Play,
    Pause,
    Next,
    Prev,
    /// Relative seek in ms.
    Seek(i64),
    /// Absolute position in ms within the current track.
    SetPosition(u64),
    Stop,
    SetShuffle(bool),
    SetRepeat(crate::queue::Repeat),
    /// 0.0..=1.0
    SetVolume(f32),
    /// Bring the window forward (a second launch, or MPRIS Raise).
    Raise,
}

#[derive(Debug, Clone)]
pub enum Update {
    Track { id: String, title: String, artist: String, album: String, length_ms: u64, art_url: Option<String> },
    Status { playing: bool, stopped: bool },
    Position(u64),
    Seeked(u64),
    Modes { shuffle: bool, repeat: crate::queue::Repeat },
    Volume(f64),
}

/// Start the MPRIS server. Returns the sender for state updates.
pub fn start(cmds: mpsc::UnboundedSender<Transport>) -> mpsc::UnboundedSender<Update> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Update>();
    std::thread::Builder::new()
        .name("mpris".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let player = match Player::builder("crate")
                    .identity("Crate")
                    .desktop_entry("crate")
                    .can_play(true)
                    .can_pause(true)
                    .can_go_next(true)
                    .can_go_previous(true)
                    .can_seek(true)
                    .can_control(true)
                    .loop_status(LoopStatus::None)
                    .shuffle(false)
                    .can_raise(true)
                    .build()
                    .await
                {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[mpris] failed to start: {e}");
                        return;
                    }
                };
                let send = |t: Transport| {
                    let c = cmds.clone();
                    move |_: &Player| {
                        let _ = c.send(t);
                    }
                };
                player.connect_play_pause(send(Transport::PlayPause));
                player.connect_play(send(Transport::Play));
                player.connect_pause(send(Transport::Pause));
                player.connect_next(send(Transport::Next));
                player.connect_previous(send(Transport::Prev));
                player.connect_stop(send(Transport::Stop));
                player.connect_raise(send(Transport::Raise));
                {
                    let c = cmds.clone();
                    player.connect_seek(move |_, offset: Time| {
                        let _ = c.send(Transport::Seek(offset.as_millis()));
                    });
                }
                {
                    let c = cmds.clone();
                    player.connect_set_position(move |_, _id, pos: Time| {
                        let _ = c.send(Transport::SetPosition(pos.as_millis().max(0) as u64));
                    });
                }
                {
                    let c = cmds.clone();
                    player.connect_set_volume(move |_, v| {
                        let _ = c.send(Transport::SetVolume(v.clamp(0.0, 1.0) as f32));
                    });
                }
                {
                    let c = cmds.clone();
                    player.connect_set_shuffle(move |_, on| {
                        let _ = c.send(Transport::SetShuffle(on));
                    });
                }
                {
                    let c = cmds.clone();
                    player.connect_set_loop_status(move |_, st| {
                        use crate::queue::Repeat;
                        let r = match st {
                            LoopStatus::None => Repeat::Off,
                            LoopStatus::Playlist => Repeat::All,
                            LoopStatus::Track => Repeat::One,
                        };
                        let _ = c.send(Transport::SetRepeat(r));
                    });
                }
                tokio::task::spawn_local(player.run());
                eprintln!("[mpris] org.mpris.MediaPlayer2.crate ready");
                while let Some(u) = rx.recv().await {
                    let r = match u {
                        Update::Track { id, title, artist, album, length_ms, art_url } => {
                            let mut m = Metadata::builder()
                                .title(title)
                                .artist([artist])
                                .album(album)
                                .length(Time::from_millis(length_ms as i64));
                            if let Ok(tid) = mpris_server::TrackId::try_from(format!("/io/crate/track/{id}")) {
                                m = m.trackid(tid);
                            }
                            if let Some(a) = art_url {
                                m = m.art_url(a);
                            }
                            player.set_metadata(m.build()).await
                        }
                        Update::Status { playing, stopped } => {
                            let st = if stopped {
                                PlaybackStatus::Stopped
                            } else if playing {
                                PlaybackStatus::Playing
                            } else {
                                PlaybackStatus::Paused
                            };
                            player.set_playback_status(st).await
                        }
                        Update::Position(ms) => {
                            player.set_position(Time::from_millis(ms as i64));
                            Ok(())
                        }
                        Update::Seeked(ms) => player.seeked(Time::from_millis(ms as i64)).await,
                        Update::Volume(v) => player.set_volume(v).await,
                        Update::Modes { shuffle, repeat } => {
                            use crate::queue::Repeat;
                            let st = match repeat {
                                Repeat::Off => LoopStatus::None,
                                Repeat::All => LoopStatus::Playlist,
                                Repeat::One => LoopStatus::Track,
                            };
                            match player.set_shuffle(shuffle).await {
                                Ok(()) => player.set_loop_status(st).await,
                                e => e,
                            }
                        }
                    };
                    if let Err(e) = r {
                        eprintln!("[mpris] {e}");
                    }
                }
            });
        })
        .unwrap();
    tx
}

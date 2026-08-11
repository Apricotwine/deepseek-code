//! Embedded PTY terminals for the sidebar — real shell sessions backed by
//! portable-pty, streamed to xterm.js over Tauri events.

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

pub struct TerminalManager {
    sessions: Mutex<HashMap<String, TerminalSession>>,
}

struct TerminalSession {
    master: Box<dyn MasterPty + Send>,
    // The writer must be taken exactly once and kept for the session's life.
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

impl TerminalManager {
    pub fn new() -> Self {
        Self { sessions: Mutex::new(HashMap::new()) }
    }

    /// Spawn the user's default shell in a new PTY and stream its output to
    /// the frontend as `terminal-output` events tagged with the session id.
    pub fn spawn(&self, app: &AppHandle, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let child = pair
            .slave
            .spawn_command(CommandBuilder::new(shell))
            .map_err(|e| e.to_string())?;
        drop(pair.slave);

        let master = pair.master;
        let writer = master.take_writer().map_err(|e| e.to_string())?;
        let reader = master.try_clone_reader().map_err(|e| e.to_string())?;
        let killer = child.clone_killer();
        drop(child);

        let app = app.clone();
        let id_owned = id.to_string();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&buf[..n]).to_string();
                        let _ = app.emit(
                            "terminal-output",
                            serde_json::json!({ "id": id_owned, "data": data }),
                        );
                    }
                }
            }
        });

        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(id.to_string(), TerminalSession { master, writer, killer });
        Ok(())
    }

    pub fn input(&self, id: &str, data: &str) -> Result<(), String> {
        let mut sessions = self.sessions.lock().unwrap();
        let s = sessions.get_mut(id).ok_or("Terminal not found")?;
        s.writer.write_all(data.as_bytes()).map(|_| ()).map_err(|e| e.to_string())
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let sessions = self.sessions.lock().unwrap();
        let s = sessions.get(id).ok_or("Terminal not found")?;
        s.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())
    }

    pub fn kill(&self, id: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(mut s) = sessions.remove(id) {
            let _ = s.killer.kill();
        }
    }
}

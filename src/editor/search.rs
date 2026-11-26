use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use super::Message;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Files,
    Workspace,
    Find,
}

pub struct Hit {
    pub path: PathBuf,
    pub row: usize,
    pub preview: String,
}

pub struct Job(Arc<AtomicBool>);
impl Drop for Job {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub fn start(
    root: PathBuf,
    query: String,
    kind: SearchKind,
    serial: u64,
    tx: std::sync::mpsc::Sender<Message>,
) -> Job {
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        let mut hits = Vec::new();
        let needle = query.to_lowercase();
        let walk = ignore::WalkBuilder::new(&root)
            .sort_by_file_path(|a, b| a.cmp(b))
            .build();
        for entry in walk.flatten() {
            if flag.load(Ordering::Relaxed) {
                return;
            }
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.into_path();
            if kind == SearchKind::Files {
                if path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&needle)
                {
                    hits.push(Hit {
                        path,
                        row: 0,
                        preview: String::new(),
                    });
                }
            } else if let Ok(text) = super::document::read_text(&path) {
                for (row, line) in text.lines().enumerate() {
                    if flag.load(Ordering::Relaxed) {
                        return;
                    }
                    if line.contains(&query) {
                        hits.push(Hit {
                            path: path.clone(),
                            row,
                            preview: line.trim().chars().take(120).collect(),
                        });
                        if hits.len() == 100 {
                            break;
                        }
                    }
                }
            }
            if hits.len() == 100 {
                break;
            }
        }
        let _ = tx.send(Message::Results(serial, hits));
    });
    Job(cancelled)
}

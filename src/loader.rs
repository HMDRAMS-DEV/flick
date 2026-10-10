//! Background decode pool. The UI replaces the whole wish list every time the
//! current photo changes, so stale prefetches never delay the photo on screen.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

use crate::decode::{self, Meta, Size};
use eframe::egui::ColorImage;
use crate::library::Kind;

#[derive(Clone)]
pub struct Job {
    pub id: u32,
    pub size: Size,
    pub path: PathBuf,
    pub kind: Kind,
}

pub struct Done {
    pub id: u32,
    pub size: Size,
    pub image: Option<ColorImage>,
    pub meta: Option<Meta>,
}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    ready: Condvar,
}

pub struct Loader {
    shared: Arc<Shared>,
    pub done: Receiver<Done>,
}

impl Loader {
    pub fn new(ctx: eframe::egui::Context) -> Self {
        let shared = Arc::new(Shared { queue: Mutex::new(VecDeque::new()), ready: Condvar::new() });
        let (tx, rx) = channel();
        let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
        for _ in 0..workers {
            let shared = shared.clone();
            let tx: Sender<Done> = tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || loop {
                let job = {
                    let mut q = shared.queue.lock().unwrap();
                    loop {
                        if let Some(j) = q.pop_front() {
                            break j;
                        }
                        q = shared.ready.wait(q).unwrap();
                    }
                };
                let (image, meta) = match decode::load(&job.path, job.kind, job.size) {
                    // Color conversion happens here, off the UI thread.
                    Ok((img, meta)) => (Some(ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.rgba)), meta),
                    Err(_) => (None, None),
                };
                if tx.send(Done { id: job.id, size: job.size, image, meta }).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }
        Self { shared, done: rx }
    }

    /// Replaces queued work with `jobs` (in priority order). Returns the jobs that were
    /// dropped before starting, so the caller can forget it requested them.
    pub fn replace(&self, jobs: Vec<Job>) -> Vec<Job> {
        let mut q = self.shared.queue.lock().unwrap();
        let dropped: Vec<Job> = q.drain(..).collect();
        q.extend(jobs);
        self.shared.ready.notify_all();
        dropped
    }
}

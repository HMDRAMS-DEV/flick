use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Event, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Vec2, ViewportCommand, pos2, vec2,
};

use crate::decode::{Meta, Size};
use crate::library::{self, Kind, Photo, human_bytes};
use crate::loader::{Job, Loader};
use crate::store::{self, Decision};

const BG: Color32 = Color32::from_rgb(14, 14, 15);
const BAR: Color32 = Color32::from_rgb(22, 22, 24);
const TEXT: Color32 = Color32::from_rgb(232, 230, 226);
const DIM: Color32 = Color32::from_rgb(130, 128, 124);
const FAINT: Color32 = Color32::from_rgb(70, 70, 72);
const ACCENT: Color32 = Color32::from_rgb(232, 89, 12);
const KEEP: Color32 = Color32::from_rgb(63, 185, 80);
const DELETE: Color32 = Color32::from_rgb(240, 70, 60);

/// One knob for all text sizes.
const TEXT_SCALE: f32 = 1.25;

fn font(pt: f32) -> FontId {
    FontId::proportional(pt * TEXT_SCALE)
}

fn mono(pt: f32) -> FontId {
    FontId::monospace(pt * TEXT_SCALE)
}

const TOP_H: f32 = 46.0;
const STRIP_H: f32 = 86.0;
const HINT_H: f32 = 32.0;
const CELL: Vec2 = vec2(78.0, 54.0);
const GAP: f32 = 4.0;
/// Previews kept decoded around the current photo.
const AHEAD: usize = 8;
const BEHIND: usize = 3;

#[derive(PartialEq)]
enum Screen {
    Start,
    Cull,
    Review,
}

pub struct App {
    screen: Screen,
    root: Option<PathBuf>,
    photos: Vec<Photo>,
    index_of: HashMap<u32, usize>,
    decisions: HashMap<String, Decision>,
    cur: usize,
    forward: bool,
    undo: Vec<(usize, Option<Decision>)>,
    zoom: bool,
    help: bool,
    /// When the current photo last changed; full resolution is prefetched once it settles.
    cur_since: f64,

    loader: Loader,
    requested: HashSet<(u32, Size)>,
    failed: HashSet<(u32, Size)>,
    thumbs: HashMap<u32, TextureHandle>,
    previews: HashMap<u32, TextureHandle>,
    full: Option<(u32, TextureHandle)>,
    meta: HashMap<u32, Meta>,
    last_wish: Vec<(u32, Size)>,
    review_visible: (usize, usize),

    scanning: Option<Receiver<(PathBuf, Vec<Photo>)>>,
    toast: Option<(String, f64)>,
    cards: Vec<PathBuf>,
    recents: Vec<PathBuf>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext, open: Option<PathBuf>) -> Self {
        install_fonts(&cc.egui_ctx);
        let mut app = Self {
            screen: Screen::Start,
            root: None,
            photos: vec![],
            index_of: HashMap::new(),
            decisions: HashMap::new(),
            cur: 0,
            forward: true,
            undo: vec![],
            zoom: false,
            help: false,
            cur_since: 0.0,
            loader: Loader::new(cc.egui_ctx.clone()),
            requested: HashSet::new(),
            failed: HashSet::new(),
            thumbs: HashMap::new(),
            previews: HashMap::new(),
            full: None,
            meta: HashMap::new(),
            last_wish: vec![],
            review_visible: (0, 0),
            scanning: None,
            toast: None,
            cards: store::cards(),
            recents: store::recents(),
        };
        if let Some(p) = open {
            app.open(p, &cc.egui_ctx);
        }
        app
    }

    // ---------- folder lifecycle ----------

    fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        let path = if path.is_file() { path.parent().map(Path::to_path_buf).unwrap_or(path) } else { path };
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let p = path.clone();
        std::thread::spawn(move || {
            let photos = library::scan(&p);
            let _ = tx.send((p, photos));
            ctx.request_repaint();
        });
        self.scanning = Some(rx);
        self.root = Some(path);
    }

    fn finish_scan(&mut self, root: PathBuf, photos: Vec<Photo>) {
        self.loader.replace(vec![]);
        self.thumbs.clear();
        self.previews.clear();
        self.full = None;
        self.meta.clear();
        self.requested.clear();
        self.failed.clear();
        self.last_wish.clear();
        self.undo.clear();
        self.zoom = false;
        self.decisions = store::load_decisions(&root);
        self.photos = photos;
        self.reindex();
        // Resume at the first undecided photo.
        self.cur = self.photos.iter().position(|p| !self.decisions.contains_key(&p.key)).unwrap_or(0);
        store::push_recent(&root);
        self.recents = store::recents();
        self.screen = Screen::Cull;
        if self.photos.is_empty() {
            self.flash(format!("No photos in {}", root.display()));
        }
    }

    fn reindex(&mut self) {
        self.index_of = self.photos.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    }

    fn close(&mut self) {
        self.screen = Screen::Start;
        self.photos.clear();
        self.root = None;
        self.cards = store::cards();
        self.recents = store::recents();
    }

    fn flash(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), 0.0));
    }

    // ---------- decisions ----------

    fn decision(&self, i: usize) -> Option<Decision> {
        self.photos.get(i).and_then(|p| self.decisions.get(&p.key).copied())
    }

    fn set_decision(&mut self, i: usize, d: Option<Decision>, record: bool) {
        let Some(p) = self.photos.get(i) else { return };
        let prev = match d {
            Some(d) => self.decisions.insert(p.key.clone(), d),
            None => self.decisions.remove(&p.key),
        };
        if record && prev != d {
            self.undo.push((i, prev));
        }
        if let Some(root) = &self.root {
            store::save_decisions(root, &self.decisions);
        }
    }

    fn decide(&mut self, d: Decision) {
        if self.photos.is_empty() {
            return;
        }
        self.set_decision(self.cur, Some(d), true);
        if self.cur + 1 < self.photos.len() {
            self.go(self.cur + 1);
        } else if self.counts().2 == 0 {
            self.flash("All decided. Press Enter to review deletions.");
        }
    }

    fn undo(&mut self) {
        if let Some((i, prev)) = self.undo.pop() {
            self.set_decision(i, prev, false);
            self.go(i);
        }
    }

    fn counts(&self) -> (usize, usize, usize) {
        let (mut k, mut d) = (0, 0);
        for p in &self.photos {
            match self.decisions.get(&p.key) {
                Some(Decision::Keep) => k += 1,
                Some(Decision::Delete) => d += 1,
                None => {}
            }
        }
        (k, d, self.photos.len() - k - d)
    }

    fn marked(&self) -> Vec<usize> {
        (0..self.photos.len()).filter(|&i| self.decision(i) == Some(Decision::Delete)).collect()
    }

    fn go(&mut self, i: usize) {
        if self.photos.is_empty() {
            return;
        }
        let i = i.min(self.photos.len() - 1);
        if i != self.cur {
            self.forward = i > self.cur;
            self.cur = i;
            self.zoom = false;
            self.cur_since = 0.0;
        }
    }

    fn step(&mut self, delta: isize) {
        let n = self.photos.len() as isize;
        if n > 0 {
            self.go((self.cur as isize + delta).clamp(0, n - 1) as usize);
        }
    }

    fn next_undecided(&mut self, forward: bool) {
        let n = self.photos.len();
        for k in 1..=n {
            let i = if forward { (self.cur + k) % n } else { (self.cur + n - k % n) % n };
            if self.decision(i).is_none() {
                self.go(i);
                return;
            }
        }
        self.flash("Nothing left undecided.");
    }

    /// Permanently removes every file of every photo marked Delete.
    fn delete_marked(&mut self) {
        let marked = self.marked();
        if marked.is_empty() {
            return;
        }
        let mut removed: HashSet<u32> = HashSet::new();
        let mut freed = 0u64;
        let mut errors: Vec<String> = vec![];
        for &i in &marked {
            let p = &self.photos[i];
            let mut ok = true;
            for f in &p.files {
                // exFAT cards keep macOS metadata in "._<name>" companions; remove those too.
                if let (Some(dir), Some(name)) = (f.parent(), f.file_name()) {
                    let _ = std::fs::remove_file(dir.join(format!("._{}", name.to_string_lossy())));
                }
                match std::fs::remove_file(f) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        ok = false;
                        errors.push(format!("{}: {e}", f.display()));
                    }
                }
            }
            if ok {
                removed.insert(p.id);
                freed += p.bytes;
            }
        }
        let cur_id = self.photos.get(self.cur).map(|p| p.id);
        for p in self.photos.iter().filter(|p| removed.contains(&p.id)) {
            self.decisions.remove(&p.key);
        }
        self.photos.retain(|p| !removed.contains(&p.id));
        for id in &removed {
            self.thumbs.remove(id);
            self.previews.remove(id);
            self.meta.remove(id);
        }
        self.full = None;
        self.reindex();
        self.undo.clear();
        if let Some(root) = &self.root {
            store::save_decisions(root, &self.decisions);
        }
        // Stay near where the user was.
        self.cur = cur_id
            .and_then(|id| self.index_of.get(&id).copied())
            .unwrap_or_else(|| self.photos.iter().position(|p| !self.decisions.contains_key(&p.key)).unwrap_or(0))
            .min(self.photos.len().saturating_sub(1));
        self.screen = Screen::Cull;
        let msg = format!("Deleted {} photos, freed {}", removed.len(), human_bytes(freed));
        if errors.is_empty() {
            self.flash(msg);
        } else {
            self.flash(format!("{msg}. {} files failed: {}", errors.len(), errors[0]));
        }
    }

    // ---------- loading ----------

    fn pump_loader(&mut self, ctx: &egui::Context) {
        while let Ok(done) = self.loader.done.try_recv() {
            self.requested.remove(&(done.id, done.size));
            if !self.index_of.contains_key(&done.id) {
                continue;
            }
            if let Some(m) = done.meta {
                self.meta.insert(done.id, m);
            }
            let Some(img) = done.image else {
                self.failed.insert((done.id, done.size));
                continue;
            };
            let name = format!("{}-{:?}", done.id, done.size);
            let options = if done.size == Size::Full {
                // Full res is shown scaled down until zoomed; mipmaps keep that clean.
                TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear))
            } else {
                TextureOptions::LINEAR
            };
            let tex = ctx.load_texture(name, img, options);
            match done.size {
                Size::Thumb => {
                    self.thumbs.insert(done.id, tex);
                }
                Size::Preview => {
                    self.previews.insert(done.id, tex);
                }
                Size::Full => {
                    if self.photos.get(self.cur).map(|p| p.id) == Some(done.id) {
                        self.full = Some((done.id, tex));
                    }
                }
            }
        }
    }

    fn have(&self, id: u32, size: Size) -> bool {
        match size {
            Size::Thumb => self.thumbs.contains_key(&id),
            Size::Preview => self.previews.contains_key(&id),
            Size::Full => self.full.as_ref().is_some_and(|f| f.0 == id),
        }
    }

    /// Builds the priority-ordered wish list and hands it to the loader when it changes.
    fn schedule(&mut self, ctx: &egui::Context, strip_range: (usize, usize)) {
        // Full resolution for the current photo once the user pauses on it, so zoom is instant.
        let now = ctx.input(|i| i.time);
        if self.cur_since == 0.0 {
            self.cur_since = now;
        }
        let settled = now - self.cur_since > 0.12;
        if !settled {
            ctx.request_repaint_after(std::time::Duration::from_millis(130));
        }
        let n = self.photos.len();
        let mut wish: Vec<(u32, Size)> = vec![];
        let push = |wish: &mut Vec<(u32, Size)>, i: usize, s: Size| {
            if i < n {
                wish.push((self.photos[i].id, s));
            }
        };
        if self.screen == Screen::Review {
            let m = self.marked();
            for &i in m.iter().skip(self.review_visible.0).take(self.review_visible.1) {
                push(&mut wish, i, Size::Thumb);
            }
        } else if n > 0 {
            let c = self.cur;
            if self.zoom {
                push(&mut wish, c, Size::Full);
            }
            push(&mut wish, c, Size::Thumb);
            push(&mut wish, c, Size::Preview);
            let (ahead, behind) = if self.forward { (AHEAD, BEHIND) } else { (BEHIND, AHEAD) };
            for k in 1..=ahead.max(behind) {
                if k <= ahead {
                    push(&mut wish, c + k, Size::Preview);
                }
                if k <= behind && c >= k {
                    push(&mut wish, c - k, Size::Preview);
                }
                if k == 2 && settled && !self.zoom && self.photos[c].kind != Kind::Video {
                    push(&mut wish, c, Size::Full);
                }
            }
            for i in strip_range.0..strip_range.1 {
                push(&mut wish, i, Size::Thumb);
            }
        }
        wish.retain(|&(id, s)| !self.have(id, s) && !self.failed.contains(&(id, s)));
        if wish == self.last_wish {
            return;
        }
        self.last_wish = wish.clone();
        let jobs: Vec<Job> = wish
            .iter()
            .filter(|k| !self.requested.contains(k))
            .map(|&(id, size)| {
                let p = &self.photos[self.index_of[&id]];
                Job { id, size, path: p.primary.clone(), kind: p.kind }
            })
            .collect();
        for j in self.loader.replace(jobs.clone()) {
            self.requested.remove(&(j.id, j.size));
        }
        for j in jobs {
            self.requested.insert((j.id, j.size));
        }

        // Evict previews far from the cursor.
        let (lo, hi) = (self.cur.saturating_sub(AHEAD + 2), self.cur + AHEAD + 2);
        let index_of = &self.index_of;
        self.previews.retain(|id, _| index_of.get(id).is_some_and(|&i| i >= lo && i <= hi));
        if self.full.as_ref().is_some_and(|f| Some(f.0) != self.photos.get(self.cur).map(|p| p.id)) {
            self.full = None;
        }
    }

    // ---------- input ----------

    fn handle_input(&mut self, ctx: &egui::Context) {
        let (events, dropped) = ctx.input(|i| (i.events.clone(), i.raw.dropped_files.clone()));
        if let Some(path) = dropped.iter().map(|f| f.path().to_path_buf()).find(|p| !p.as_os_str().is_empty()) {
            self.open(path, ctx);
            return;
        }
        for ev in events {
            match ev {
                Event::Text(t) if t == "?" => self.help = !self.help,
                Event::Key { key, pressed: true, repeat, modifiers, .. } => {
                    self.key(ctx, key, repeat, modifiers);
                }
                _ => {}
            }
        }
    }

    fn key(&mut self, ctx: &egui::Context, key: Key, repeat: bool, m: egui::Modifiers) {
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if key == Key::F && !m.command {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen));
            return;
        }
        if (key == Key::O && !m.shift) || (key == Key::O && m.command) {
            if let Some(p) = rfd::FileDialog::new().set_title("Choose a folder of photos").pick_folder() {
                self.open(p, ctx);
            }
            return;
        }
        if key == Key::H {
            self.help = !self.help;
            return;
        }
        if key == Key::Escape && self.help {
            self.help = false;
            return;
        }

        match self.screen {
            Screen::Start => self.key_start(ctx, key),
            Screen::Review => match key {
                Key::Escape | Key::Backspace => self.screen = Screen::Cull,
                Key::Enter if !repeat => self.delete_marked(),
                _ => {}
            },
            Screen::Cull => {
                let decide_ok = !repeat;
                match key {
                    Key::ArrowRight => self.step(1),
                    Key::ArrowLeft => self.step(-1),
                    Key::Space => self.step(if m.shift { -1 } else { 1 }),
                    Key::ArrowUp | Key::K | Key::P if decide_ok => self.decide(Decision::Keep),
                    Key::ArrowDown | Key::X | Key::D if decide_ok => self.decide(Decision::Delete),
                    Key::U | Key::Num0 => self.set_decision(self.cur, None, true),
                    Key::Z if m.command => self.undo(),
                    Key::Backspace if !m.command => self.undo(),
                    Key::Z => self.zoom = !self.zoom,
                    Key::Tab => self.next_undecided(!m.shift),
                    Key::Home => self.go(0),
                    Key::End => self.go(usize::MAX),
                    Key::Enter | Key::Delete => self.open_review(),
                    Key::Backspace if m.command => self.open_review(),
                    Key::Escape if self.zoom => self.zoom = false,
                    Key::Escape if fullscreen => ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false)),
                    Key::W if m.command => self.close(),
                    _ => {}
                }
            }
        }
    }

    fn key_start(&mut self, ctx: &egui::Context, key: Key) {
        let targets: Vec<PathBuf> = self.cards.iter().chain(self.recents.iter()).cloned().collect();
        let pick = match key {
            Key::Enter => targets.first().cloned(),
            k => {
                let n = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9]
                    .iter()
                    .position(|&x| x == k);
                n.and_then(|n| targets.get(n).cloned())
            }
        };
        if let Some(p) = pick {
            self.open(p, ctx);
        }
    }

    fn open_review(&mut self) {
        if self.marked().is_empty() {
            self.flash("Nothing marked for deletion.");
        } else {
            self.screen = Screen::Review;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(rx) = &self.scanning
            && let Ok((root, photos)) = rx.try_recv()
        {
            self.scanning = None;
            self.finish_scan(root, photos);
        }
        self.pump_loader(&ctx);
        self.handle_input(&ctx);

        let full = ui.max_rect();
        let painter = ui.painter().clone();
        painter.rect_filled(full, 0.0, BG);

        let strip_range = match self.screen {
            Screen::Start => {
                self.draw_start(ui, full);
                (0, 0)
            }
            Screen::Cull => self.draw_cull(ui, full),
            Screen::Review => {
                self.draw_review(ui, full);
                (0, 0)
            }
        };
        self.draw_top_bar(ui, full);
        if self.help {
            draw_help(&painter, full);
        }
        self.draw_toast(&ctx, &painter, full);
        self.schedule(&ctx, strip_range);
    }
}

// ---------- drawing ----------

impl App {
    fn draw_top_bar(&mut self, ui: &mut egui::Ui, full: Rect) {
        let p = ui.painter();
        let bar = Rect::from_min_size(full.min, vec2(full.width(), TOP_H));
        p.rect_filled(bar, 0.0, BAR);
        // Drag the window by its top bar (the OS title bar is hidden).
        let r = ui.interact(bar, ui.id().with("drag"), Sense::click_and_drag());
        if r.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if r.double_clicked() {
            let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
            ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!max));
        }
        let y = bar.center().y;
        let left = full.left() + 84.0; // clears the traffic lights
        let f = font(13.0);
        p.text(pos2(left, y), Align2::LEFT_CENTER, "Flick", font(13.5), ACCENT);
        if self.scanning.is_some() {
            let name = self.root.as_ref().map(|r| r.display().to_string()).unwrap_or_default();
            p.text(pos2(left + 58.0, y), Align2::LEFT_CENTER, format!("Scanning {name}…"), f, DIM);
            return;
        }
        if self.screen == Screen::Start {
            return;
        }
        // Last two path components, so a card shows as "Sigma BF / DCIM".
        let folder = self.root.as_ref().map(|r| {
            let parts: Vec<_> = r.components().rev().take(2).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            parts.into_iter().rev().filter(|c| c != "/" && c != "Volumes").collect::<Vec<_>>().join(" / ")
        }).unwrap_or_default();
        p.text(pos2(left + 58.0, y), Align2::LEFT_CENTER, folder, f.clone(), DIM);

        if let Some(ph) = self.photos.get(self.cur).filter(|_| self.screen == Screen::Cull) {
            let meta = self.meta.get(&ph.id);
            let mut s = ph.name.clone();
            if let Some(m) = meta
                && !m.exposure.is_empty()
            {
                s = format!("{s}      {}", m.exposure);
            }
            p.text(pos2(full.center().x, y), Align2::CENTER_CENTER, s, f.clone(), TEXT);
        }
        let (k, d, left_n) = self.counts();
        let right = full.right() - 16.0;
        let parts = [
            (format!("{left_n} left"), DIM),
            (format!("{d} delete"), if d > 0 { DELETE } else { DIM }),
            (format!("{k} keep"), if k > 0 { KEEP } else { DIM }),
            (format!("{} / {}", (self.cur + 1).min(self.photos.len()), self.photos.len()), TEXT),
        ];
        let mut x = right;
        for (t, c) in parts {
            let r = p.text(pos2(x, y), Align2::RIGHT_CENTER, t, f.clone(), c);
            x = r.left() - 20.0;
        }
    }

    fn draw_start(&mut self, ui: &mut egui::Ui, full: Rect) {
        let p = ui.painter().clone();
        let c = full.center();
        let top = c.y - 140.0;
        p.text(pos2(c.x, top), Align2::CENTER_CENTER, "Flick", font(44.0), TEXT);
        p.text(
            pos2(c.x, top + 48.0),
            Align2::CENTER_CENTER,
            "Fly through a folder of photos. Keep or delete each one with a single key.",
            font(14.0),
            DIM,
        );
        p.text(
            pos2(c.x, top + 76.0),
            Align2::CENTER_CENTER,
            "Drop a folder anywhere on this window, or pick one below.",
            font(14.0),
            DIM,
        );

        let mut rows: Vec<(String, String, Option<PathBuf>)> = vec![];
        let targets: Vec<(PathBuf, bool)> =
            self.cards.iter().map(|p| (p.clone(), true)).chain(self.recents.iter().map(|p| (p.clone(), false))).collect();
        for (n, (path, card)) in targets.iter().enumerate().take(9) {
            let key = if n == 0 { "Enter".to_string() } else { format!("{}", n + 1) };
            let label = if *card {
                format!("{}  (camera card)", path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
            } else {
                tilde(path)
            };
            rows.push((key, label, Some(path.clone())));
        }
        rows.push(("O".into(), "Choose a folder…".into(), None));

        let w = 600.0;
        let mut y = top + 130.0;
        for (key, label, path) in rows {
            let r = Rect::from_min_size(pos2(c.x - w / 2.0, y), vec2(w, 46.0));
            let resp = ui.interact(r, ui.id().with(("row", &label)), Sense::click());
            p.rect_filled(r, 0.0, if resp.hovered() { Color32::from_rgb(34, 34, 37) } else { BAR });
            p.text(pos2(r.left() + 14.0, r.center().y), Align2::LEFT_CENTER, &key, mono(12.0), ACCENT);
            p.text(pos2(r.left() + 96.0, r.center().y), Align2::LEFT_CENTER, &label, font(14.0), TEXT);
            if resp.clicked() {
                match path {
                    Some(p) => self.open(p, ui.ctx()),
                    None => {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            self.open(p, ui.ctx());
                        }
                    }
                }
            }
            y += 52.0;
        }
        p.text(
            pos2(c.x, y + 24.0),
            Align2::CENTER_CENTER,
            "From a terminal:  flick <folder>",
            mono(12.0),
            FAINT,
        );
    }

    /// Returns the filmstrip index range so its thumbnails can be scheduled.
    fn draw_cull(&mut self, ui: &mut egui::Ui, full: Rect) -> (usize, usize) {
        let p = ui.painter().clone();
        let stage = Rect::from_min_max(pos2(full.left(), full.top() + TOP_H), pos2(full.right(), full.bottom() - STRIP_H - HINT_H));
        let strip = Rect::from_min_max(pos2(full.left(), stage.bottom()), pos2(full.right(), full.bottom() - HINT_H));
        let hint = Rect::from_min_max(pos2(full.left(), strip.bottom()), full.max);

        if self.photos.is_empty() {
            if self.scanning.is_none() {
                p.text(stage.center(), Align2::CENTER_CENTER, "No photos here. Press O to pick another folder.", font(15.0), DIM);
            }
            return (0, 0);
        }

        // Photo stage.
        let ph = &self.photos[self.cur];
        let id = ph.id;
        let area = stage.shrink(12.0);
        let resp = ui.interact(stage, ui.id().with("stage"), Sense::click());
        if resp.clicked() {
            self.zoom = !self.zoom;
        }
        let tex = self.full.as_ref().filter(|f| f.0 == id).map(|f| &f.1)
            .or_else(|| self.previews.get(&id))
            .or_else(|| self.thumbs.get(&id));

        if let Some(tex) = tex {
            let ts = tex.size_vec2();
            let fit = (area.width() / ts.x).min(area.height() / ts.y);
            let fitted = Rect::from_center_size(area.center(), ts * fit);
            let rect = if self.zoom {
                // 1:1 on physical pixels. Pan follows the pointer, so the edge you point at is the edge you see.
                let ppp = ui.ctx().pixels_per_point();
                let dims = self.meta.get(&id).and_then(|m| m.dims).map(|(w, h)| vec2(w as f32, h as f32));
                let size = dims.map(|d| d / ppp).unwrap_or(ts * fit * 2.0);
                let ptr = ui.ctx().pointer_hover_pos().unwrap_or(area.center());
                let fx = ((ptr.x - fitted.left()) / fitted.width()).clamp(0.0, 1.0);
                let fy = ((ptr.y - fitted.top()) / fitted.height()).clamp(0.0, 1.0);
                let x = if size.x > stage.width() { stage.left() - fx * (size.x - stage.width()) } else { stage.center().x - size.x / 2.0 };
                let y = if size.y > stage.height() { stage.top() - fy * (size.y - stage.height()) } else { stage.center().y - size.y / 2.0 };
                Rect::from_min_size(pos2(x, y), size)
            } else {
                fitted
            };
            p.with_clip_rect(stage).image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);

            if !self.zoom {
                if let Some(d) = self.decision(self.cur) {
                    let (c, label) = match d {
                        Decision::Keep => (KEEP, "KEEP"),
                        Decision::Delete => (DELETE, "DELETE"),
                    };
                    if d == Decision::Delete {
                        p.rect_filled(fitted, 0.0, Color32::from_black_alpha(110));
                    }
                    p.rect_stroke(fitted, 0.0, Stroke::new(3.0, c), StrokeKind::Inside);
                    let tag = Rect::from_min_size(fitted.min + vec2(12.0, 12.0), vec2(if d == Decision::Keep { 62.0 } else { 84.0 }, 28.0));
                    p.rect_filled(tag, 0.0, c);
                    p.text(tag.center(), Align2::CENTER_CENTER, label, font(12.5), Color32::BLACK);
                }
            } else if self.full.as_ref().is_none_or(|f| f.0 != id) && ph.kind != Kind::Video {
                p.text(stage.left_bottom() + vec2(14.0, -14.0), Align2::LEFT_BOTTOM, "Loading full resolution…", font(12.0), TEXT);
            }
        } else {
            let msg = if ph.kind == Kind::Video {
                format!("{}\nVideo. Keep or delete it like a photo.", ph.name)
            } else if self.failed.contains(&(id, Size::Preview)) {
                format!("{}\nNo preview available.", ph.name)
            } else {
                String::new()
            };
            p.text(stage.center(), Align2::CENTER_CENTER, msg, font(15.0), DIM);
            if let Some(d) = self.decision(self.cur) {
                let c = if d == Decision::Keep { KEEP } else { DELETE };
                p.rect_stroke(area, 0.0, Stroke::new(3.0, c), StrokeKind::Inside);
            }
        }

        // Filmstrip, current photo centered.
        p.rect_filled(strip, 0.0, BAR);
        let slot = CELL.x + GAP;
        let half = ((strip.width() / slot) / 2.0).ceil() as isize + 1;
        let n = self.photos.len() as isize;
        let lo = (self.cur as isize - half).max(0);
        let hi = (self.cur as isize + half + 1).min(n);
        let cy = strip.center().y - 2.0;
        let strip_resp = ui.interact(strip, ui.id().with("strip"), Sense::hover());
        for i in lo..hi {
            let x = strip.center().x + (i - self.cur as isize) as f32 * slot;
            let cell = Rect::from_center_size(pos2(x, cy), CELL);
            let i = i as usize;
            let pid = self.photos[i].id;
            let dec = self.decision(i);
            p.rect_filled(cell, 0.0, Color32::from_rgb(30, 30, 32));
            if let Some(t) = self.thumbs.get(&pid) {
                let ts = t.size_vec2();
                let s = (CELL.x / ts.x).min(CELL.y / ts.y);
                let tint = if dec == Some(Decision::Delete) { Color32::from_gray(70) } else { Color32::WHITE };
                p.image(t.id(), Rect::from_center_size(cell.center(), ts * s), Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), tint);
            }
            if let Some(d) = dec {
                let c = if d == Decision::Keep { KEEP } else { DELETE };
                p.rect_filled(Rect::from_min_size(pos2(cell.left(), cell.bottom() + 4.0), vec2(CELL.x, 3.0)), 0.0, c);
            }
            if i == self.cur {
                p.rect_stroke(cell.expand(2.0), 0.0, Stroke::new(2.0, TEXT), StrokeKind::Outside);
            }
            if ui.interact(cell, ui.id().with(("cell", pid)), Sense::click()).clicked() {
                self.go(i);
            }
        }
        if strip_resp.hovered() {
            let dy = ui.ctx().input(|i| i.smooth_scroll_delta);
            let d = if dy.x.abs() > dy.y.abs() { -dy.x } else { -dy.y };
            if d.abs() > 0.5 {
                self.step(d.signum() as isize);
            }
        }

        // Key hints.
        let hints = "←  →  browse      ↑ / K  keep      ↓ / X  delete      Z  zoom      Tab  next undecided      ⌫  undo      Enter  review & delete      H  help";
        p.text(hint.center(), Align2::CENTER_CENTER, hints, font(12.0), DIM);

        (lo as usize, hi as usize)
    }

    fn draw_review(&mut self, ui: &mut egui::Ui, full: Rect) {
        let p = ui.painter().clone();
        let marked = self.marked();
        if marked.is_empty() {
            self.screen = Screen::Cull;
            return;
        }
        let files: usize = marked.iter().map(|&i| self.photos[i].files.len()).sum();
        let bytes: u64 = marked.iter().map(|&i| self.photos[i].bytes).sum();
        let head = Rect::from_min_max(pos2(full.left(), full.top() + TOP_H), pos2(full.right(), full.top() + TOP_H + 104.0));
        p.text(
            pos2(head.left() + 28.0, head.top() + 30.0),
            Align2::LEFT_CENTER,
            format!("Delete {} photos permanently?", marked.len()),
            font(22.0),
            TEXT,
        );
        p.text(
            pos2(head.left() + 28.0, head.top() + 66.0),
            Align2::LEFT_CENTER,
            format!("{files} files, {}. They skip the Trash and cannot be recovered. Click a photo to keep it instead.", human_bytes(bytes)),
            font(13.5),
            DIM,
        );
        let btn = Rect::from_min_size(pos2(head.right() - 300.0, head.top() + 26.0), vec2(272.0, 44.0));
        let back = Rect::from_min_size(pos2(btn.left() - 140.0, btn.top()), vec2(128.0, 44.0));
        let br = ui.interact(btn, ui.id().with("del"), Sense::click());
        let bk = ui.interact(back, ui.id().with("back"), Sense::click());
        p.rect_filled(btn, 0.0, if br.hovered() { Color32::from_rgb(255, 95, 80) } else { DELETE });
        p.text(btn.center(), Align2::CENTER_CENTER, "Delete permanently   Enter", font(14.0), Color32::BLACK);
        p.rect_stroke(back, 0.0, Stroke::new(1.0, if bk.hovered() { TEXT } else { FAINT }), StrokeKind::Inside);
        p.text(back.center(), Align2::CENTER_CENTER, "Back   Esc", font(14.0), TEXT);
        if br.clicked() {
            self.delete_marked();
            return;
        }
        if bk.clicked() {
            self.screen = Screen::Cull;
            return;
        }

        let grid = Rect::from_min_max(pos2(full.left() + 20.0, head.bottom()), pos2(full.right() - 20.0, full.bottom() - 12.0));
        let cell = vec2(150.0, 118.0);
        let cols = ((grid.width() + 8.0) / (cell.x + 8.0)).floor().max(1.0) as usize;
        let rows = marked.len().div_ceil(cols);
        let mut unmark = None;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(grid));
        egui::ScrollArea::vertical().auto_shrink(false).show(&mut child, |ui| {
            let (area, _) = ui.allocate_exact_size(vec2(grid.width(), rows as f32 * (cell.y + 8.0)), Sense::hover());
            let clip = ui.clip_rect();
            let first_row = (((clip.top() - area.top()) / (cell.y + 8.0)).floor().max(0.0)) as usize;
            let vis_rows = (clip.height() / (cell.y + 8.0)).ceil() as usize + 1;
            self.review_visible = (first_row * cols, vis_rows * cols);
            let p = ui.painter();
            for (k, &i) in marked.iter().enumerate().skip(first_row * cols).take(vis_rows * cols) {
                let (r, c) = (k / cols, k % cols);
                let rect = Rect::from_min_size(area.min + vec2(c as f32 * (cell.x + 8.0), r as f32 * (cell.y + 8.0)), cell);
                let ph = &self.photos[i];
                let resp = ui.interact(rect, ui.id().with(("rev", ph.id)), Sense::click());
                p.rect_filled(rect, 0.0, BAR);
                let img = rect.shrink2(vec2(0.0, 10.0)).translate(vec2(0.0, -8.0));
                if let Some(t) = self.thumbs.get(&ph.id) {
                    let ts = t.size_vec2();
                    let s = (img.width() / ts.x).min(img.height() / ts.y);
                    p.image(t.id(), Rect::from_center_size(img.center(), ts * s), Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
                }
                p.text(pos2(rect.center().x, rect.bottom() - 10.0), Align2::CENTER_CENTER, &ph.name, font(11.0), DIM);
                if resp.hovered() {
                    p.rect_stroke(rect, 0.0, Stroke::new(2.0, KEEP), StrokeKind::Inside);
                    p.text(rect.center(), Align2::CENTER_CENTER, "Keep", font(16.0), KEEP);
                }
                if resp.clicked() {
                    unmark = Some(i);
                }
            }
        });
        if let Some(i) = unmark {
            self.set_decision(i, Some(Decision::Keep), true);
        }
    }

    fn draw_toast(&mut self, ctx: &egui::Context, p: &egui::Painter, full: Rect) {
        let now = ctx.input(|i| i.time);
        let Some((msg, t0)) = &mut self.toast else { return };
        if *t0 == 0.0 {
            *t0 = now;
        }
        let age = now - *t0;
        if age > 3.0 {
            self.toast = None;
            return;
        }
        let alpha = ((3.0 - age) / 0.3).min(1.0) as f32;
        let f = font(14.0);
        let galley = p.layout_no_wrap(msg.clone(), f, TEXT.gamma_multiply(alpha));
        let y = full.bottom() - STRIP_H - HINT_H - 40.0;
        let r = Rect::from_center_size(pos2(full.center().x, y), galley.size() + vec2(28.0, 16.0));
        p.rect_filled(r, CornerRadius::ZERO, Color32::from_black_alpha((220.0 * alpha) as u8));
        p.galley(r.min + vec2(14.0, 8.0), galley, TEXT);
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

fn draw_help(p: &egui::Painter, full: Rect) {
    let rows = [
        ("→  Space", "Next photo"),
        ("←  ⇧Space", "Previous photo"),
        ("↑  K", "Keep, then next"),
        ("↓  X  D", "Mark for deletion, then next"),
        ("U", "Clear the decision"),
        ("⌫  ⌘Z", "Undo the last decision"),
        ("Z  click", "Zoom to 100%. Move the pointer to pan."),
        ("Tab  ⇧Tab", "Next / previous undecided photo"),
        ("Home  End", "First / last photo"),
        ("Enter", "Review marked photos, then delete them permanently"),
        ("F", "Full screen"),
        ("O", "Open another folder (or drop one on the window)"),
        ("⌘W", "Close folder"),
        ("H  ?", "This help"),
    ];
    p.rect_filled(full, 0.0, Color32::from_black_alpha(200));
    let w = 700.0;
    let h = 70.0 + rows.len() as f32 * 34.0;
    let r = Rect::from_center_size(full.center(), vec2(w, h));
    p.rect_filled(r, 0.0, BAR);
    p.text(pos2(r.left() + 28.0, r.top() + 34.0), Align2::LEFT_CENTER, "Keys", font(18.0), TEXT);
    for (k, (key, what)) in rows.iter().enumerate() {
        let y = r.top() + 80.0 + k as f32 * 34.0;
        p.text(pos2(r.left() + 28.0, y), Align2::LEFT_CENTER, *key, font(14.0), ACCENT);
        p.text(pos2(r.left() + 200.0, y), Align2::LEFT_CENTER, *what, font(14.0), TEXT);
    }
}

fn tilde(p: &Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(h).ok().map(|r| r.to_path_buf())) {
        Some(r) => format!("~/{}", r.display()),
        None => p.display().to_string(),
    }
}

/// Uses the macOS system font so text matches the OS and arrow/⌘ glyphs render.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, path) in [("sf", "/System/Library/Fonts/SFNS.ttf"), ("sfmono", "/System/Library/Fonts/SFNSMono.ttf")] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(name.into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            let fam = if name == "sf" { egui::FontFamily::Proportional } else { egui::FontFamily::Monospace };
            fonts.families.entry(fam).or_default().insert(0, name.into());
        }
    }
    ctx.set_fonts(fonts);
}

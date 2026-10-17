#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod decode;
mod export;
mod fix;
mod library;
mod loader;
mod store;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The tools Claude uses during a fix, served to `claude -p` (see fix.rs).
    if args.first().map(String::as_str) == Some("--mcp") {
        fix::serve(std::path::Path::new(args.get(1).expect("usage: flick --mcp <session dir>")));
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--bench") {
        bench(args.get(1).map(PathBuf::from).expect("usage: flick --bench <folder>"));
        return Ok(());
    }
    // Finder launches pass -psn_*; ignore anything that is not a path.
    let open = args.iter().find(|a| !a.starts_with('-')).map(PathBuf::from);

    // `flick <folder>` from a terminal: relaunch through the app bundle so the shell
    // returns immediately and the window gets a proper Dock icon.
    if std::io::IsTerminal::is_terminal(&std::io::stdout())
        && let Some(bundle) = app_bundle()
    {
        let mut cmd = std::process::Command::new("/usr/bin/open");
        cmd.arg("-n").arg(&bundle);
        if let Some(dir) = &open {
            cmd.arg("--args").arg(std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone()));
        }
        let ok = cmd.status().is_ok_and(|s| s.success());
        if ok {
            return Ok(());
        }
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Flick")
            .with_inner_size([1440.0, 940.0])
            .with_min_inner_size([760.0, 520.0])
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Flick", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, open)))))
}

fn app_bundle() -> Option<PathBuf> {
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(PathBuf::from)
}

/// Times scanning and each decode size on real files, without a window.
fn bench(root: PathBuf) {
    use std::time::Instant;
    let t = Instant::now();
    let photos = library::scan(&root);
    println!("scan: {} photos in {:?}", photos.len(), t.elapsed());
    let sample: Vec<_> = photos.iter().filter(|p| p.kind != library::Kind::Video).step_by((photos.len() / 20).max(1)).take(20).collect();
    for size in [decode::Size::Thumb, decode::Size::Preview, decode::Size::Full] {
        let mut times = vec![];
        let mut dims = (0, 0);
        for p in &sample {
            let t = Instant::now();
            match decode::load(&p.primary, p.kind, size) {
                Ok((img, _)) => dims = (img.width, img.height),
                Err(e) => println!("  {}: {e}", p.name),
            }
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = times.len().max(1);
        println!("{size:?}: {}x{}  median {:.1} ms  p90 {:.1} ms  (n={})", dims.0, dims.1, times[n / 2], times[(n * 9 / 10).min(n - 1)], n);
    }
}

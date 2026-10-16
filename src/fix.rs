//! "Fix with Claude": the photographer says or types what's wrong, and a headless `claude -p`
//! run edits the photo in RapidRAW (github.com/CyberTimon/RapidRAW), a raw editor the user
//! installs separately. It is AGPL, so Flick only runs its command line and never ships it.
//! Claude works through two tools Flick serves over MCP (`flick --mcp <session dir>`):
//! `segment` makes masks so an adjustment can apply to part of the photo (Segment Anything 2.1
//! and Apple Vision, in tools/segment.swift), and `render` shows Claude what an edit looks like,
//! so it can check its work and refine it before answering. Follow-up requests refine the last
//! result. `flick-listen` (tools/listen.swift) does the dictation.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Claude's answer: an edit as the `render` tool takes it, and what it did, in words.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Params {
    pub edit: Value,
    pub summary: String,
}

/// One earlier request in a fix and what Claude answered.
#[derive(Clone)]
pub struct Round {
    pub feedback: String,
    pub params: Params,
}

/// Settings an edit and its masks can set: RapidRAW's name, Flick's label, range.
const SETTINGS: &[(&str, &str, f64, f64)] = &[
    ("exposure", "Exposure", -5.0, 5.0),
    ("contrast", "Contrast", -100.0, 100.0),
    ("highlights", "Highlights", -100.0, 100.0),
    ("shadows", "Shadows", -100.0, 100.0),
    ("whites", "Whites", -100.0, 100.0),
    ("blacks", "Blacks", -100.0, 100.0),
    ("temperature", "Temp", -100.0, 100.0),
    ("tint", "Tint", -100.0, 100.0),
    ("vibrance", "Vibrance", -100.0, 100.0),
    ("saturation", "Saturation", -100.0, 100.0),
    ("clarity", "Clarity", -100.0, 100.0),
    ("dehaze", "Dehaze", -100.0, 100.0),
    ("structure", "Structure", -100.0, 100.0),
    ("sharpness", "Sharpness", 0.0, 100.0),
    ("lumaNoiseReduction", "Noise reduction", 0.0, 100.0),
    ("colorNoiseReduction", "Color noise reduction", 0.0, 100.0),
    ("vignetteAmount", "Vignette", -100.0, 100.0),
];
const COLORS: [&str; 8] = ["reds", "oranges", "yellows", "greens", "aquas", "blues", "purples", "magentas"];

const EDIT_DOC: &str = "An edit is a JSON object. Leave out what you don't change; 0 means unchanged. \
Positions are fractions of the photo: [x, y], x from the left, y from the top.
- exposure: EV, -5 to 5. contrast, highlights, shadows, whites, blacks: -100 to 100. Negative highlights \
recovers bright areas; positive shadows lifts dark ones.
- temperature, tint: -100 to 100, relative to the camera's white balance. Positive is warmer, or more magenta. 20 is a clear shift.
- vibrance, saturation, clarity (local contrast), dehaze, structure (fine texture): -100 to 100.
- sharpness, lumaNoiseReduction, colorNoiseReduction: 0 to 100. vignetteAmount: -100 (darker corners) to 100.
- hsl: by color range (reds, oranges, yellows, greens, aquas, blues, purples, magentas), each \
{hue, saturation, luminance}, -100 to 100. Skin is mostly oranges.
- rotation: degrees, -15 to 15, positive turns the photo clockwise. The frame is cropped so no corners show.
- crop: {left, top, right, bottom}, the fraction to trim from each side, 0 to 0.45.
- masks: local adjustments, a list of {name, areas, invert, adjustments}. adjustments takes the settings \
above except hsl, rotation and crop, and adds to the global ones. areas is a list; each area is one of \
{mask: \"<name you gave segment>\"}, {gradient: {from: [x, y], to: [x, y]}} (full strength at from, fading \
to nothing at to), or {ellipse: {center: [x, y], radius: [rx, ry], feather: 0 to 1}} (radius as fractions \
of width and height). An area with subtract: true is cut out of the areas before it. invert: true applies \
the adjustments outside the areas instead.";

const ANSWER: &str = r#"{"type":"object","additionalProperties":false,"required":["edit","summary"],"properties":{"edit":{"type":"object"},"summary":{"type":"string"}}}"#;

/// Edge of the images Claude looks at. Larger costs more and shows nothing new for tone and color.
const ANALYZE_EDGE: u32 = 1568;
/// Edge of the before/after preview shown in Flick.
const PREVIEW_EDGE: u32 = 2400;
/// Renders Claude gets per request, to check and refine its edit.
const MAX_RENDERS: u32 = 5;

const MODEL: &str = "sam2.1-large";
const MODEL_URL: &str = "https://huggingface.co/apple/coreml-sam2.1-large/resolve/main";
const MODEL_PARTS: [&str; 3] = ["ImageEncoder", "PromptEncoder", "MaskDecoder"];

impl Params {
    /// "Exposure +0.4   Shadows +30   Mask “sky”", skipping zeros.
    pub fn describe(&self) -> String {
        let e = &self.edit;
        let mut parts = describe_settings(e);
        if e["hsl"].as_object().is_some_and(|h| h.values().any(|c| c.as_object().is_some_and(|c| c.values().any(nonzero)))) {
            parts.push("Color mix".into());
        }
        if let Some(r) = e["rotation"].as_f64().filter(|r| r.abs() >= 0.05) {
            parts.push(format!("Straighten {r:+.1}°"));
        }
        if e["crop"].as_object().is_some_and(|c| c.values().any(|v| v.as_f64().unwrap_or(0.0) >= 0.005)) {
            parts.push("Crop".into());
        }
        for m in e["masks"].as_array().into_iter().flatten() {
            parts.push(format!("Mask “{}”", m["name"].as_str().unwrap_or("?")));
        }
        if parts.is_empty() { "No changes".into() } else { parts.join("   ") }
    }
}

fn nonzero(v: &Value) -> bool {
    v.as_f64().is_some_and(|x| x.abs() >= 0.05)
}

fn describe_settings(e: &Value) -> Vec<String> {
    let mut parts = vec![];
    for (key, label, ..) in SETTINGS {
        match e[*key].as_f64() {
            Some(v) if *key == "exposure" && v.abs() >= 0.05 => parts.push(format!("{label} {v:+.1}")),
            Some(v) if *key != "exposure" && v.round() != 0.0 => parts.push(format!("{label} {v:+.0}")),
            _ => {}
        }
    }
    parts
}

/// The file the edit starts from: the raw if the shot has one, else the camera JPEG.
pub fn source(p: &crate::library::Photo) -> PathBuf {
    p.raw().or(p.camera_jpeg()).unwrap_or(&p.primary).to_path_buf()
}

/// Where a saved fix goes: `<stem>-fixed.jpg` next to the original.
pub fn fixed_path(p: &crate::library::Photo) -> PathBuf {
    if let Some(e) = &p.edit {
        return e.clone();
    }
    let stem = p.primary.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    p.primary.with_file_name(format!("{stem}{}.jpg", crate::library::FIXED_SUFFIX))
}

fn cache() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("Flick")
}

/// Working files for one fix: the unedited render Claude looks at (`original.jpg`), its masks,
/// and the renders of its edits. Removed with the fix.
pub struct Session {
    dir: PathBuf,
    src: PathBuf,
}

/// What the MCP server needs to know about a session, in `info.json`.
#[derive(Serialize, Deserialize)]
struct Info {
    /// The file RapidRAW reads.
    input: PathBuf,
    /// Size of RapidRAW's unedited render. Masks are made at this size.
    width: u32,
    height: u32,
}

impl Session {
    pub fn new(src: &Path) -> Result<Self, String> {
        static N: AtomicU32 = AtomicU32::new(0);
        let root = cache();
        // Left behind by a crash.
        for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a.as_secs() > 86_400);
            if old && e.file_name().to_string_lossy().starts_with("fix-") {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
        let dir = root.join(format!("fix-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(dir.join("masks")).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(dir.join("renders")).map_err(|e| e.to_string())?;
        Ok(Self { dir, src: src.to_path_buf() })
    }

    /// Renders the photo with no edits for Claude to look at, once per fix.
    fn prepare(&self) -> Result<Info, String> {
        if let Ok(info) = read_info(&self.dir) {
            return Ok(info);
        }
        let ext = self.src.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        // RapidRAW doesn't read HEIC.
        let input = if ext == "heic" || ext == "heif" {
            let tiff = self.dir.join("source.tiff");
            let o = Command::new("/usr/bin/sips").args(["-s", "format", "tiff"]).arg(&self.src).arg("--out").arg(&tiff).output();
            if !o.is_ok_and(|o| o.status.success()) || !tiff.is_file() {
                return Err("could not convert the HEIC for RapidRAW".into());
            }
            tiff
        } else {
            self.src.clone()
        };
        let full = self.dir.join("original-full.jpg");
        rapidraw(&input, &json!({ "baselineExposure": true }), &full)?;
        let img = image::open(&full).map_err(|e| e.to_string())?;
        let info = Info { input, width: img.width(), height: img.height() };
        let _ = std::fs::remove_file(&full);
        save_jpeg(&img.thumbnail(ANALYZE_EDGE, ANALYZE_EDGE), &self.dir.join("original.jpg"))?;
        std::fs::write(self.dir.join("info.json"), serde_json::to_vec(&info).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        Ok(info)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn read_info(dir: &Path) -> Result<Info, String> {
    let s = std::fs::read(dir.join("info.json")).map_err(|e| e.to_string())?;
    serde_json::from_slice(&s).map_err(|e| e.to_string())
}

/// Asks Claude to edit the photo. It sees the unedited render, and on a follow-up also the
/// latest result. `step` hears what it is doing; `cancel` stops it.
pub fn analyze(s: &Session, feedback: &str, history: &[Round], step: &dyn Fn(String), cancel: &AtomicBool) -> Result<Params, String> {
    let claude = claude().ok_or("Claude Code is not installed (no `claude` on PATH or in ~/.local/bin).")?;
    rapidraw_app()?;
    step("Preparing the photo…".into());
    let info = s.prepare()?;
    step("Claude is looking at the photo…".into());
    let exe = std::fs::canonicalize(std::env::current_exe().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let mcp = json!({ "mcpServers": { "flick": { "type": "stdio", "command": exe, "args": ["--mcp", s.dir] } } });
    std::fs::write(s.dir.join("mcp.json"), mcp.to_string()).map_err(|e| e.to_string())?;
    let log = s.dir.join("log");
    let _ = std::fs::remove_file(&log);

    let raw = crate::library::is_raw(&s.src);
    let stats = clip_stats(&s.dir.join("original.jpg"))?;
    let source = if raw {
        "The edit applies to the raw file, which keeps detail above white: negative highlights, whites or \
exposure can bring back areas that look blown in original.jpg. White balance and noise reduction work well. \
A raw file with no edits often looks darker and flatter than the camera's own JPEG, which the photographer has \
been looking at: first bring it to a normal exposure and contrast, and don't describe that as a fault of the photo."
    } else {
        "The edit applies to a JPEG: areas clipped to white cannot be recovered, only toned down."
    };
    let mut rounds = String::new();
    for (n, r) in history.iter().enumerate() {
        let asked = if r.feedback.is_empty() { "(no request, your own judgment)" } else { &r.feedback };
        rounds += &format!("Round {}. The photographer asked: {asked}\nYour edit: {}\n", n + 1, r.params.edit);
    }
    let mut request = if history.is_empty() {
        String::new()
    } else {
        format!("Earlier rounds:\n{rounds}\nlatest.jpg shows the result of your last edit.\n\n")
    };
    request += &match (feedback.trim(), history.is_empty()) {
        ("", true) => "The photographer gave no instructions. Make it look balanced and natural. Typical problems: \
a bright background or sky behind a well-exposed subject, a subject too dark against a bright background, crushed \
shadows, a color cast, a crooked horizon. A good photo should get few or no changes."
            .to_string(),
        ("", false) => "The photographer wants another try. Improve on your last edit.".to_string(),
        (f, true) => format!("The photographer says: \"{f}\"\nDo what they ask first. Fix other clear problems only if they don't conflict."),
        (f, false) => format!("The photographer now says about the latest result: \"{f}\"\nKeep what they didn't complain about."),
    };
    let masks: Vec<String> = std::fs::read_dir(s.dir.join("masks"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".png").map(str::to_string))
        .collect();
    if !masks.is_empty() {
        request += &format!("\nMasks made earlier, still usable: {}.", masks.join(", "));
    }
    let prompt = format!(
        "You are a photo editor, editing a photo in RapidRAW, a raw editor. Read original.jpg with the Read tool: \
the photo as it comes from the camera file, with no edits. {source}\n\n{request}\n\n\
Measured on original.jpg: {stats}\n\n\
How to work:\n\
1. Look at the photo and decide what it needs.\n\
2. For changes to one part (a person, a face, the sky, the background), make a mask with the segment tool and \
check that the red overlay it returns covers the right area and nothing else. Gradients and ellipses need no tool.\n\
3. Call render with your edit and look at the result. Compare it with the request and with original.jpg. If \
something is off (too strong, a color cast, a visible mask edge, new clipping), change the edit and render again. \
You have {MAX_RENDERS} renders; stop once it looks right.\n\
4. Answer with the final edit, exactly as you would pass it to render, and a summary.\n\n\
{EDIT_DOC}\n\n\
Good edits are restrained. Keep skin tones natural and keep the mood (a night scene stays dark). Several small \
adjustments beat one large one, and large global exposure or contrast changes usually ruin a photo: use masks \
for local problems. If they ask for something these tools can't do, such as removing an object, do what you can \
and say so. In summary, say in one or two plain sentences what you changed and why."
    );
    let out = std::fs::File::create(s.dir.join("answer.json")).map_err(|e| e.to_string())?;
    let err = std::fs::File::create(s.dir.join("claude.err")).map_err(|e| e.to_string())?;
    let tools = "Read mcp__flick__segment mcp__flick__render";
    let mut child = Command::new(claude)
        .current_dir(&s.dir)
        .args(["-p", &prompt, "--output-format", "json", "--json-schema", ANSWER])
        // Only Flick's tools and Read: no other MCP servers, user hooks or skills, and it can't touch files.
        .args(["--tools", "Read", "--allowedTools", tools, "--mcp-config", "mcp.json", "--strict-mcp-config"])
        .args(["--disable-slash-commands", "--setting-sources", "project", "--no-session-persistence"])
        // Segment Anything is downloaded on first use.
        .env("MCP_TIMEOUT", "60000")
        .env("MCP_TOOL_TIMEOUT", "900000")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .map_err(|e| format!("could not run claude: {e}"))?;
    let mut shown = String::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Cancelled".into());
        }
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
        }
        let last = std::fs::read_to_string(&log).ok().and_then(|l| l.lines().last().map(str::to_string));
        if let Some(l) = last.filter(|l| *l != shown) {
            step(l.clone());
            shown = l;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let v: Value = serde_json::from_slice(&std::fs::read(s.dir.join("answer.json")).unwrap_or_default()).map_err(|_| {
        let err = std::fs::read_to_string(s.dir.join("claude.err")).unwrap_or_default();
        format!("claude failed: {}", err.lines().last().unwrap_or("no output"))
    })?;
    if v["is_error"].as_bool() == Some(true) {
        return Err(format!("claude: {}", v["result"].as_str().unwrap_or("error")));
    }
    let params: Params = serde_json::from_value(v["structured_output"].clone()).map_err(|e| format!("claude returned no edit: {e}"))?;
    to_rapidraw(&params.edit, &s.dir, &info)?;
    Ok(params)
}

/// Renders the edit and decodes it at `PREVIEW_EDGE` for display. Also keeps a smaller copy
/// that Claude sees as the latest result on a follow-up.
pub fn preview(s: &Session, p: &Params) -> Result<image::RgbaImage, String> {
    let info = s.prepare()?;
    let full = render(&s.dir, &info, &p.edit)?;
    let img = image::open(&full).map_err(|e| e.to_string())?.thumbnail(PREVIEW_EDGE, PREVIEW_EDGE);
    save_jpeg(&img.thumbnail(ANALYZE_EDGE, ANALYZE_EDGE), &s.dir.join("latest.jpg"))?;
    Ok(img.to_rgba8())
}

/// Writes the full-size render of the edit to `dest`, atomically.
pub fn save(s: &Session, dest: &Path, p: &Params) -> Result<u64, String> {
    let info = s.prepare()?;
    let full = render(&s.dir, &info, &p.edit)?;
    let tmp = dest.with_extension("jpg.tmp");
    std::fs::copy(&full, &tmp).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })?;
    std::fs::metadata(dest).map(|m| m.len()).map_err(|e| e.to_string())
}

/// Full-size render of an edit, cached in the session by its RapidRAW settings.
fn render(dir: &Path, info: &Info, edit: &Value) -> Result<PathBuf, String> {
    use std::hash::{Hash, Hasher};
    let adj = to_rapidraw(edit, dir, info)?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    adj.to_string().hash(&mut h);
    let out = dir.join("renders").join(format!("{:016x}.jpg", h.finish()));
    if !out.is_file() {
        rapidraw(&info.input, &adj, &out)?;
    }
    Ok(out)
}

fn rapidraw_app() -> Result<PathBuf, String> {
    let home = dirs::home_dir().unwrap_or_default();
    [PathBuf::from("/Applications"), home.join("Applications")]
        .into_iter()
        .map(|d| d.join("RapidRAW.app/Contents/MacOS/RapidRAW"))
        .find(|p| p.is_file())
        .ok_or_else(|| "Fix needs RapidRAW, a free raw editor. Install it in Applications from github.com/CyberTimon/RapidRAW/releases.".into())
}

/// Runs RapidRAW's command line export. It always renders at full size.
fn rapidraw(input: &Path, adjustments: &Value, out: &Path) -> Result<(), String> {
    let app = rapidraw_app()?;
    let settings = out.with_extension("json");
    std::fs::write(&settings, adjustments.to_string()).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(out);
    let o = Command::new(app)
        .arg("export")
        .arg(input)
        .arg("--output")
        .arg(out)
        .args(["--format", "jpeg", "--quality", "92", "--keep-metadata", "--adjustments"])
        .arg(&settings)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run RapidRAW: {e}"));
    let _ = std::fs::remove_file(&settings);
    let o = o?;
    if out.is_file() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout);
    Err(format!("RapidRAW failed: {}", text.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("no output")))
}

/// Turns Claude's edit into RapidRAW's settings. Values out of range are clamped; anything
/// else wrong is an error Claude can read and fix.
fn to_rapidraw(edit: &Value, dir: &Path, info: &Info) -> Result<Value, String> {
    let e = edit.as_object().ok_or("the edit must be a JSON object")?;
    let (w, h) = (info.width as f64, info.height as f64);
    let mut out = Map::new();
    out.insert("baselineExposure".into(), true.into());
    let rotation = e.get("rotation").map(|r| r.as_f64().ok_or("rotation must be a number")).transpose()?.unwrap_or(0.0).clamp(-15.0, 15.0);
    for (k, v) in e {
        match k.as_str() {
            "rotation" | "crop" | "masks" => {}
            "hsl" => {
                let mut hsl = Map::new();
                for c in COLORS {
                    let get = |f: &str| v[c][f].as_f64().unwrap_or(0.0).clamp(-100.0, 100.0);
                    hsl.insert(c.into(), json!({ "hue": get("hue"), "saturation": get("saturation"), "luminance": get("luminance") }));
                }
                if let Some(bad) = v.as_object().ok_or("hsl must be an object")?.keys().find(|k| !COLORS.contains(&k.as_str())) {
                    return Err(format!("hsl has no color range \"{bad}\""));
                }
                out.insert("hsl".into(), hsl.into());
            }
            _ => {
                out.insert(k.clone(), setting(k, v)?);
            }
        }
    }
    if rotation != 0.0 {
        out.insert("rotation".into(), rotation.into());
    }
    // The largest frame of the same shape that the turned photo still fills, then the crop inside it.
    let crop = e.get("crop");
    if rotation != 0.0 || crop.is_some() {
        let side = |s: &str| crop.and_then(|c| c[s].as_f64()).unwrap_or(0.0).clamp(0.0, 0.45);
        let t = rotation.abs().to_radians();
        let k = (w / (w * t.cos() + h * t.sin())).min(h / (w * t.sin() + h * t.cos()));
        let (cw, ch) = (w * k, h * k);
        let (x0, y0) = ((w - cw) / 2.0, (h - ch) / 2.0);
        out.insert(
            "crop".into(),
            json!({
                "x": (x0 + side("left") * cw).round(),
                "y": (y0 + side("top") * ch).round(),
                "width": (cw * (1.0 - side("left") - side("right"))).round(),
                "height": (ch * (1.0 - side("top") - side("bottom"))).round(),
            }),
        );
    }
    let point = |v: &Value, what: &str| -> Result<(f64, f64), String> {
        match v.as_array().map(|a| a.iter().map(Value::as_f64).collect::<Vec<_>>()).as_deref() {
            Some([Some(x), Some(y)]) => Ok((x * w, y * h)),
            _ => Err(format!("{what} must be [x, y], fractions of the photo")),
        }
    };
    let mut masks = vec![];
    for (i, m) in e.get("masks").map(|m| m.as_array().ok_or("masks must be a list")).transpose()?.into_iter().flatten().enumerate() {
        let name = m["name"].as_str().unwrap_or("mask");
        let mut areas = vec![];
        for (j, a) in m["areas"].as_array().ok_or_else(|| format!("mask \"{name}\" needs areas"))?.iter().enumerate() {
            let (kind, params) = if let Some(n) = a["mask"].as_str() {
                let png = std::fs::read(dir.join("masks").join(format!("{}.png", mask_name(n))))
                    .map_err(|_| format!("there is no mask named \"{n}\": make it with segment first"))?;
                let data = format!("data:image/png;base64,{}", base64(&png));
                // RapidRAW turns the mask with the photo.
                ("ai-subject", json!({ "startX": 0, "startY": 0, "endX": 0, "endY": 0, "rotation": rotation, "maskDataBase64": data }))
            } else if let Some(g) = a.get("gradient") {
                // RapidRAW's gradient is full strength on one side of a line through start, fading
                // over `range`; its direction is from start to end turned a quarter.
                let ((fx, fy), (tx, ty)) = (point(&g["from"], "gradient from")?, point(&g["to"], "gradient to")?);
                let (cx, cy, dx, dy) = ((fx + tx) / 2.0, (fy + ty) / 2.0, tx - fx, ty - fy);
                ("linear", json!({ "startX": cx, "startY": cy, "endX": cx + dy, "endY": cy - dx, "range": dx.hypot(dy) / 2.0 }))
            } else if let Some(el) = a.get("ellipse") {
                let ((cx, cy), (rx, ry)) = (point(&el["center"], "ellipse center")?, point(&el["radius"], "ellipse radius")?);
                let feather = el["feather"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0);
                ("radial", json!({ "centerX": cx, "centerY": cy, "radiusX": rx, "radiusY": ry, "rotation": 0, "feather": feather }))
            } else {
                return Err(format!("mask \"{name}\": each area needs mask, gradient or ellipse"));
            };
            let mode = if a["subtract"].as_bool() == Some(true) { "subtractive" } else { "additive" };
            areas.push(json!({ "id": format!("a{i}-{j}"), "type": kind, "visible": true, "invert": false, "opacity": 100, "mode": mode, "parameters": params }));
        }
        let mut adj = Map::new();
        for (k, v) in m["adjustments"].as_object().ok_or_else(|| format!("mask \"{name}\" needs adjustments"))? {
            adj.insert(k.clone(), setting(k, v)?);
        }
        let invert = m["invert"].as_bool().unwrap_or(false);
        masks.push(json!({ "id": format!("m{i}"), "name": name, "visible": true, "invert": invert, "opacity": 100, "adjustments": adj, "subMasks": areas }));
    }
    if !masks.is_empty() {
        out.insert("masks".into(), masks.into());
    }
    Ok(out.into())
}

fn setting(key: &str, v: &Value) -> Result<Value, String> {
    let (_, _, lo, hi) = SETTINGS.iter().find(|s| s.0 == key).ok_or_else(|| format!("there is no setting \"{key}\""))?;
    Ok(v.as_f64().ok_or_else(|| format!("{key} must be a number"))?.clamp(*lo, *hi).into())
}

/// A mask name that is safe as a file name.
fn mask_name(n: &str) -> String {
    n.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(40).collect()
}

fn save_jpeg(img: &image::DynamicImage, out: &Path) -> Result<(), String> {
    img.to_rgb8().save_with_format(out, image::ImageFormat::Jpeg).map_err(|e| e.to_string())
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            s.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    s
}

// ---------- the MCP server Claude uses: `flick --mcp <session dir>` ----------

/// Serves `segment` and `render` over stdio, one JSON-RPC message per line, until Claude exits.
pub fn serve(dir: &Path) {
    let renders = AtomicU32::new(0);
    let mut stdout = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        // Notifications get no answer.
        let Some(id) = req.get("id").cloned() else { continue };
        let result = match req["method"].as_str().unwrap_or("") {
            "initialize" => json!({
                "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "flick", "version": env!("CARGO_PKG_VERSION") },
            }),
            "tools/list" => json!({ "tools": tools() }),
            "tools/call" => {
                let args = &req["params"]["arguments"];
                let res = match req["params"]["name"].as_str().unwrap_or("") {
                    "segment" => segment(dir, args),
                    "render" => render_tool(dir, args, &renders),
                    n => Err(format!("no tool {n}")),
                };
                match res {
                    Ok(content) => json!({ "content": content }),
                    Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
                }
            }
            "ping" => json!({}),
            m => {
                let err = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("no method {m}") } });
                let _ = writeln!(stdout, "{err}");
                let _ = stdout.flush();
                continue;
            }
        };
        let _ = writeln!(stdout, "{}", json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        let _ = stdout.flush();
    }
}

fn tools() -> Value {
    json!([
        {
            "name": "segment",
            "description": "Makes a mask of part of the photo, saved under `name` for use in an edit as {mask: name}. \
Returns the photo with the mask tinted red, and how much of the frame it covers. Check the overlay: if it misses \
part of what you wanted or spills onto other things, call again with a better box or points (same name replaces it). \
kind \"object\" uses Segment Anything and needs a box around the thing and/or points on it (label 1) and on nearby \
things to leave out (label 0); best for a specific person, face, object, the sky or water. \"subject\" finds the main \
subjects, \"people\" all people; a box limits either to that area. Positions are fractions of the photo, x from the \
left, y from the top.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Short name, letters, digits, - and _, such as baby or sky." },
                    "kind": { "type": "string", "enum": ["object", "subject", "people"] },
                    "box": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4, "description": "[x0, y0, x1, y1]" },
                    "points": { "type": "array", "items": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 }, "description": "[[x, y, 1 or 0]]" },
                },
                "required": ["name", "kind"],
            },
        },
        {
            "name": "render",
            "description": format!("Renders the photo with an edit, as the photographer would get it, and returns it with \
how much is clipped. Look at it carefully and refine the edit if needed. {MAX_RENDERS} renders per request.\n\n{EDIT_DOC}"),
            "inputSchema": { "type": "object", "properties": { "edit": { "type": "object" } }, "required": ["edit"] },
        },
    ])
}

/// What the tools are doing, for the progress line in Flick.
fn log(dir: &Path, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log")) {
        let _ = writeln!(f, "{line}");
    }
}

fn image_content(path: &Path) -> Result<Value, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok(json!({ "type": "image", "data": base64(&data), "mimeType": "image/jpeg" }))
}

fn segment(dir: &Path, args: &Value) -> Result<Vec<Value>, String> {
    let info = read_info(dir)?;
    let name = mask_name(args["name"].as_str().unwrap_or(""));
    if name.is_empty() {
        return Err("give the mask a name: letters, digits, - and _".into());
    }
    let kind = args["kind"].as_str().unwrap_or("object");
    let helper = helper("flick-segment").ok_or("flick-segment is missing. Build with ./build.sh.")?;
    log(dir, &format!("Claude is masking “{name}”…"));
    let models = if kind == "object" { models(dir)? } else { cache().join("models").join(MODEL) };
    let prompt = json!({ "kind": kind, "box": args["box"], "points": args["points"] });
    let (mask, view) = (dir.join("masks").join(format!("{name}.png")), dir.join("masks").join(format!("{name}-view.jpg")));
    let o = Command::new(helper)
        .arg(dir.join("original.jpg"))
        .arg(&mask)
        .arg(&view)
        .arg(info.width.to_string())
        .arg(info.height.to_string())
        .arg(models)
        .arg(prompt.to_string())
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        let _ = std::fs::remove_file(&mask);
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    let coverage: f64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0.0);
    let text = format!("Mask \"{name}\" covers {:.1}% of the frame. Use it as {{\"mask\": \"{name}\"}}.", coverage * 100.0);
    Ok(vec![image_content(&view)?, json!({ "type": "text", "text": text })])
}

/// Segment Anything for the `object` kind: about 460 MB, downloaded on first use.
fn models(dir: &Path) -> Result<PathBuf, String> {
    let root = cache().join("models");
    let done = root.join(MODEL);
    if done.is_dir() {
        return Ok(done);
    }
    log(dir, "Downloading the segmentation model (460 MB, once)…");
    let tmp = root.join(format!("{MODEL}.part"));
    let _ = std::fs::remove_dir_all(&tmp);
    for part in MODEL_PARTS {
        let pkg = format!("SAM2_1Large{part}FLOAT16.mlpackage");
        for file in ["Manifest.json", "Data/com.apple.CoreML/model.mlmodel", "Data/com.apple.CoreML/weights/weight.bin"] {
            let ok = Command::new("/usr/bin/curl")
                .args(["-fsSL", "--retry", "2", "--create-dirs", "-o"])
                .arg(tmp.join(&pkg).join(file))
                .arg(format!("{MODEL_URL}/{pkg}/{file}"))
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err("could not download the segmentation model; use subject or people masks, gradients or ellipses".into());
            }
        }
    }
    std::fs::rename(&tmp, &done).map_err(|e| e.to_string())?;
    Ok(done)
}

fn render_tool(dir: &Path, args: &Value, renders: &AtomicU32) -> Result<Vec<Value>, String> {
    let n = renders.fetch_add(1, Ordering::Relaxed) + 1;
    if n > MAX_RENDERS {
        return Err("No renders left. Give your final answer.".into());
    }
    let info = read_info(dir)?;
    log(dir, &format!("Claude is checking its edit ({n} of {MAX_RENDERS})…"));
    let full = render(dir, &info, &args["edit"])?;
    let small = dir.join(format!("try-{n}.jpg"));
    save_jpeg(&image::open(&full).map_err(|e| e.to_string())?.thumbnail(ANALYZE_EDGE, ANALYZE_EDGE), &small)?;
    let text = format!("Render {n} of {MAX_RENDERS}. {}", clip_stats(&small)?);
    Ok(vec![image_content(&small)?, json!({ "type": "text", "text": text })])
}

/// Share of clipped and crushed pixels and mean brightness, so Claude doesn't have to guess
/// whether a bright area is blown.
fn clip_stats(image: &Path) -> Result<String, String> {
    let img = image::open(image).map_err(|e| e.to_string())?.to_rgb8();
    let n = (img.width() * img.height()).max(1) as f64;
    let (mut clipped, mut crushed, mut sum) = (0u32, 0u32, 0f64);
    for px in img.pixels() {
        let [r, g, b] = px.0;
        if r.max(g).max(b) >= 250 {
            clipped += 1;
        }
        if r.max(g).max(b) <= 5 {
            crushed += 1;
        }
        sum += 0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64;
    }
    Ok(format!(
        "{:.1}% of pixels clipped to white, {:.1}% crushed to black, mean brightness {:.0}/255.",
        100.0 * clipped as f64 / n,
        100.0 * crushed as f64 / n,
        sum / n
    ))
}

fn exe_dir() -> Option<PathBuf> {
    std::fs::canonicalize(std::env::current_exe().ok()?).ok()?.parent().map(Path::to_path_buf)
}

/// Inside the app bundle next to `flick`, or in `target/` during `cargo run`.
fn helper(name: &str) -> Option<PathBuf> {
    let dir = exe_dir()?;
    [dir.join(name), dir.join("..").join(name)].into_iter().find(|p| p.is_file())
}

pub enum Heard {
    /// The whole transcript so far.
    Text(String),
    Failed(String),
}

/// Dictation in progress. `stop` lets the last words arrive before `rx` disconnects;
/// dropping it stops the microphone too.
pub struct Listener {
    child: Option<Child>,
    pub rx: Receiver<Heard>,
}

impl Listener {
    pub fn start(wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        let helper = helper("flick-listen").ok_or("flick-listen is missing. Build with ./build.sh.")?;
        let mut child = Command::new(helper)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let (stdout, mut stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(Heard::Text(line));
                wake();
            }
            let mut err = String::new();
            let _ = std::io::Read::read_to_string(&mut stderr, &mut err);
            if !err.trim().is_empty() {
                let _ = tx.send(Heard::Failed(err.trim().to_string()));
                wake();
            }
        });
        Ok(Self { child: Some(child), rx })
    }

    /// Closing stdin asks the helper to finish; it prints the final transcript and exits.
    pub fn stop(&mut self) {
        if let Some(c) = &mut self.child {
            drop(c.stdin.take());
        }
    }

    pub fn stopping(&self) -> bool {
        self.child.as_ref().is_none_or(|c| c.stdin.is_none())
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop();
        if let Some(mut c) = self.child.take() {
            std::thread::spawn(move || c.wait());
        }
    }
}

/// Apps opened from Finder don't get the shell's PATH, so check the usual install spots too.
fn claude() -> Option<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("claude")).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .chain([
            home.join(".local/bin/claude"),
            home.join(".claude/local/claude"),
            PathBuf::from("/opt/homebrew/bin/claude"),
            PathBuf::from("/usr/local/bin/claude"),
        ])
        .find(|p| p.is_file())
}

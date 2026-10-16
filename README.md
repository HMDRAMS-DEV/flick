<h1 align="center">Flick</h1>

<p align="center">An extremely fast photo culler for macOS, written in Rust.</p>

<p align="center"><img src="docs/screenshot.png" alt="Flick showing a landscape photo with a filmstrip of keep and delete marks below it"></p>

Open a folder or camera card, then fly through it with the keyboard. ↑ keeps a photo, ↓ marks it for deletion, and each key moves straight to the next one. When you're done, one screen shows everything you marked, and Enter deletes it for good.

<p align="center"><img src="docs/speed.svg" alt="Bar chart: Flick shows a photo in 30 ms, a full JPEG decode takes 390 ms, and a prefetched photo shows on the next frame" width="720"></p>

## Highlights

- **Next photo on the next frame.** The next 8 photos are decoded on background threads before you reach them. At 15 photos a second, 117 of 117 photos were on screen the frame after the keypress.
- **Reads 8% of each file, not 100%.** Camera JPEGs carry a smaller preview inside them. Flick reads only that preview, about 0.7 MB of an 8 MB file. It loads full resolution once you stop on a photo, so zoom is ready when you want it.
- **One key per decision.** ↑ keeps, ↓ marks for deletion, and both move on. ⌫ undoes. Tab jumps to the next photo you haven't decided on.
- **1:1 zoom for focus checks.** Z shows true pixels. Move the pointer to pan.
- **RAW + JPEG are one photo.** `BF_0001.JPG`, `BF_0001.DNG`, and `BF_0001.xmp` are one shot. Deleting removes all of them. To keep only half of a pair, press J to keep the JPEG or R to keep the RAW. The other half goes at the same review step as deletions.
- **Fix with Claude.** Press E, type what is wrong with the photo, or press V and say it. Claude Code edits it in [RapidRAW](https://github.com/CyberTimon/RapidRAW): exposure, tone, white balance, color by hue, detail, noise reduction, straightening and crop, and masks that limit a change to a person, the sky, or any other part of the photo. It renders its edit and checks the result before answering. Flick shows the result with Claude's reasoning. Press E or V again to ask for changes, hold B to compare with the original, Enter saves it as `BF_0001-fixed.jpg`, and Esc discards it. When the shot has a RAW or DNG, the fix renders from it, so white balance and noise reduction happen in the raw conversion and highlights clipped in the JPEG can come back. The originals are never changed, and the fixed file is deleted with the shot.
- **Nothing is lost if you quit.** Marks are saved in `~/Library/Application Support/Flick` as you make them, never on the card. Reopen the folder and you're back at the first undecided photo.
- **Small and native.** One 12 MB universal binary for Apple Silicon and Intel, drawn on the GPU. There's no runtime, no Electron, and no catalog to import into.

## What Flick leaves out

Flick does one job: deciding what to keep. It has no library, no import step, no ratings or color labels, no manual editing, and no export. Finder, Photos, and your editor already do those well. Keep the photos Flick leaves behind, then open them in whatever you use next.

## Install

Download `Flick.dmg` from the [latest release](https://github.com/HMDRAMS-DEV/flick/releases/latest) and drag Flick to Applications. It's signed and notarized, and runs on macOS 13 or later.

To open folders from a terminal, link the binary onto your `PATH`:

```sh
sudo ln -sf /Applications/Flick.app/Contents/MacOS/flick /usr/local/bin/flick
flick ~/Pictures/2026-10-10
```

You can also drop any folder on the window, or press O. A mounted camera card shows up on the start screen, and Enter opens it.

## Keys

| Key | Does |
|---|---|
| → or Space | Next photo |
| ← or ⇧Space | Previous photo |
| ↑ or K | Keep, then next |
| ↓ or X | Mark for deletion, then next |
| J / R | RAW + JPEG pair: keep only the JPEG / only the RAW, then next |
| E | Fix with Claude: type a request, Enter sends it. Empty means "fix what looks wrong" |
| V | Fix with Claude, starting with the mic on. ⌘D turns the mic on or off while typing |
| | On a fix: Enter or ↑ saves, E or V asks for changes, Esc or ↓ discards, hold B for the original |
| U | Clear the decision |
| ⌫ or ⌘Z | Undo the last decision |
| Z or click | Zoom to 100%. Move the pointer to pan |
| Tab, ⇧Tab | Next or previous undecided photo |
| Enter | Review marked photos, then delete them |
| F | Full screen |
| O | Open another folder |
| H or ? | All keys |

## Fix with Claude

E needs [Claude Code](https://claude.com/claude-code) installed and logged in. Flick looks for `claude` on your `PATH`, then in `~/.local/bin`, `~/.claude/local`, `/opt/homebrew/bin`, and `/usr/local/bin`.

It also needs [RapidRAW](https://github.com/CyberTimon/RapidRAW/releases), a free raw editor, in `/Applications` or `~/Applications`. Flick runs its command line export to render every edit. RapidRAW is AGPL-licensed and installed separately; Flick doesn't include it. Its Dock icon shows briefly during each render. RapidRAW doesn't read HEIC, so Flick converts HEIC photos to TIFF first.

Flick renders the photo with no edits at 1568 px and runs `claude -p` with the Read tool and two tools of its own, served by `flick --mcp`: no other MCP servers, user settings, or saved session. `segment` makes a mask: Segment Anything 2.1 (Meta's model, Apple's Core ML build) for a specific object from a box or points, or Apple Vision for the main subject or all people. Claude sees each mask as a red overlay and can redo it. `render` shows Claude its edit as you would get it, up to 5 times per request, so it can fix what looks wrong. Claude then answers with the final edit. A fix takes 30 to 60 seconds and costs about $0.15 on API billing. The 1568 px renders and mask overlays are sent to Anthropic; the original file is not. Follow-up requests also send the latest result and the earlier rounds.

The first object mask downloads Segment Anything (about 460 MB, from Hugging Face) to `~/Library/Caches/Flick/models`, which takes about 900 MB once compiled for your Mac. Masking runs on your Mac. The "main subject" mask needs macOS 14 or later.

Dictation runs in the bundled `flick-listen` helper with Apple's speech recognizer, on-device when the language model is installed. The first time you press V or ⌘D, macOS asks for Microphone and Speech Recognition access for Flick. If you decline, turn them on in System Settings > Privacy & Security.

## Deleting is permanent

Deleting skips the Trash. The files are removed from the folder or card, so the space frees up right away. You never delete by accident from the cull screen: ↓ only marks a photo. Enter opens a review of every marked photo and every dropped RAW or JPEG half, with the file count and total size. Click a photo there to keep it instead, and press Enter again to delete.

## How it's fast

- **Embedded previews.** Most cameras store a smaller JPEG inside each photo, in its MPF block or EXIF thumbnail. Flick finds it by parsing the file header and reads only those bytes. It skips HDR gain maps, which share the MPF block but aren't meant to be viewed. RAW files use a JPEG preview embedded in their TIFF structure.
- **A queue that follows you.** Every keypress replaces the decode queue in priority order: this photo, then the photos ahead in the direction you're moving, then visible thumbnails. Stale work never delays the photo on screen.
- **Nothing blocks the UI thread.** Disk reads, decoding, and color conversion happen on worker threads. The UI thread only uploads finished textures and draws.
- **The card is the limit.** Over a ~36 MB/s camera USB link, holding an arrow key (about 30 photos a second) outruns the card. Flick shows each photo's thumbnail while you fly, then sharpens it when you stop.

## Build

Requires Rust 1.88 or later, and Xcode's command line tools for the Swift helpers.

```sh
cargo run --release -- ~/Pictures/some-folder
./build.sh             # target/Flick.app, universal; also builds the target/flick-* helpers for cargo run
./build.sh --install   # also copies it to /Applications and links ~/.cargo/bin/flick
cargo run --release -- --bench /path/to/folder   # time scan and decode without a window
```

`scripts/release.sh` signs, notarizes, and publishes a release.

## License

MIT

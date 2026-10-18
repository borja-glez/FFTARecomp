#![cfg_attr(windows, windows_subsystem = "windows")]

use eframe::egui::{self, Align2, Color32, FontId, RichText, Rounding, Sense, Stroke, Vec2};
mod rom_policy;
mod platform;
mod path_picker;
mod native_picker;
#[path = "../../gba-core/src/ffta_rom.rs"]
mod ffta_rom;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

// ---- Customize here ---------------------------------------------------
const APP_NAME: &str = "FFTARecomp";
const AUTHORS: &str = "Jorge Rios";
const ACCENT: Color32 = Color32::from_rgb(0x7C, 0x5C, 0xFF);
const ACCENT_HOVER: Color32 = Color32::from_rgb(0x8E, 0x72, 0xFF);
const BG: Color32 = Color32::from_rgb(0x0E, 0x0E, 0x11);
const PANEL: Color32 = Color32::from_rgb(0x17, 0x17, 0x1C);
const TEXT: Color32 = Color32::from_rgb(0xE8, 0xE8, 0xEC);
const TEXT_DIM: Color32 = Color32::from_rgb(0x8B, 0x8B, 0x96);
const OK: Color32 = Color32::from_rgb(0x4C, 0xD1, 0x7E);
const ERR: Color32 = Color32::from_rgb(0xE5, 0x5B, 0x5B);

const KNOWN_SHA1: &[&str] = &[
    "4ac05441f4de70a4ec3dd932116346c61b8783d9", // USA
    "9efaf328cbbcbc830be14940e42e4b92d90dfb58", // Europe (En,Fr,De,Es,It)
];

const ROM_FILE: &str = "ffta.gba";
const PLAYER_EXE: &str = platform::PLAYER;

const LOGO: &[u8] = include_bytes!("../assets/logo.png");
// -----------------------------------------------------------------------

enum Screen { Intro, Rom, Done }

enum Msg {
    Progress(f32, String),
    /// Phase with unknown duration: bar creeps from `base` towards `base + span`.
    Creep(f32, f32, String),
    Text(String),
    Warning(String),
    Finished,
    Failed(String),
}

struct App {
    screen: Screen,
    logo: Option<egui::TextureHandle>,
    rom: Option<PathBuf>,
    rom_status: Option<Result<String, String>>,
    warning: Option<String>,
    install_dir: PathBuf,
    path_picker: Option<path_picker::PathPicker>,
    native_picker: Option<(path_picker::Kind, Receiver<native_picker::Result>)>,
    shortcut: bool,
    rx: Option<Receiver<Msg>>,
    progress: f32,
    status: String,
    error: Option<String>,
    creeping: bool,
    base: f32,
    span: f32,
    since: Instant,
}

fn load_rgba(bytes: &[u8]) -> Option<(Vec<u8>, [usize; 2])> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some((img.into_raw(), size))
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let mut v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = PANEL;
        v.override_text_color = Some(TEXT);
        v.widgets.noninteractive.rounding = Rounding::same(8.0);
        v.widgets.inactive.rounding = Rounding::same(8.0);
        v.widgets.hovered.rounding = Rounding::same(8.0);
        v.widgets.active.rounding = Rounding::same(8.0);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x0B, 0x0B, 0x0E);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x1E, 0x1E, 0x27);
        v.widgets.active.weak_bg_fill = Color32::from_rgb(0x26, 0x26, 0x31);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x2A, 0x2A, 0x33));
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
        cc.egui_ctx.set_visuals(v);
        cc.egui_ctx.style_mut(|s| s.spacing.button_padding = Vec2::new(12.0, 5.0));

        let logo = load_rgba(LOGO).map(|(px, size)| {
            cc.egui_ctx.load_texture(
                "logo",
                egui::ColorImage::from_rgba_unmultiplied(size, &px),
                egui::TextureOptions::LINEAR,
            )
        });
        let install_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(APP_NAME);
        Self {
            screen: Screen::Intro, logo, rom: None, rom_status: None, warning: None, install_dir,
            path_picker: None,
            native_picker: None,
            shortcut: true, rx: None, progress: 0.0, status: String::new(), error: None,
            creeping: false, base: 0.0, span: 0.0, since: Instant::now(),
        }
    }

    fn set_rom(&mut self, path: PathBuf) {
        self.rom_status = Some(validate_rom(&path));
        self.rom = Some(path);
    }

    fn choose_path(&mut self, kind: path_picker::Kind, ctx: &egui::Context) {
        let (tx, rx) = channel();
        self.native_picker = Some((kind, rx));
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(native_picker::pick(kind));
            ctx.request_repaint();
        });
    }

    fn rom_ok(&self) -> bool { matches!(self.rom_status, Some(Ok(_))) }

    fn start_install(&mut self, ctx: &egui::Context) {
        let Some(rom) = self.rom.clone() else { return };
        let dir = self.install_dir.clone();
        let shortcut = self.shortcut;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.progress = 0.0;
        self.error = None;
        self.warning = None;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let send = |m: Msg| { let _ = tx.send(m); ctx.request_repaint(); };
            send(Msg::Progress(0.02, "Step 1/5 · Reading ROM...".into()));
            let data = match std::fs::read(&rom) {
                Ok(d) => d,
                Err(e) => return send(Msg::Failed(format!("Could not read the ROM: {e}"))),
            };
            // Eligibility uses these exact bytes, before mkdir/copy/compile or patch work.
            let plan = match rom_policy::plan(&data) {
                Ok(plan) => plan,
                Err(message) => return send(Msg::Failed(message)),
            };
            if let rom_policy::InstallPlan::Unpatched(message) = &plan {
                send(Msg::Warning(message.clone()));
            }
            send(Msg::Progress(0.05, "Step 1/5 · Creating folder...".into()));
            if let Err(e) = std::fs::create_dir_all(&dir) {
                return send(Msg::Failed(format!("Could not create the folder: {e}")));
            }
            send(Msg::Progress(0.08, "Step 2/5 · Copying ROM...".into()));
            if let Err(e) = std::fs::write(dir.join(ROM_FILE), &data) {
                return send(Msg::Failed(format!("Could not save the ROM: {e}")));
            }
            send(Msg::Progress(0.12, "Step 3/5 · Copying game files...".into()));
            let Some(payload) = find_payload() else {
                return send(Msg::Failed(
                    "The 'payload' folder was not found next to the installer.".into(),
                ));
            };
            if let Err(e) = copy_dir(&payload, &dir) {
                return send(Msg::Failed(format!("Could not copy the game files: {e}")));
            }
            if !dir.join(PLAYER_EXE).exists() {
                return send(Msg::Failed(format!("{PLAYER_EXE} is missing from 'payload'.")));
            }
            for name in [PLAYER_EXE,platform::RECOMP] {
                if let Err(error)=platform::executable(&dir.join(name)) {
                    return send(Msg::Failed(format!("Could not prepare {name}: {error}")));
                }
            }

            // ---- Recompile step -------------------------------------------------
            send(Msg::Creep(0.20, 0.06, "Step 4/5 · Recompiling the game (this may take several minutes)...".into()));
            let mut cmd = Command::new(dir.join(platform::RECOMP));
            cmd.current_dir(&dir)
                .arg("play")
                .arg("--status")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if dir.join("gamedb.sqlite").is_file() {cmd.arg("--gamedb").arg(dir.join("gamedb.sqlite"));}
            cmd.arg(dir.join(ROM_FILE));
            let tool_bin = dir.join("toolchain").join("bin");
            let old = std::env::var_os("PATH").unwrap_or_default();
            let mut paths = vec![tool_bin];
            paths.extend(std::env::split_paths(&old));
            if let Ok(p) = std::env::join_paths(paths) { cmd.env("PATH", p); }
            cmd.env("RECOMP_JOBS", "2");
            #[cfg(any(target_os = "linux", windows))]
            cmd.env("FFTA_FRAME_PACING", std::env::var_os("FFTA_FRAME_PACING").unwrap_or_else(|| "clock".into()));
            if matches!(plan, rom_policy::InstallPlan::Unpatched(_)) {
                cmd.env("FFTA_PATCH_BG", "0").env("FFTA_PATCH_SPR", "0");
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000);
            }
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => return send(Msg::Failed(format!("Could not start the recompiler: {e}"))),
            };
            let (ltx, lrx) = channel::<String>();
            if let Some(out) = child.stdout.take() {
                let t = ltx.clone();
                std::thread::spawn(move || {
                    for l in BufReader::new(out).lines().map_while(Result::ok) { let _ = t.send(l); }
                });
            }
            if let Some(e) = child.stderr.take() {
                let t = ltx.clone();
                std::thread::spawn(move || {
                    for l in BufReader::new(e).lines().map_while(Result::ok) { let _ = t.send(l); }
                });
            }
            drop(ltx);
            let mut ready = false;
            let mut tail: Vec<String> = Vec::new();
            for line in lrx {
                tail.push(line.clone());
                if tail.len() > 8 { tail.remove(0); }
                let playing = line.to_lowercase().contains("playing");
                match parse_frac(&line) {
                    Some((k, m)) => {
                        let span = 0.75 / m as f32;
                        let base = 0.20 + span * k.saturating_sub(1) as f32;
                        send(Msg::Creep(base, span, format!("Step 4/5 · Recompiling ({k}/{m}) · {line}")));
                    }
                    None => send(Msg::Text(format!("Step 4/5 · {line}"))),
                }
                if playing {
                    send(Msg::Progress(0.95, "Step 4/5 · Build complete".into()));
                    // build finished and cache is filled: stop the game window
                    ready = true;
                    let _ = child.kill();
                    break;
                }
            }
            let exited_ok = child.wait().map(|s| s.success()).unwrap_or(false);
            if !(ready || exited_ok) {
                return send(Msg::Failed(format!("Recompilation failed.\n{}", tail.join("\n"))));
            }

            // ---- Desktop shortcut -----------------------------------------------
            send(Msg::Progress(0.97, "Step 5/5 · Finishing...".into()));
            if shortcut {
                create_shortcut(&dir);
            }
            send(Msg::Progress(1.0, "Done".into()));
            send(Msg::Finished);
        });
    }

    fn poll(&mut self) {
        let mut finished = false;
        if let Some(rx) = &self.rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Progress(p, s) => { self.creeping = false; self.progress = p; self.status = s; }
                    Msg::Creep(b, sp, s) => {
                        self.creeping = true; self.base = b; self.span = sp;
                        self.since = Instant::now(); self.status = s;
                    }
                    Msg::Text(s) => self.status = s,
                    Msg::Warning(s) => self.warning = Some(s),
                    Msg::Finished => finished = true,
                    Msg::Failed(e) => { self.error = Some(e); self.rx = None; break; }
                }
            }
        }
        if finished { self.rx = None; self.screen = Screen::Done; }
    }
}

/// Finds "N/M" in a status line (e.g. "Building 1/4").
fn parse_frac(s: &str) -> Option<(u32, u32)> {
    let b = s.as_bytes();
    for i in 1..b.len().saturating_sub(1) {
        if b[i] == b'/' && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit() {
            let mut a = i;
            while a > 0 && b[a - 1].is_ascii_digit() { a -= 1; }
            let mut e = i + 1;
            while e < b.len() && b[e].is_ascii_digit() { e += 1; }
            let n: u32 = s[a..i].parse().ok()?;
            let m: u32 = s[i + 1..e].parse().ok()?;
            if m > 0 && m <= 20 && n <= m { return Some((n, m)); }
        }
    }
    None
}

#[cfg(windows)]
fn create_shortcut(dir: &Path) {
    let Some(desktop) = dirs::desktop_dir() else { return };
    let exe = dir.join(PLAYER_EXE);
    let lnk = desktop.join(format!("{APP_NAME}.lnk"));
    let q = |p: &Path| p.display().to_string().replace('\'', "''");
    let script = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');\
         $s.TargetPath='{}';$s.WorkingDirectory='{}';$s.IconLocation='{},0';$s.Save()",
        q(&lnk), q(&exe), q(dir), q(&exe)
    );
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.status();
}

#[cfg(target_os="linux")]
fn create_shortcut(dir:&Path) {
    let Ok(entry)=platform::desktop_entry(dir) else{return;};
    if let Some(data)=dirs::data_local_dir() {
        let applications=data.join("applications");
        if std::fs::create_dir_all(&applications).is_ok() {let _=std::fs::write(applications.join("FFTARecomp.desktop"),&entry);}
    }
    if let Some(desktop)=dirs::desktop_dir() {
        let path=desktop.join("FFTARecomp.desktop");
        if std::fs::write(&path,entry).is_ok(){let _=platform::executable(&path);}
    }
}

#[cfg(not(any(windows,target_os="linux")))]
fn create_shortcut(_:&Path) {}

fn find_payload() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok()?.parent()?.join("payload");
    if beside.is_dir() { return Some(beside); }
    let cwd = PathBuf::from("payload");
    if cwd.is_dir() { Some(cwd) } else { None }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn validate_rom(path: &Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("Could not read the file: {e}"))?;
    match rom_policy::plan(&data)? {
        rom_policy::InstallPlan::Verified(region) => Ok(format!("{region} ROM verified (SHA1, SHA256 and all hook bytes)")),
        rom_policy::InstallPlan::Unpatched(message) => Ok(message),
    }
}

/// Button with text perfectly centered (independent of the parent layout alignment).
fn accent_button(ui: &mut egui::Ui, text: &str, enabled: bool, width: f32) -> egui::Response {
    let size = Vec2::new(width, 40.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let fill = if !enabled {
        Color32::from_rgb(0x2A, 0x2A, 0x33)
    } else if resp.hovered() {
        ACCENT_HOVER
    } else {
        ACCENT
    };
    let text_color = if enabled { Color32::WHITE } else { TEXT_DIM };
    ui.painter().rect_filled(rect, Rounding::same(8.0), fill);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(15.0), text_color);
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// Paragraph with mixed emphasis: `true` parts are drawn in white, the rest dimmed.
fn rich(ui: &mut egui::Ui, parts: &[(&str, bool)]) {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    for (t, hi) in parts {
        job.append(
            t,
            0.0,
            egui::TextFormat {
                font_id: FontId::proportional(14.0),
                color: if *hi { Color32::WHITE } else { TEXT_DIM },
                ..Default::default()
            },
        );
    }
    ui.label(job);
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(PANEL)
        .rounding(Rounding::same(10.0))
        .inner_margin(16.0)
        .show(ui, |ui| { ui.set_width(ui.available_width()); add(ui) });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        if let Some((kind, rx)) = &self.native_picker {
            if let Ok(result) = rx.try_recv() {
                let kind = *kind;
                self.native_picker = None;
                match result {
                    native_picker::Result::Selected(path) => match kind {
                        path_picker::Kind::Rom => self.set_rom(path),
                        path_picker::Kind::Folder => self.install_dir = path.join(APP_NAME),
                    },
                    native_picker::Result::Cancelled => {},
                    native_picker::Result::Unavailable(message) => {
                        eprintln!("Native file selector unavailable: {message}");
                        self.path_picker = Some(path_picker::PathPicker::new(kind));
                    }
                }
            }
        }

        if matches!(self.screen, Screen::Rom) && self.rx.is_none() {
            let dropped = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()));
            if let Some(p) = dropped { self.set_rom(p); }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(32.0))
            .show(ctx, |ui| match self.screen {
                Screen::Intro => self.ui_intro(ui),
                Screen::Rom => self.ui_rom(ui, ctx),
                Screen::Done => self.ui_done(ui),
            });
        if let Some(picker) = &mut self.path_picker {
            if let Some(result) = picker.show(ctx) {
                self.path_picker = None;
                match result {
                    path_picker::Selection::Rom(path) => self.set_rom(path),
                    path_picker::Selection::Folder(path) => self.install_dir = path.join(APP_NAME),
                    path_picker::Selection::Cancel => {}
                }
            }
        }
    }
}

impl App {
    fn ui_intro(&mut self, ui: &mut egui::Ui) {
        let logo_size = (ui.available_height() - 160.0).clamp(96.0, 176.0);
        ui.vertical_centered(|ui| {
            ui.add_space(16.0);
            if let Some(t) = &self.logo {
                ui.image((t.id(), Vec2::splat(logo_size)));
            }
            ui.add_space(12.0);
            ui.label(RichText::new(APP_NAME).size(32.0).strong());
            ui.label(RichText::new(format!("A project by {AUTHORS}")).color(TEXT_DIM));
            ui.label(RichText::new("v2.0").color(TEXT_DIM));
        });
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
            if accent_button(ui, "Next", true, 150.0).clicked() { self.screen = Screen::Rom; }
        });
    }

    fn ui_rom(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let installing = self.rx.is_some();
        let scroll_height = (ui.available_height() - 56.0).max(1.0);
        egui::ScrollArea::vertical()
            .id_salt("rom_setup")
            .max_height(scroll_height)
            .auto_shrink([false, false])
            .stick_to_bottom(installing)
            .show(ui, |ui| {
        ui.label(RichText::new("Important").size(22.0).strong());
        ui.add_space(8.0);
        card(ui, |ui| {
            rich(ui, &[(
                "FFTARecomp is a native port of Final Fantasy Tactics Advance for modern PCs. \
                 Instead of running the game inside a traditional emulator, it rebuilds it as a \
                 program of its own, which makes room for widescreen support, adjustable video, \
                 audio and controls, and other quality-of-life improvements.\n",
                false,
            )]);
            rich(ui, &[
                ("A ROM is required. ", true),
                ("Provide your own copy of ", false),
                ("Final Fantasy Tactics Advance", true),
                (", the ", false),
                ("United States (USA) or European (Europe) version", true),
                (". For licensing reasons no game data is included or distributed, and your ROM \
                  never leaves your PC.\n", false),
            ]);
            rich(ui, &[
                ("The game is built on your computer from your ROM during installation, which can \
                  take several minutes. It is normal for the installed size to be noticeably larger \
                  than the game's ROM alone: the installer includes the complete compiler toolchain \
                  needed to build it, so this is not just a game but everything required to build it. \
                  You don't need to install anything else.\n", false),
            ]);
            rich(ui, &[(
                "This is an unfinished project. In our tests it ran well and was playable, but \
                 you may still run into bugs; if you do, please report them. While the game is \
                 being built it is normal for your PC to slow down and for CPU and RAM usage to \
                 rise, especially on low-end hardware. This only happens once.",
                false,
            )]);
        });
        ui.add_space(12.0);
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Install to").color(TEXT_DIM));
                ui.label(self.install_dir.display().to_string());
                if ui.add_enabled(!installing && self.native_picker.is_none(), egui::Button::new("Change")).clicked() {
                    self.choose_path(path_picker::Kind::Folder, ctx);
                }
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                match &self.rom {
                    Some(p) => ui.label(p.file_name().unwrap_or_default().to_string_lossy().to_string()),
                    None => ui.label(RichText::new("Drag your ROM (.gba) here").color(TEXT_DIM)),
                };
                if ui.add_enabled(!installing && self.native_picker.is_none(), egui::Button::new("Browse")).clicked() {
                    self.choose_path(path_picker::Kind::Rom, ctx);
                }
            });
            match &self.rom_status {
                Some(Ok(m)) => { ui.label(RichText::new(m).color(if m.starts_with("WARNING") { ERR } else { OK })); }
                Some(Err(e)) => { ui.label(RichText::new(e).color(ERR)); }
                None => {}
            }
            if self.native_picker.is_some() { ui.label("Choose a file or folder in the desktop dialog..."); }
            ui.add_space(8.0);
            ui.add_enabled(!installing, egui::Checkbox::new(&mut self.shortcut, "Create desktop shortcut"));
        });

        if installing {
            ui.add_space(12.0);
            let shown = if self.creeping {
                let t = self.since.elapsed().as_secs_f32();
                self.base + self.span * 0.95 * (1.0 - (-t / 60.0).exp())
            } else {
                self.progress
            };
            ctx.request_repaint_after(Duration::from_millis(200));
            ui.add(egui::ProgressBar::new(shown).fill(ACCENT).rounding(Rounding::same(8.0)));
            ui.label(RichText::new(&self.status).color(TEXT_DIM));
        }
        if let Some(e) = &self.error {
            ui.add_space(8.0);
            ui.label(RichText::new(e).color(ERR));
        }
        if let Some(warning) = &self.warning { ui.label(RichText::new(warning).color(ERR)); }

            });

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
            if accent_button(ui, "Recompile & Install", self.rom_ok() && !installing && self.native_picker.is_none() && self.path_picker.is_none(), 210.0).clicked() {
                self.start_install(ctx);
            }
        });
    }

    fn ui_done(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("installation_complete")
            .auto_shrink([false, false])
            .show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(16.0);
            if let Some(t) = &self.logo { ui.image((t.id(), Vec2::splat(112.0))); }
            ui.add_space(10.0);
            ui.label(RichText::new("Installation complete").size(26.0).strong());
            ui.label(RichText::new("The game is recompiled and ready to play.").color(TEXT_DIM));
            if let Some(warning) = &self.warning { ui.label(RichText::new(warning).color(ERR)); }
            ui.add_space(20.0);
            if accent_button(ui, "Play", true, 150.0).clicked() {
                let mut command = Command::new(self.install_dir.join(PLAYER_EXE));
                command.current_dir(&self.install_dir);
                #[cfg(any(target_os = "linux", windows))]
                command.env("FFTA_FRAME_PACING", std::env::var_os("FFTA_FRAME_PACING").unwrap_or_else(|| "clock".into()));
                let _ = command.spawn();
                std::process::exit(0);
            }
            ui.add_space(8.0);
            if ui.button("Close").clicked() { std::process::exit(0); }
        });
            });
    }
}

fn main() -> eframe::Result<()> {
    let mut vb = egui::ViewportBuilder::default()
        .with_inner_size([720.0, 640.0])
        .with_min_inner_size([400.0, 320.0])
        .with_resizable(true)
        .with_title(format!("{APP_NAME} - Installer"));
    if let Some((rgba, [w, h])) = load_rgba(LOGO) {
        vb = vb.with_icon(egui::IconData { rgba, width: w as u32, height: h as u32 });
    }
    eframe::run_native(
        &format!("{APP_NAME} Installer"),
        eframe::NativeOptions { viewport: vb, ..Default::default() },
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

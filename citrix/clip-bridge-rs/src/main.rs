//! Offer Wayland image/png captures to X11 as CF_DIB for Citrix wfica.
//!
//! Rust port of the Python citrix-clip-bridge. Same wire behavior:
//! _ISL_DIB with pixels at 0x428, one-shot ChangeProperty (wfica corrupts
//! INCR reassembly), no PRIMARY ownership, CLIPBOARD only while wfica runs.
//!
//! Differences from the Python daemon:
//! - no GTK/PyGObject; ICCCM selection handling is done directly over x11rb
//! - _NET_ACTIVE_WINDOW is read natively instead of shelling out to xprop
//! - the wl-paste --watch child re-execs /proc/self/exe instead of argv[0]
//! - INCR receiving is implemented for the Citrix -> Wayland pull
//! - PIXMAP and TIMESTAMP targets are not advertised (wfica picks _ISL_DIB)

mod dib;

use std::collections::hash_map::DefaultHasher;
use std::ffi::CString;
use std::fs;
use std::hash::Hasher;
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Property,
    SelectionClearEvent, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
    SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::{CURRENT_TIME, NONE};

const WFICA_BIN: &[u8] = b"/opt/Citrix/ICAClient/wfica";

fn rundir() -> String {
    std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())
}
fn lock_path() -> String {
    format!("{}/citrix-clip-bridge-rs.lock", rundir())
}
fn pid_path() -> String {
    format!("{}/citrix-clip-bridge-rs.pid", rundir())
}
fn png_path() -> String {
    format!("{}/citrix-clip-bridge-rs.png", rundir())
}
fn notify_path() -> String {
    format!("{}/citrix-clip-bridge-rs.notify", rundir())
}

fn log_init() {
    let ident = CString::new("citrix-clip-bridge-rs").unwrap();
    // openlog keeps the ident pointer; leak it deliberately.
    unsafe { libc::openlog(ident.as_ptr(), libc::LOG_PID, libc::LOG_USER) };
    std::mem::forget(ident);
}

fn log(msg: &str) {
    if let Ok(c) = CString::new(msg) {
        unsafe { libc::syslog(libc::LOG_INFO | libc::LOG_USER, c"%s".as_ptr(), c.as_ptr()) };
    }
    eprintln!("{msg}");
}

fn hash_bytes(data: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    h.write(data);
    h.finish()
}

fn bytes_contain(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// True while at least one ICA session process exists.
fn wfica_running_scan() -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(cmd) = fs::read(format!("/proc/{name}/cmdline")) else {
            continue;
        };
        if bytes_contain(&cmd, b"citrix-clip-bridge") {
            continue;
        }
        if bytes_contain(&cmd, WFICA_BIN) {
            return true;
        }
    }
    false
}

#[derive(Clone, Copy)]
struct Atoms {
    clipboard: Atom,
    dib: Atom,
    isl_dib: Atom,
    cf_dib: Atom,
    bmp: Atom,
    image_bmp: Atom,
    png: Atom,
    image_png: Atom,
    rgbquad: Atom,
    targets: Atom,
    wm_class: Atom,
    net_active_window: Atom,
    pull_prop: Atom,
    incr: Atom,
}

fn intern_atoms(conn: &RustConnection) -> Result<Atoms, Box<dyn std::error::Error>> {
    let get = |name: &[u8]| -> Result<Atom, Box<dyn std::error::Error>> {
        Ok(conn.intern_atom(false, name)?.reply()?.atom)
    };
    Ok(Atoms {
        clipboard: get(b"CLIPBOARD")?,
        dib: get(b"DIB")?,
        isl_dib: get(b"_ISL_DIB")?,
        cf_dib: get(b"CF_DIB")?,
        bmp: get(b"BMP")?,
        image_bmp: get(b"image/bmp")?,
        png: get(b"PNG")?,
        image_png: get(b"image/png")?,
        rgbquad: get(b"_ISL_RGBQUAD")?,
        targets: get(b"TARGETS")?,
        wm_class: get(b"WM_CLASS")?,
        net_active_window: get(b"_NET_ACTIVE_WINDOW")?,
        pull_prop: get(b"CITRIX_CLIP_BRIDGE_PULL")?,
        incr: get(b"INCR")?,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PullStage {
    Isl,
    Dib,
}

struct Incr {
    prop: Atom,
    buf: Vec<u8>,
}

struct Pull {
    owner: Window,
    stage: PullStage,
    deadline: Instant,
    incr: Option<Incr>,
}

#[derive(Clone, Copy)]
enum TimerKind {
    Reclaim,
    Poke(&'static str),
    PullRequest,
}

struct Timer {
    at: Instant,
    seq: u64,
    kind: TimerKind,
}

struct Bridge {
    conn: RustConnection,
    holder: Window,
    root: Window,
    atoms: Atoms,
    max_prop: usize,

    dib: Vec<u8>,
    png: Vec<u8>,
    img_w: u32,
    img_h: u32,
    last_hash: u64,

    session_active: bool,
    session_pull_done: bool,
    pending_serve: bool,
    import_pending: bool,
    reassert_left: u32,
    ingest_seq: u64,
    pulled_owner_xid: Window,
    wfica_pull_tries: u32,
    last_ingest_at: Option<Instant>,
    last_export_at: Option<Instant>,
    pull_cool_until: Option<Instant>,
    last_clip_owner: Window,
    wfica_focused: bool,
    wfica_cache: Option<(Instant, bool)>,
    focus_cache: Option<(Instant, bool)>,

    pull: Option<Pull>,
    timers: Vec<Timer>,
}

impl Bridge {
    // ---- X11 helpers -------------------------------------------------

    fn selection_owner(&self, sel: Atom) -> Window {
        self.conn
            .get_selection_owner(sel)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.owner)
            .unwrap_or(NONE)
    }

    fn we_own(&self, sel: Atom) -> bool {
        self.selection_owner(sel) == self.holder
    }

    fn class_is_wfica(&self, xid: Window) -> bool {
        let Some(reply) = self
            .conn
            .get_property(false, xid, self.atoms.wm_class, AtomEnum::STRING, 0, 64)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return false;
        };
        let text = String::from_utf8_lossy(&reply.value);
        text.contains("Wfica") || text.contains("wfica")
    }

    /// wfica's CLIPBOARD owner is an unmapped child without WM_CLASS.
    fn window_is_wfica(&self, xid: Window) -> bool {
        let mut cur = xid;
        for _ in 0..10 {
            if cur == NONE {
                return false;
            }
            if self.class_is_wfica(cur) {
                return true;
            }
            let parent = self
                .conn
                .query_tree(cur)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.parent)
                .unwrap_or(NONE);
            if parent == cur {
                return false;
            }
            cur = parent;
        }
        false
    }

    fn owner_is_wfica(&self, sel: Atom) -> bool {
        let xid = self.selection_owner(sel);
        if xid == NONE || xid == self.holder {
            return false;
        }
        self.window_is_wfica(xid)
    }

    fn wfica_running(&mut self) -> bool {
        if let Some((ts, cached)) = self.wfica_cache {
            if ts.elapsed() < Duration::from_millis(400) {
                return cached;
            }
        }
        let alive = wfica_running_scan();
        self.wfica_cache = Some((Instant::now(), alive));
        alive
    }

    fn active_window_is_wfica(&self) -> bool {
        let Some(reply) = self
            .conn
            .get_property(
                false,
                self.root,
                self.atoms.net_active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return false;
        };
        let Some(xid) = reply
            .value
            .first_chunk::<4>()
            .map(|b| u32::from_le_bytes(*b))
        else {
            return false;
        };
        xid != 0 && self.window_is_wfica(xid)
    }

    fn wfica_is_focused(&mut self) -> bool {
        if let Some((ts, cached)) = self.focus_cache {
            if ts.elapsed() < Duration::from_millis(200) {
                return cached;
            }
        }
        let focused = self.active_window_is_wfica();
        self.focus_cache = Some((Instant::now(), focused));
        focused
    }

    // ---- ownership ---------------------------------------------------

    fn citrix_is_target(&self) -> bool {
        // Own X11 CLIPBOARD whenever a session is open. We advertise
        // image/png too, so Linux paste still works; wfica picks _ISL_DIB.
        self.session_active
    }

    /// Drop CLIPBOARD so the next claim is a real owner change. Never touch
    /// PRIMARY (Linux middle-click) and never clear a selection we do not
    /// own: SetSelectionOwner(None) as a third party can unown the host
    /// clipboard.
    fn release_selections(&self) {
        if self.selection_owner(self.atoms.clipboard) == self.holder {
            let _ = self
                .conn
                .set_selection_owner(NONE, self.atoms.clipboard, CURRENT_TIME);
            let _ = self.conn.flush();
        }
    }

    fn offer_selections(&self) {
        if !self.citrix_is_target() || self.dib.is_empty() {
            return;
        }
        let _ = self
            .conn
            .set_selection_owner(self.holder, self.atoms.clipboard, CURRENT_TIME);
        let _ = self.conn.flush();
    }

    fn drop_clipboard_ownership(&mut self) {
        self.reassert_left = 0;
        self.pending_serve = false;
        self.ingest_seq += 1;
        self.dib.clear();
        self.png.clear();
        self.img_w = 0;
        self.img_h = 0;
        self.release_selections();
    }

    fn claim_for_citrix(&mut self, reason: &str) {
        if !self.citrix_is_target() || self.dib.is_empty() {
            return;
        }
        self.pending_serve = true;
        self.ingest_seq += 1;
        let seq = self.ingest_seq;
        self.reassert_left = 6;
        self.release_selections();
        self.schedule(Duration::from_millis(40), seq, TimerKind::Reclaim);
        for (ms, why) in [(200, "200ms"), (500, "500ms"), (1200, "1200ms")] {
            self.schedule(Duration::from_millis(ms), seq, TimerKind::Poke(why));
        }
        log(&format!(
            "offered DIB {}x{} ({} bytes) ({reason})",
            self.img_w,
            self.img_h,
            self.dib.len()
        ));
    }

    fn reclaim_after_release(&mut self) {
        if !self.citrix_is_target() || self.dib.is_empty() {
            return;
        }
        self.offer_selections();
        log("reclaimed CLIPBOARD after release");
    }

    /// If Citrix still has not fetched _ISL_DIB, force a fresh owner claim.
    fn poke_c2h_offer(&mut self, seq: u64, reason: &str) {
        if !self.citrix_is_target()
            || seq != self.ingest_seq
            || !self.pending_serve
            || self.dib.is_empty()
        {
            return;
        }
        log(&format!("c2h poke ({reason})"));
        self.release_selections();
        self.reassert_left = self.reassert_left.max(4);
        self.schedule(Duration::from_millis(40), self.ingest_seq, TimerKind::Reclaim);
    }

    fn on_reassert(&mut self) {
        if !self.session_active || self.reassert_left == 0 || self.dib.is_empty() {
            return;
        }
        self.reassert_left -= 1;
        if self.we_own(self.atoms.clipboard) {
            return;
        }
        if self.owner_is_wfica(self.atoms.clipboard) && !self.pending_serve {
            // Do not steal CLIPBOARD back from Citrix after a successful serve.
            self.reassert_left = 0;
            return;
        }
        self.offer_selections();
    }

    // ---- session tracking ---------------------------------------------

    fn set_session_active(&mut self, active: bool, reason: &str) {
        if active == self.session_active {
            return;
        }
        self.session_active = active;
        if active {
            self.session_pull_done = false;
            log(&format!("Citrix session detected; bridging clipboard ({reason})"));
            self.schedule(
                Duration::from_millis(400),
                self.ingest_seq,
                TimerKind::PullRequest,
            );
            return;
        }
        self.session_pull_done = true;
        self.drop_clipboard_ownership();
        log(&format!("no Citrix session; left host clipboard alone ({reason})"));
    }

    fn poll_session(&mut self) {
        let alive = self.wfica_running();
        self.set_session_active(alive, "poll");
    }

    fn poll_focus(&mut self) {
        if !self.session_active {
            self.wfica_focused = false;
            return;
        }
        let focused = self.wfica_is_focused();
        if focused == self.wfica_focused {
            return;
        }
        self.wfica_focused = focused;
        log(if focused { "wfica focused" } else { "wfica unfocused" });
        if focused && !self.dib.is_empty() {
            self.claim_for_citrix("focus");
        }
    }

    // ---- serving (Wayland -> Citrix) ----------------------------------

    fn target_name(&self, atom: Atom) -> String {
        self.conn
            .get_atom_name(atom)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.name).into_owned())
            .unwrap_or_else(|| format!("atom-{atom}"))
    }

    /// Answers a selection request; returns true when the data was actually
    /// delivered, false when refused (oversize or ChangeProperty failure).
    fn answer(&self, ev: &SelectionRequestEvent, prop: Atom, type_: Atom, data: &[u8], format: u8) -> bool {
        if data.len() > self.max_prop {
            // Over BIG-REQUESTS (a 4K screenshot is ~33 MB). wfica reassembles
            // INCR chunks incorrectly, so refuse outright instead of letting
            // the peer fall back to a chunked transfer that pastes a split image.
            log(&format!(
                "refused {}: {} bytes over the X11 request limit ({})",
                self.target_name(ev.target),
                data.len(),
                self.max_prop
            ));
            self.refuse(ev);
            return false;
        }
        let units = (data.len() / (format as usize / 8)) as u32;
        if let Err(e) = self.conn.change_property(
            PropMode::REPLACE,
            ev.requestor,
            prop,
            type_,
            format,
            units,
            data,
        ) {
            log(&format!("ChangeProperty failed: {e}"));
            self.refuse(ev);
            return false;
        }
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: ev.time,
            requestor: ev.requestor,
            selection: ev.selection,
            target: ev.target,
            property: prop,
        };
        let _ = self
            .conn
            .send_event(false, ev.requestor, EventMask::NO_EVENT, notify);
        let _ = self.conn.flush();
        true
    }

    fn refuse(&self, ev: &SelectionRequestEvent) {
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: ev.time,
            requestor: ev.requestor,
            selection: ev.selection,
            target: ev.target,
            property: NONE,
        };
        let _ = self
            .conn
            .send_event(false, ev.requestor, EventMask::NO_EVENT, notify);
        let _ = self.conn.flush();
    }

    fn on_selection_request(&mut self, ev: SelectionRequestEvent) {
        let a = self.atoms;
        let t = ev.target;
        let prop = if ev.property == NONE { t } else { ev.property };
        if t != a.targets {
            log(&format!("selection-request {}", self.target_name(t)));
        }
        // Citrix ConvertSelection is in progress. Stop reclaiming CLIPBOARD
        // or we race the C2H render and paste into the session fails.
        if [
            a.isl_dib,
            a.dib,
            a.cf_dib,
            a.image_bmp,
            a.image_png,
            a.rgbquad,
            a.bmp,
            a.png,
        ]
        .contains(&t)
        {
            self.reassert_left = 0;
        }
        if t == a.targets {
            let list = [
                a.targets, a.isl_dib, a.dib, a.cf_dib, a.rgbquad, a.image_bmp, a.bmp,
                a.image_png, a.png,
            ];
            let mut data = Vec::with_capacity(list.len() * 4);
            for at in list {
                data.extend_from_slice(&at.to_le_bytes());
            }
            self.answer(&ev, prop, AtomEnum::ATOM.into(), &data, 32);
            return;
        }
        if t == a.image_png || t == a.png {
            if self.png.is_empty() {
                self.refuse(&ev);
            } else {
                self.answer(&ev, prop, t, &self.png, 8);
            }
            return;
        }
        if t == a.isl_dib {
            if self.dib.is_empty() {
                self.refuse(&ev);
            } else {
                let n = self.dib.len();
                if self.answer(&ev, prop, t, &self.dib, 8) {
                    self.pending_serve = false;
                    log(&format!("served _ISL_DIB {n} bytes"));
                }
            }
            return;
        }
        if t == a.dib || t == a.cf_dib {
            if self.dib.is_empty() {
                self.refuse(&ev);
            } else {
                let payload = dib::as_cf_dib(&self.dib);
                let n = payload.len();
                if self.answer(&ev, prop, t, &payload, 8) {
                    self.pending_serve = false;
                    log(&format!("served DIB {n} bytes"));
                }
            }
            return;
        }
        if t == a.rgbquad {
            self.answer(&ev, prop, t, &vec![0u8; dib::ISL_RGBQUAD], 8);
            return;
        }
        if t == a.image_bmp || t == a.bmp {
            if self.dib.is_empty() {
                self.refuse(&ev);
            } else {
                let payload = dib::wrap_bmp(&dib::as_cf_dib(&self.dib));
                self.answer(&ev, prop, t, &payload, 8);
            }
            return;
        }
        self.refuse(&ev);
    }

    fn on_selection_clear(&mut self, ev: SelectionClearEvent) {
        if !self.session_active || ev.selection != self.atoms.clipboard {
            return;
        }
        if self.owner_is_wfica(self.atoms.clipboard) {
            // Mid-_ISL_DIB transfer: do not reclaim.
            if !self.pending_serve {
                self.reassert_left = 0;
            }
            // After a Wayland ingest, Citrix often claims CLIPBOARD once it
            // has ConvertSelection'd _ISL_DIB. Pulling that ownership races
            // the C2H path and starts an empty-DIB storm.
            if self
                .last_ingest_at
                .is_some_and(|t| t.elapsed() < Duration::from_secs(20))
            {
                self.session_pull_done = true;
                log("selection-clear after Wayland ingest; skip pull");
                return;
            }
            self.session_pull_done = false;
            self.schedule(
                Duration::from_millis(300),
                self.ingest_seq,
                TimerKind::PullRequest,
            );
        }
    }

    // ---- pulling (Citrix -> Wayland) ----------------------------------

    fn start_pull(&mut self, owner: Window, stage: PullStage) {
        let target = match stage {
            PullStage::Isl => self.atoms.isl_dib,
            PullStage::Dib => self.atoms.dib,
        };
        self.import_pending = true;
        self.pull_cool_until = Some(Instant::now() + Duration::from_millis(800));
        log(&format!("pull image from wfica owner 0x{owner:x} ({stage:?})"));
        let _ = self.conn.convert_selection(
            self.holder,
            self.atoms.clipboard,
            target,
            self.atoms.pull_prop,
            CURRENT_TIME,
        );
        let _ = self.conn.flush();
        self.pull = Some(Pull {
            owner,
            stage,
            deadline: Instant::now() + Duration::from_secs(5),
            incr: None,
        });
    }

    fn request_wfica_image(&mut self) {
        let now = Instant::now();
        if !self.session_active || self.import_pending {
            return;
        }
        if self.session_pull_done {
            return;
        }
        if self.pull_cool_until.is_some_and(|t| now < t) {
            return;
        }
        if self
            .last_ingest_at
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(2))
        {
            return;
        }
        if self.reassert_left > 0 {
            return;
        }
        if self.we_own(self.atoms.clipboard) {
            return;
        }
        let xid = self.selection_owner(self.atoms.clipboard);
        if !self.window_is_wfica(xid) {
            return;
        }
        // Already decided this owner has no image; wait for a new owner.
        if xid != NONE && xid == self.pulled_owner_xid {
            return;
        }
        self.start_pull(xid, PullStage::Isl);
    }

    fn poll_wfica_export(&mut self) {
        if !self.session_active || self.session_pull_done || self.reassert_left > 0 {
            return;
        }
        if self.we_own(self.atoms.clipboard) {
            return;
        }
        self.request_wfica_image();
    }

    fn on_selection_notify(&mut self, ev: SelectionNotifyEvent) {
        if ev.requestor != self.holder || ev.selection != self.atoms.clipboard {
            return;
        }
        let Some(pull) = self.pull.take() else {
            return;
        };
        if pull.incr.is_some() {
            // Already streaming chunks; ignore stray notifies, keep the pull.
            self.pull = Some(pull);
            return;
        }
        if ev.property == NONE {
            self.pull_failed(pull);
            return;
        }
        let reply = match self
            .conn
            .get_property(
                false,
                self.holder,
                ev.property,
                AtomEnum::ANY,
                0,
                (self.max_prop / 4) as u32,
            )
            .ok()
            .and_then(|c| c.reply().ok())
        {
            Some(r) => r,
            None => {
                log("pull GetProperty failed");
                self.pull_failed(pull);
                return;
            }
        };
        if reply.type_ == self.atoms.incr {
            // Chunked transfer: value is the lower-bound size; deleting the
            // property tells the owner to start posting chunks.
            let expected = reply
                .value
                .first_chunk::<4>()
                .map(|b| u32::from_le_bytes(*b) as usize)
                .unwrap_or(0);
            let _ = self.conn.delete_property(self.holder, ev.property);
            let _ = self.conn.flush();
            log(&format!("pull via INCR, at least {expected} bytes"));
            self.pull = Some(Pull {
                incr: Some(Incr {
                    prop: ev.property,
                    buf: Vec::with_capacity(expected),
                }),
                ..pull
            });
            return;
        }
        let data = reply.value;
        let _ = self.conn.delete_property(self.holder, ev.property);
        let _ = self.conn.flush();
        log(&format!("pull {} {} bytes", self.target_name(ev.target), data.len()));
        match dib::isl_dib_to_rgb(&data) {
            Some(img) => self.pull_done(pull, Some(img)),
            None => self.pull_failed(pull),
        }
    }

    fn on_property_notify(&mut self, ev: x11rb::protocol::xproto::PropertyNotifyEvent) {
        if ev.window != self.holder || ev.state != Property::NEW_VALUE {
            return;
        }
        let streaming = self
            .pull
            .as_ref()
            .and_then(|p| p.incr.as_ref())
            .is_some_and(|i| i.prop == ev.atom);
        if !streaming {
            return;
        }
        let reply = match self
            .conn
            .get_property(
                true,
                self.holder,
                ev.atom,
                AtomEnum::ANY,
                0,
                (self.max_prop / 4) as u32,
            )
            .ok()
            .and_then(|c| c.reply().ok())
        {
            Some(r) => r,
            None => {
                log("INCR chunk GetProperty failed");
                self.pull = None;
                self.import_pending = false;
                return;
            }
        };
        let Some(pull) = &mut self.pull else { return };
        let Some(incr) = &mut pull.incr else { return };
        if !reply.value.is_empty() {
            incr.buf.extend_from_slice(&reply.value);
            return;
        }
        // Empty chunk: end of INCR.
        let data = std::mem::take(&mut incr.buf);
        let pull = self.pull.take().expect("pull checked above");
        log(&format!("pull INCR complete, {} bytes", data.len()));
        match dib::isl_dib_to_rgb(&data) {
            Some(img) => self.pull_done(pull, Some(img)),
            None => self.pull_failed(pull),
        }
    }

    fn pull_failed(&mut self, pull: Pull) {
        if pull.stage == PullStage::Isl {
            // wfica may only offer plain DIB.
            self.start_pull(pull.owner, PullStage::Dib);
            return;
        }
        self.pull_done(pull, None);
    }

    fn pull_done(&mut self, pull: Pull, img: Option<(Vec<u8>, u32, u32)>) {
        self.import_pending = false;
        self.pull = None;
        match img {
            Some((rgb, w, h)) => {
                self.pulled_owner_xid = pull.owner;
                self.wfica_pull_tries = 0;
                self.pull_cool_until = Some(Instant::now() + Duration::from_secs(1));
                log(&format!("pull {:?} {w}x{h}", pull.stage));
                self.publish_session_image(rgb, w, h);
            }
            None => {
                self.wfica_pull_tries += 1;
                let delay = if self.wfica_pull_tries >= 3 { 8 } else { 1 };
                self.pull_cool_until =
                    Some(Instant::now() + Duration::from_millis(delay * 1500));
                if self.wfica_pull_tries >= 3 {
                    self.pulled_owner_xid = pull.owner;
                    self.session_pull_done = true;
                    log("pull empty; stop until clipboard owner changes");
                } else {
                    log("pull empty; retry later");
                }
            }
        }
    }

    /// Citrix -> Wayland. Do not steal X11 from wfica.
    fn publish_session_image(&mut self, rgb: Vec<u8>, w: u32, h: u32) {
        let Some(png) = dib::png_encode(&rgb, w, h) else {
            return;
        };
        let hash = hash_bytes(&png);
        if hash == self.last_hash {
            self.session_pull_done = true;
            return;
        }
        self.last_hash = hash;
        self.dib = dib::rgb_to_isl_dib(&rgb, w, h);
        wl_copy_png(png.clone());
        self.png = png;
        self.img_w = w;
        self.img_h = h;
        self.reassert_left = 0;
        self.session_pull_done = true;
        self.last_export_at = Some(Instant::now());
        log(&format!("exported session image {w}x{h} to Wayland"));
    }

    // ---- Wayland ingest ------------------------------------------------

    fn ingest_png(&mut self) {
        let Ok(png) = fs::read(png_path()) else {
            return;
        };
        if png.is_empty() {
            return;
        }
        if !self.wfica_running() {
            self.set_session_active(false, "ingest");
            return;
        }
        self.set_session_active(true, "ingest");
        let hash = hash_bytes(&png);
        // Same-png retries while pending_serve raced KWin and stole CLIPBOARD
        // even with no Citrix session. c2h pokes already re-claim if needed.
        if hash == self.last_hash {
            return;
        }
        if self
            .last_export_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
        {
            return;
        }
        let Some((rgb, w, h)) = dib::png_decode(&png) else {
            log("png to DIB failed: decode error");
            return;
        };
        // A 4K screenshot is ~33 MB as a DIB, over the X11 request limit;
        // shrink it instead of refusing every serve.
        let (rgb, w, h) = match dib::downscale_to_fit(&rgb, w, h, self.max_prop) {
            (r, nw, nh) if (nw, nh) != (w, h) => {
                log(&format!(
                    "downscaled {w}x{h} -> {nw}x{nh} to fit the X11 request limit"
                ));
                (r, nw, nh)
            }
            same => same,
        };
        self.dib = dib::rgb_to_isl_dib(&rgb, w, h);
        self.png = png;
        self.img_w = w;
        self.img_h = h;
        self.last_hash = hash;
        self.last_ingest_at = Some(Instant::now());
        self.pull_cool_until = Some(Instant::now() + Duration::from_secs(2));
        self.pulled_owner_xid = 0;
        self.wfica_pull_tries = 0;
        self.session_pull_done = false;
        self.pending_serve = true;
        self.ingest_seq += 1;
        self.claim_for_citrix("ingest");
    }

    // ---- events / timers ------------------------------------------------

    fn on_xfixes_selection(&mut self, ev: xfixes::SelectionNotifyEvent) {
        if ev.selection != self.atoms.clipboard {
            return;
        }
        let xid = ev.owner;
        if xid == self.last_clip_owner {
            return;
        }
        let prev = self.last_clip_owner;
        self.last_clip_owner = xid;
        if xid == NONE || xid == self.holder {
            return;
        }
        if !self.session_active || !self.window_is_wfica(xid) {
            return;
        }
        if prev != xid {
            self.session_pull_done = false;
            self.pulled_owner_xid = 0;
            self.wfica_pull_tries = 0;
        }
        self.request_wfica_image();
    }

    fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::SelectionRequest(e) => self.on_selection_request(e),
            Event::SelectionClear(e) => self.on_selection_clear(e),
            Event::SelectionNotify(e) => self.on_selection_notify(e),
            Event::PropertyNotify(e) => self.on_property_notify(e),
            Event::XfixesSelectionNotify(e) => self.on_xfixes_selection(e),
            // Void requests (ChangeProperty, SetSelectionOwner, SendEvent)
            // report errors through the event stream; log instead of dying.
            Event::Error(e) => log(&format!(
                "X11 protocol error {} (seq {})",
                e.error_code, e.sequence
            )),
            _ => {}
        }
    }

    fn schedule(&mut self, delay: Duration, seq: u64, kind: TimerKind) {
        self.timers.push(Timer {
            at: Instant::now() + delay,
            seq,
            kind,
        });
    }

    fn run_timer(&mut self, t: Timer) {
        match t.kind {
            TimerKind::Reclaim => self.reclaim_after_release(),
            TimerKind::Poke(why) => self.poke_c2h_offer(t.seq, why),
            TimerKind::PullRequest => self.request_wfica_image(),
        }
    }
}

fn wl_copy_png(png: Vec<u8>) {
    let mut cmd = Command::new("wl-copy");
    cmd.arg("--type")
        .arg("image/png")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            log(&format!("wl-copy failed: {e}"));
            return;
        }
    };
    // wl-copy reads stdin, then forks to serve the selection in the
    // background. Write + reap on a thread so the event loop never blocks.
    std::thread::spawn(move || {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&png);
        }
        let _ = child.wait();
    });
}

static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: i32) {
    QUIT.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
    }
}

fn daemonize() {
    unsafe {
        if libc::fork() > 0 {
            libc::_exit(0);
        }
        libc::setsid();
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        if libc::fork() > 0 {
            libc::_exit(0);
        }
    }
    let _ = std::env::set_current_dir("/");
    unsafe {
        let fd = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if fd >= 0 {
            libc::dup2(fd, 0);
            libc::dup2(fd, 1);
            libc::dup2(fd, 2);
            if fd > 2 {
                libc::close(fd);
            }
        }
    }
}

fn acquire_lock() -> Option<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .open(lock_path())
        .ok()?;
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        None
    } else {
        Some(f)
    }
}

fn open_notify_fifo() -> std::io::Result<RawFd> {
    let path = CString::new(notify_path()).unwrap();
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        let mut exists = libc::lstat(path.as_ptr(), &mut st) == 0;
        if exists && st.st_mode & libc::S_IFMT != libc::S_IFIFO {
            libc::unlink(path.as_ptr());
            exists = false;
        }
        if !exists && libc::mkfifo(path.as_ptr(), 0o600) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd = libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK);
        if fd < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(fd)
        }
    }
}

fn notify_daemon() {
    let Ok(path) = CString::new(notify_path()) else {
        return;
    };
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK);
        if fd < 0 {
            log("ingest notify failed");
            return;
        }
        libc::write(fd, b"x".as_ptr().cast(), 1);
        libc::close(fd);
    }
}

fn ingest_mode() -> i32 {
    if !wfica_running_scan() {
        return 0;
    }
    let mut png = Vec::new();
    if std::io::stdin().read_to_end(&mut png).is_err() || png.is_empty() {
        return 0;
    }
    let write = (|| -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(png_path())?;
        f.write_all(&png)?;
        f.sync_all()?;
        Ok(())
    })();
    match write {
        Ok(()) => notify_daemon(),
        Err(e) => log(&format!("ingest failed: {e}")),
    }
    0
}

/// Drain every event x11rb already holds. This must run unconditionally, not
/// only when poll() reports the fd readable: blocking round trips
/// (get_property, query_tree, ...) read from the socket and can pull events
/// into x11rb's internal buffer, after which the raw fd no longer signals
/// POLLIN for them.
fn drain_events(br: &mut Bridge) -> Result<(), String> {
    loop {
        match br.conn.poll_for_event() {
            Ok(Some(ev)) => br.handle_event(ev),
            Ok(None) => return Ok(()),
            Err(e) => return Err(format!("{e}")),
        }
    }
}

fn run_loop(br: &mut Bridge, notify_fd: RawFd) -> i32 {
    let xfd = br.conn.stream().as_raw_fd();
    let mut next_reassert = Instant::now();
    let mut next_session = Instant::now();
    let mut next_export = Instant::now();
    let mut next_focus = Instant::now();
    loop {
        if QUIT.load(Ordering::Relaxed) {
            return 0;
        }
        // Drain before polling: timer handlers below do blocking round trips
        // that may have swallowed events into x11rb's buffer.
        if let Err(e) = drain_events(br) {
            log(&format!("X11 poll failed: {e}"));
            return 1;
        }
        let mut pfds = [
            libc::pollfd {
                fd: xfd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: notify_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, 100) };
        if n < 0 {
            continue; // EINTR from the quit signal, or transient
        }
        if pfds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            log("X11 connection dropped; exiting so the user unit restarts us");
            return 1;
        }
        if pfds[0].revents & libc::POLLIN != 0 {
            if let Err(e) = drain_events(br) {
                log(&format!("X11 poll failed: {e}"));
                return 1;
            }
        }
        if pfds[1].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 4096];
            unsafe { libc::read(notify_fd, buf.as_mut_ptr().cast(), buf.len()) };
            br.ingest_png();
        }
        let now = Instant::now();
        let mut i = 0;
        let mut due = Vec::new();
        while i < br.timers.len() {
            if br.timers[i].at <= now {
                due.push(br.timers.remove(i));
            } else {
                i += 1;
            }
        }
        for t in due {
            br.run_timer(t);
        }
        if now >= next_reassert {
            next_reassert = now + Duration::from_millis(250);
            br.on_reassert();
        }
        if now >= next_session {
            next_session = now + Duration::from_secs(1);
            br.poll_session();
        }
        if now >= next_export {
            next_export = now + Duration::from_millis(1500);
            br.poll_wfica_export();
        }
        if now >= next_focus {
            next_focus = now + Duration::from_millis(300);
            br.poll_focus();
        }
        if br.pull.as_ref().is_some_and(|p| now >= p.deadline) {
            log("pull timed out");
            let pull = br.pull.take().expect("checked above");
            br.import_pending = false;
            // Count as a failed try so the retry backoff (and eventual stop)
            // applies even when the owner never answers.
            br.pull_done(pull, None);
        }
    }
}

fn daemon_mode(foreground: bool) -> i32 {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() || std::env::var_os("DISPLAY").is_none() {
        // Pure X11 (or headless) session: nothing to bridge. Exit 0 so a
        // Restart=on-failure user unit does not loop on X11 desktops.
        return 0;
    }
    if !foreground {
        daemonize();
    }
    let _lock = match acquire_lock() {
        Some(f) => f,
        None => return 0, // another instance holds it
    };
    let (conn, screen_num) = match RustConnection::connect(None) {
        Ok(v) => v,
        Err(e) => {
            log(&format!("X11 connect failed: {e}"));
            return 0;
        }
    };
    // maximum_request_bytes() lazily enables BIG-REQUESTS.
    let max_prop = conn.maximum_request_bytes().saturating_sub(64);
    log(&format!("X11 BIG-REQUESTS max {} bytes", max_prop + 64));

    let atoms = match intern_atoms(&conn) {
        Ok(a) => a,
        Err(e) => {
            log(&format!("intern atoms failed: {e}"));
            return 1;
        }
    };
    let root = conn.setup().roots[screen_num].root;
    let holder = match conn.generate_id() {
        Ok(id) => id,
        Err(e) => {
            log(&format!("generate_id failed: {e}"));
            return 1;
        }
    };
    let window_created = match conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        holder,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        0, // CopyFromParent visual
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    ) {
        Ok(cookie) => cookie.check().map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = window_created {
        log(&format!("create_window failed: {e}"));
        return 1;
    }
    let mut xfixes_ok = false;
    if let Ok(cookie) = conn.xfixes_query_version(5, 0) {
        match cookie.reply() {
            Ok(_) => {
                let mask = xfixes::SelectionEventMask::SET_SELECTION_OWNER
                    | xfixes::SelectionEventMask::SELECTION_WINDOW_DESTROY
                    | xfixes::SelectionEventMask::SELECTION_CLIENT_CLOSE;
                match conn.xfixes_select_selection_input(holder, atoms.clipboard, mask) {
                    Ok(cookie) => match cookie.check() {
                        Ok(()) => xfixes_ok = true,
                        Err(e) => log(&format!("XFixes select failed: {e}")),
                    },
                    Err(e) => log(&format!("XFixes select failed: {e}")),
                }
            }
            Err(e) => log(&format!("XFixes query failed: {e}")),
        }
    }
    if !xfixes_ok {
        log("libXfixes missing; session copies may need a focus change");
    }
    let _ = conn.flush();

    let notify_fd = match open_notify_fifo() {
        Ok(fd) => fd,
        Err(e) => {
            log(&format!("notify fifo failed: {e}"));
            return 1;
        }
    };

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("clip-bridge-rs"));
    let mut watch: Option<Child> = match Command::new("wl-paste")
        .args(["--type", "image/png", "--watch"])
        .arg(&exe)
        .arg("--ingest")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(c) => Some(c),
        Err(e) => {
            log(&format!("wl-paste spawn failed: {e}"));
            None
        }
    };

    let _ = fs::write(pid_path(), std::process::id().to_string());
    install_signal_handlers();

    let mut br = Bridge {
        conn,
        holder,
        root,
        atoms,
        max_prop,
        dib: Vec::new(),
        png: Vec::new(),
        img_w: 0,
        img_h: 0,
        last_hash: 0,
        session_active: false,
        session_pull_done: false,
        pending_serve: false,
        import_pending: false,
        reassert_left: 0,
        ingest_seq: 0,
        pulled_owner_xid: 0,
        wfica_pull_tries: 0,
        last_ingest_at: None,
        last_export_at: None,
        pull_cool_until: None,
        last_clip_owner: 0,
        wfica_focused: false,
        wfica_cache: None,
        focus_cache: None,
        pull: None,
        timers: Vec::new(),
    };
    br.poll_session();
    br.poll_focus();
    log("watching Wayland image/png for X11 ISL_DIB (pixels at 0x428)");
    if !br.session_active {
        log("idle until a Citrix session starts");
    }
    let rc = run_loop(&mut br, notify_fd);

    if let Some(mut w) = watch.take() {
        let _ = w.kill();
        let _ = w.wait();
    }
    let _ = fs::remove_file(pid_path());
    rc
}

fn main() -> std::process::ExitCode {
    log_init();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().skip(1).any(|a| a == "--ingest" || a == "--convert") {
        return std::process::ExitCode::from(ingest_mode() as u8);
    }
    let foreground = args.iter().skip(1).any(|a| a == "--foreground");
    std::process::ExitCode::from(daemon_mode(foreground) as u8)
}

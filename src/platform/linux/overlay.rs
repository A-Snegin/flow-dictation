//! The floating status pill on Wayland: the only thing Flow puts on screen.
//!
//! The same pill as the Windows one, pixel for pixel. What it is made of, how
//! it animates and when it goes away all live in `crate::overlay_model`; this
//! file is the Wayland half: a layer-shell surface, a shared-memory buffer, and
//! a CPU rasteriser.
//!
//! No toolkit. The pill is a rounded rectangle, eleven bars, a keycap and one
//! line of text, repainted 25 times a second and only while the key is held.
//! `smithay-client-toolkit` gives the protocol plumbing, `tiny-skia` fills the
//! paths and `ab_glyph` rasterises the glyphs. Between dictations nothing runs.
//!
//! Four Wayland details this depends on:
//!
//! The surface is on the `Overlay` layer with `KeyboardInteractivity::None` and
//! an empty input region. That is what stops it taking focus and swallowing
//! clicks. If it ever took focus the caret would leave the user's document and
//! the dictated text would land in the wrong place, which is the same trap
//! `WS_EX_NOACTIVATE` avoids on Windows.
//!
//! `wl_shm` `Argb8888` is little-endian, so a pixel is B, G, R, A in memory,
//! while tiny-skia lays its pixmaps out R, G, B, A. Rather than sweep the whole
//! buffer after every frame, the colours handed to tiny-skia have red and blue
//! already swapped: a channel permutation commutes with alpha blending, so the
//! bytes land in the order Wayland wants for free.
//!
//! The buffer is rendered at the output's scale and tagged with
//! `set_buffer_scale`, so on this 2x display the pill is a real 1120x88 image
//! rather than a doubled 560x44 one.
//!
//! Hiding attaches a null buffer, which unmaps the surface. The layer-shell
//! protocol says an unmapped surface goes back to its initial state, so showing
//! it again is a commit with no buffer, a configure, and then the first frame.
//! That round trip happens once per dictation and costs a fraction of a
//! millisecond, which is cheaper than leaving a surface mapped all day.

use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Duration;

use ab_glyph::{point, Font, FontVec, PxScale, PxScaleFont, ScaleFont};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::reexports::client as wayland_client;
use smithay_client_toolkit::{delegate_registry, registry_handlers};
use tiny_skia::{FillRule, Paint, PathBuilder, PixmapMut, Transform};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_output, wl_shm, wl_surface};
use wayland_client::{Connection, EventQueue, QueueHandle};

use crate::overlay_model as model;
use crate::overlay_model::{OverlayModel, Tick, BARS};

pub use crate::overlay_model::{OverlayState, LISTENING_TICK};

/// How far above the bottom of the screen the pill floats, in logical pixels.
/// Windows measures from the bottom of the work area, which already excludes
/// the taskbar; a layer surface is positioned against the raw screen edge, so
/// the number is smaller here for the same apparent gap.
const MARGIN_BOTTOM: i32 = 48;

/// The pill background in the byte order the shm buffer uses.
const BG_BYTES: [u8; 3] = [model::COL_BG[2], model::COL_BG[1], model::COL_BG[0]];

/// Where to look for the two faces, best first. Noto Sans is on every Omarchy
/// box; the rest are there so a stripped system still gets readable words
/// rather than an empty pill.
const TEXT_FACES: &[&str] = &[
    "/usr/share/fonts/noto/NotoSans-Medium.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
];
const HINT_FACES: &[&str] = &[
    "/usr/share/fonts/noto/NotoSans-SemiBold.ttf",
    "/usr/share/fonts/noto/NotoSans-Bold.ttf",
    "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Bold.ttf",
];

pub struct Overlay {
    m: OverlayModel,
    wl: Option<Wayland>,
    fonts: Option<Fonts>,
    /// Whether a buffer is attached and the compositor is showing the surface.
    visible: bool,
}

/// The Wayland connection and everything hanging off it. Lives on the thread
/// that created it, which is the main loop's thread.
struct Wayland {
    conn: Connection,
    queue: EventQueue<Surface>,
    surface: Surface,
}

/// The dispatch state. Kept apart from `Overlay` so a tick can borrow the queue
/// and the state at the same time.
struct Surface {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    /// Held so the bound `wl_compositor` outlives the surface and the region
    /// that were made from it.
    _compositor: CompositorState,
    layer: LayerSurface,
    /// Empty, so pointer events go straight through to what is underneath.
    _input_region: Region,
    pool: Option<SlotPool>,
    /// Two, so the next frame can be drawn while the compositor still holds the
    /// last one.
    buffers: Vec<Buffer>,
    next_buffer: usize,
    /// Output scale factor. 2 on this machine.
    scale: i32,
    /// Device pixels.
    width: i32,
    height: i32,
    /// The compositor has sent a configure since the last map.
    configured: bool,
    /// The compositor asked us to go away.
    closed: bool,
}

impl Overlay {
    /// An overlay that draws nothing, for when the surface cannot be created.
    /// Dictation has to keep working with no visual feedback.
    pub fn disabled() -> Overlay {
        Overlay { m: OverlayModel::new("Ctrl"), wl: None, fonts: None, visible: false }
    }

    pub fn create(hint_key: &str) -> Result<Overlay, String> {
        // On Windows the pill says "Hold [Ctrl]" because that is what the
        // keycap on the keyboard says. Here the README and the Hyprland bind
        // both say Right Ctrl, and the left one does nothing, so say so.
        let label = if hint_key == "Ctrl" { "Right Ctrl" } else { hint_key };
        let fonts = Fonts::load()?;
        let wl = Wayland::create()?;
        Ok(Overlay { m: OverlayModel::new(label), wl: Some(wl), fonts: Some(fonts), visible: false })
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Whether the surface and the fonts are both there. Without the fonts the
    /// pill would be a black bar with no words in it, which is worse than no
    /// pill at all.
    pub fn is_drawable(&self) -> bool {
        self.wl.is_some() && self.fonts.is_some()
    }

    /// Hands the overlay the counter the capture thread writes peaks into.
    pub fn attach_level(&mut self, source: Arc<AtomicU32>) {
        self.m.attach_level(source);
    }

    /// Live microphone level from a raw sample peak, 0..1.
    pub fn set_level(&mut self, peak: f32) {
        self.m.set_level(peak);
    }

    pub fn set(&mut self, state: OverlayState, text: &str) {
        self.m.set(state, text);
        self.render();
    }

    /// Called from the main loop. Pumps the Wayland connection, advances the
    /// animation, repaints, and takes the pill down when a confirmation has
    /// been up long enough. Returns how long to wait before the next tick, or
    /// None when the screen is clear and the loop can sleep indefinitely.
    pub fn tick(&mut self) -> Option<Duration> {
        self.pump();
        let next = match self.m.tick() {
            Tick::Redraw(next) => {
                self.render();
                Some(next)
            }
            Tick::Wait(next) => Some(next),
            Tick::Hide => {
                self.set(OverlayState::Hidden, "");
                None
            }
            Tick::Idle => None,
        };
        if let Some(wl) = self.wl.as_mut() {
            let _ = wl.conn.flush();
        }
        next
    }

    /// Reads whatever the compositor has sent and acts on it, without ever
    /// blocking. The pill's cadence comes from `tick`, not from frame
    /// callbacks, so there is never anything to wait for here.
    fn pump(&mut self) {
        let Some(wl) = self.wl.as_mut() else { return };
        if wl.dispatch().is_err() || wl.surface.closed {
            self.wl = None;
            self.visible = false;
            return;
        }
        // A monitor change can hand us a different scale factor, which means
        // new buffers at a new size.
        if wl.surface.pool.is_none() {
            wl.surface.rebuild();
            if self.visible {
                self.render();
            }
        }
    }

    fn render(&mut self) {
        let (Some(wl), Some(fonts)) = (self.wl.as_mut(), self.fonts.as_ref()) else { return };

        if self.m.state == OverlayState::Hidden {
            if self.visible {
                wl.surface.hide();
                self.visible = false;
                let _ = wl.conn.flush();
            }
            return;
        }

        if !self.visible {
            // An unmapped layer surface has to be re-mapped with an empty
            // commit and a fresh configure before a buffer may be attached.
            wl.surface.request_map();
            let _ = wl.roundtrip();
        }
        if wl.surface.paint(&self.m, fonts) {
            self.visible = true;
        }
        let _ = wl.conn.flush();
    }
}

impl Wayland {
    fn create() -> Result<Wayland, String> {
        let conn = Connection::connect_to_env()
            .map_err(|e| format!("no Wayland display: {e}"))?;
        let (globals, queue) = registry_queue_init::<Surface>(&conn)
            .map_err(|e| format!("Wayland registry: {e}"))?;
        let qh = queue.handle();

        let compositor = CompositorState::bind(&globals, &qh)
            .map_err(|e| format!("wl_compositor: {e}"))?;
        let layer_shell = LayerShell::bind(&globals, &qh)
            .map_err(|e| format!("no wlr-layer-shell, which Flow's pill needs: {e}"))?;
        let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {e}"))?;

        let wl_surface = compositor.create_surface(&qh);
        let layer = layer_shell.create_layer_surface(
            &qh,
            wl_surface,
            Layer::Overlay,
            Some("flow"),
            None,
        );
        layer.set_anchor(Anchor::BOTTOM);
        layer.set_size(model::WIDTH as u32, model::HEIGHT as u32);
        layer.set_margin(0, 0, MARGIN_BOTTOM, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        // Never reserve screen space: the pill floats over whatever is there.
        layer.set_exclusive_zone(-1);

        // An empty region: every click and every hover goes to the window
        // underneath, exactly as if the pill were not there.
        let input_region =
            Region::new(&compositor).map_err(|e| format!("wl_region: {e}"))?;
        layer.set_input_region(Some(input_region.wl_region()));

        let surface = Surface {
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),
            shm,
            _compositor: compositor,
            layer,
            _input_region: input_region,
            pool: None,
            buffers: Vec::new(),
            next_buffer: 0,
            scale: 1,
            width: 0,
            height: 0,
            configured: false,
            closed: false,
        };

        let mut wl = Wayland { conn, queue, surface };
        // Learn what outputs exist before asking any of them for a scale.
        wl.roundtrip()?;
        wl.surface.scale = wl.surface.output_scale();
        wl.surface.rebuild();
        Ok(wl)
    }

    fn dispatch(&mut self) -> Result<(), String> {
        let Wayland { conn, queue, surface } = self;
        queue.dispatch_pending(surface).map_err(|e| e.to_string())?;
        // Nothing was buffered; see whether the socket has anything for us,
        // without waiting for it.
        if let Some(guard) = conn.prepare_read() {
            if readable(&guard) {
                match guard.read() {
                    Ok(_) => {
                        queue.dispatch_pending(surface).map_err(|e| e.to_string())?;
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
        }
        Ok(())
    }

    fn roundtrip(&mut self) -> Result<(), String> {
        let Wayland { queue, surface, .. } = self;
        queue.roundtrip(surface).map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Whether the Wayland socket has bytes waiting. `poll` with a zero timeout, so
/// this never costs the loop anything.
fn readable(guard: &wayland_client::backend::ReadEventsGuard) -> bool {
    use std::os::fd::AsRawFd;
    let mut fds = [libc::pollfd {
        fd: guard.connection_fd().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }];
    // SAFETY: one initialised pollfd, zero timeout, no ownership taken.
    unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) > 0 && fds[0].revents & libc::POLLIN != 0 }
}

impl Surface {
    /// The scale factor of the busiest output we can see. Wayland only tells a
    /// surface which output it is on once it is mapped, and the pill has to
    /// look right on its very first frame, so this guesses from the outputs
    /// themselves and `scale_factor_changed` corrects it later if need be.
    fn output_scale(&self) -> i32 {
        self.output_state
            .outputs()
            .filter_map(|o| self.output_state.info(&o))
            .map(|i| i.scale_factor)
            .max()
            .unwrap_or(1)
            .max(1)
    }

    /// Builds the shm pool and the two buffers for the current scale.
    fn rebuild(&mut self) {
        let scale = self.scale.max(1);
        self.width = (model::WIDTH * scale as f32) as i32;
        self.height = (model::HEIGHT * scale as f32) as i32;
        let frame = (self.width * self.height * 4) as usize;

        self.buffers.clear();
        self.pool = None;
        let Ok(mut pool) = SlotPool::new(frame * 2, &self.shm) else { return };
        let stride = self.width * 4;
        for _ in 0..2 {
            let Ok(slot) = pool.new_slot(frame) else { return };
            let Ok(buffer) = pool.create_buffer_in(
                &slot,
                self.width,
                self.height,
                stride,
                wl_shm::Format::Argb8888,
            ) else {
                return;
            };
            self.buffers.push(buffer);
        }
        self.next_buffer = 0;
        self.pool = Some(pool);
        let _ = self.layer.set_buffer_scale(scale as u32);
    }

    /// Unmaps the surface. The layer surface object stays, so showing the pill
    /// again does not mean renegotiating with the compositor from scratch.
    fn hide(&mut self) {
        self.layer.attach(None, 0, 0);
        self.layer.commit();
        self.configured = false;
    }

    /// The empty commit that asks the compositor to map us and send a
    /// configure. The caller round-trips afterwards.
    fn request_map(&mut self) {
        if !self.configured {
            self.layer.commit();
        }
    }

    /// Draws one frame and puts it on screen. Returns whether anything was
    /// attached: a frame is skipped when the compositor still holds both
    /// buffers, which double buffering makes rare and which costs 40 ms.
    fn paint(&mut self, m: &OverlayModel, fonts: &Fonts) -> bool {
        if self.buffers.len() < 2 {
            return false;
        }
        let (w, h, scale) = (self.width, self.height, self.scale as f32);

        // Whichever of the two the compositor is not reading.
        let mut chosen = None;
        for step in 0..2 {
            let idx = (self.next_buffer + step) % 2;
            if self.buffers[idx].slot().has_active_buffers() {
                continue;
            }
            chosen = Some(idx);
            break;
        }
        let Some(idx) = chosen else { return false };
        self.next_buffer = (idx + 1) % 2;

        let buffer = &self.buffers[idx];
        let Some(pool) = self.pool.as_mut() else { return false };
        let Some(canvas) = buffer.canvas(pool) else { return false };

        draw(m, fonts, canvas, w, h, scale);

        let surface = self.layer.wl_surface();
        surface.damage_buffer(0, 0, w, h);
        if buffer.attach_to(surface).is_err() {
            return false;
        }
        self.layer.commit();
        true
    }
}

// ---- drawing ---------------------------------------------------------------

/// A shared RGB colour as a tiny-skia colour with red and blue swapped, so the
/// pixmap bytes come out in `wl_shm` `Argb8888` order.
fn colour(rgb: [u8; 3], alpha: u8) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(rgb[2], rgb[1], rgb[0], alpha)
}

/// One frame of the pill, into a buffer of `w` by `h` device pixels.
///
/// Deliberately a free function over a byte slice: it is the whole of what the
/// pill looks like, and nothing in it should be able to touch the Wayland
/// connection.
fn draw(m: &OverlayModel, fonts: &Fonts, canvas: &mut [u8], w: i32, h: i32, scale: f32) {
    let px = |v: f32| (v * scale).round() as i32;
    let text_font = fonts.words(model::TEXT_FONT_HEIGHT * scale);
    let hint_font = fonts.hint(model::HINT_FONT_HEIGHT * scale);

    let accent = m.accent();
    let live = m.is_live();
    let pad = px(model::PAD);
    let dot_r = px(model::DOT_RADIUS);
    let bar_w = px(model::BAR_WIDTH).max(2);
    let bar_gap = px(model::BAR_GAP).max(2);
    let wave_left = pad + dot_r * 2 + px(model::DOT_TO_WAVE);
    let wave_w = bar_w * BARS as i32 + bar_gap * (BARS as i32 - 1);

    // ---- work out where the words may go, before drawing anything ----------
    // The hint on the right is fixed, so the text knows where it has to stop.
    let mut right_edge = w - pad;
    let hint = if m.state == OverlayState::Listening {
        let cap_text_w = hint_font.width(&m.hint_key);
        let cap_pad = px(model::KEYCAP_PAD);
        let cap_w = cap_text_w + cap_pad * 2;
        let cap_h = px(model::KEYCAP_HEIGHT);
        let cap_left = right_edge - cap_w;
        let hold_w = hint_font.width("Hold");
        let hold_left = cap_left - px(model::HOLD_TO_KEYCAP) - hold_w;
        right_edge = hold_left - px(model::HINT_TO_TEXT);
        Some(Hint { cap_left, cap_top: h / 2 - cap_h / 2, cap_w, cap_h, cap_pad, hold_left })
    } else {
        None
    };

    let text_left = if live { wave_left + wave_w + px(model::WAVE_TO_TEXT) } else { wave_left };
    let caret_space =
        if m.state == OverlayState::Listening { px(model::CARET_SPACE) } else { 0 };
    let text_max = (right_edge - text_left - caret_space).max(0);
    let body = m.body();
    let (shown, clipped) = model::tail_that_fits(&body, text_max, |s| text_font.width(s));
    let shown_w = text_font.width(&shown);

    // ---- shapes -------------------------------------------------------------
    {
        let Some(mut pixmap) = PixmapMut::from_bytes(canvas, w as u32, h as u32) else { return };
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        let mut paint = Paint::default();
        paint.anti_alias = true;

        // The pill. A stadium: the corner radius is half the height, so the
        // ends are semicircles.
        paint.set_color(colour(model::COL_BG, 255));
        if let Some(path) =
            rounded_rect(0.0, 0.0, w as f32, h as f32, h as f32 / 2.0)
        {
            pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }

        // Recording dot.
        let dot_dim = if m.state == OverlayState::Finalising { 0.55 } else { 1.0 };
        paint.set_color(colour(model::dim(accent, dot_dim), 255));
        if let Some(path) = PathBuilder::from_circle(
            (pad + dot_r) as f32,
            (h / 2) as f32,
            dot_r as f32,
        ) {
            pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }

        // Waveform.
        if live {
            let max_h = px(model::BAR_MAX_HEIGHT);
            let min_h = px(model::BAR_MIN_HEIGHT).max(2);
            for i in 0..BARS {
                let value = m.bars[i].clamp(0.0, 1.0);
                let bar_h = ((max_h as f32) * value).round().max(min_h as f32) as i32;
                let x = wave_left + i as i32 * (bar_w + bar_gap);
                let top = h / 2 - bar_h / 2;
                // Taller bars brighter, so the trace has depth instead of
                // being a row of identical marks.
                paint.set_color(colour(
                    model::dim(model::COL_WAVE, 0.42 + 0.58 * value),
                    255,
                ));
                // Short bars are plain rectangles: a rounded end on something
                // three pixels tall is not a shape, it is a smudge.
                let radius = if bar_h > bar_w * 3 { bar_w as f32 / 2.0 } else { 0.0 };
                if let Some(path) = rounded_rect(
                    x as f32,
                    top as f32,
                    (x + bar_w) as f32,
                    (top + bar_h) as f32,
                    radius,
                ) {
                    pixmap.fill_path(
                        &path,
                        &paint,
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
            }
        }

        // Keycap.
        if let Some(hint) = &hint {
            paint.set_color(colour(model::COL_KEYCAP, 255));
            if let Some(path) = rounded_rect(
                hint.cap_left as f32,
                hint.cap_top as f32,
                (hint.cap_left + hint.cap_w) as f32,
                (hint.cap_top + hint.cap_h) as f32,
                px(model::KEYCAP_RADIUS) as f32,
            ) {
                pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            }
        }

        // Caret, right after the words: the same signal a text field gives.
        if m.state == OverlayState::Listening {
            let caret_x = (text_left + shown_w + px(model::CARET_OFFSET)).min(right_edge - px(2.0));
            let caret_h = px(model::CARET_HEIGHT);
            let caret_w = px(model::CARET_WIDTH).max(1);
            paint.set_color(colour(model::COL_ACCENT, 255));
            if let Some(path) = rounded_rect(
                caret_x as f32,
                (h / 2 - caret_h / 2) as f32,
                (caret_x + caret_w) as f32,
                (h / 2 + caret_h / 2) as f32,
                0.0,
            ) {
                pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            }
        }
    }

    // ---- text ---------------------------------------------------------------
    if let Some(hint) = &hint {
        hint_font.draw(
            canvas,
            w,
            h,
            hint.cap_left + hint.cap_pad,
            centred_baseline(&hint_font, h),
            &m.hint_key,
            model::COL_KEYCAP_TEXT,
        );
        hint_font.draw(
            canvas,
            w,
            h,
            hint.hold_left,
            centred_baseline(&hint_font, h),
            "Hold",
            model::COL_HINT,
        );
    }
    if !shown.is_empty() {
        text_font.draw(
            canvas,
            w,
            h,
            text_left,
            centred_baseline(&text_font, h),
            &shown,
            m.text_colour(),
        );
    }

    // ---- compose -------------------------------------------------------------
    if clipped {
        model::fade_into_background(
            canvas,
            w,
            h,
            text_left,
            text_left + px(model::FADE_WIDTH),
            BG_BYTES,
        );
    }
    // Not quite solid, so what is behind the pill is still faintly there.
    // Scaling all four channels together keeps the premultiplied buffer valid
    // and dims the antialiased edge by exactly as much as the middle.
    let opacity = model::PILL_OPACITY as u32;
    for byte in canvas.iter_mut() {
        *byte = ((*byte as u32 * opacity) / 255) as u8;
    }
}

/// Where the keycap and its label sit, worked out before anything is drawn.
struct Hint {
    cap_left: i32,
    cap_top: i32,
    cap_w: i32,
    cap_h: i32,
    cap_pad: i32,
    hold_left: i32,
}

/// A rectangle with equal corner radii, as a fillable path. `radius` of zero
/// gives a plain rectangle.
fn rounded_rect(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let r = radius.min((right - left) / 2.0).min((bottom - top) / 2.0).max(0.0);
    if r <= 0.0 {
        pb.push_rect(tiny_skia::Rect::from_ltrb(left, top, right, bottom)?);
        return pb.finish();
    }
    // The circle-through-Bezier constant: four cubics with control points this
    // far along the tangents land within a thousandth of a true arc.
    const K: f32 = 0.552_284_75;
    let c = r * K;
    pb.move_to(left + r, top);
    pb.line_to(right - r, top);
    pb.cubic_to(right - r + c, top, right, top + r - c, right, top + r);
    pb.line_to(right, bottom - r);
    pb.cubic_to(right, bottom - r + c, right - r + c, bottom, right - r, bottom);
    pb.line_to(left + r, bottom);
    pb.cubic_to(left + r - c, bottom, left, bottom - r + c, left, bottom - r);
    pb.line_to(left, top + r);
    pb.cubic_to(left, top + r - c, left + r - c, top, left + r, top);
    pb.close();
    pb.finish()
}

/// The baseline that centres a line of text in a pill `h` pixels tall, the way
/// `DT_VCENTER` does: the font's cell, ascent plus descent, is centred rather
/// than the ink of these particular letters.
fn centred_baseline(font: &Face, h: i32) -> i32 {
    let cell = font.scaled.ascent() - font.scaled.descent();
    ((h as f32 - cell) / 2.0 + font.scaled.ascent()).round() as i32
}

// ---- fonts -----------------------------------------------------------------

/// The two faces, loaded once when the overlay is created. Reading and parsing
/// a font per frame would be the most expensive thing the pill did.
struct Fonts {
    text: FontVec,
    hint: FontVec,
}

impl Fonts {
    fn load() -> Result<Fonts, String> {
        Ok(Fonts { text: first_of(TEXT_FACES)?, hint: first_of(HINT_FACES)? })
    }

    /// The words.
    fn words(&self, cell_px: f32) -> Face<'_> {
        Face::new(&self.text, cell_px)
    }

    /// "Hold" and the key name, a touch heavier so the keycap reads as a key.
    fn hint(&self, cell_px: f32) -> Face<'_> {
        Face::new(&self.hint, cell_px)
    }
}

fn first_of(paths: &[&str]) -> Result<FontVec, String> {
    for path in paths {
        let Ok(bytes) = std::fs::read(path) else { continue };
        if let Ok(font) = FontVec::try_from_vec(bytes) {
            return Ok(font);
        }
    }
    Err(format!("no usable font, looked for {}", paths.join(", ")))
}

/// A font at a size, with the measuring and rasterising the pill needs.
struct Face<'a> {
    scaled: PxScaleFont<&'a FontVec>,
}

impl<'a> Face<'a> {
    /// `cell_px` is the height `CreateFontW` would have been given on Windows:
    /// ascent plus descent, not the em size. `PxScale` happens to mean exactly
    /// the same thing, so the two platforms' text comes out the same size from
    /// the same number.
    fn new(font: &'a FontVec, cell_px: f32) -> Face<'a> {
        Face { scaled: font.as_scaled(PxScale::from(cell_px)) }
    }

    /// Width of a string in device pixels, advances and kerning included.
    fn width(&self, s: &str) -> i32 {
        let mut x = 0.0f32;
        let mut previous = None;
        for ch in s.chars() {
            let id = self.scaled.glyph_id(ch);
            if let Some(prev) = previous {
                x += self.scaled.kern(prev, id);
            }
            x += self.scaled.h_advance(id);
            previous = Some(id);
        }
        x.round() as i32
    }

    /// Rasterises a string onto the buffer at an integer pen position.
    ///
    /// Every glyph lands on a whole pixel. Sub-pixel positioning would look
    /// marginally better on a still frame, but the words here are redrawn 25
    /// times a second with the caret chasing their right edge, and a caret that
    /// shifts by a third of a pixel between frames reads as a wobble.
    fn draw(
        &self,
        buf: &mut [u8],
        w: i32,
        h: i32,
        x: i32,
        baseline: i32,
        s: &str,
        rgb: [u8; 3],
    ) {
        let bgr = [rgb[2], rgb[1], rgb[0]];
        let mut pen = x as f32;
        let mut previous = None;
        for ch in s.chars() {
            let id = self.scaled.glyph_id(ch);
            if let Some(prev) = previous {
                pen += self.scaled.kern(prev, id);
            }
            let mut glyph = self.scaled.scaled_glyph(ch);
            glyph.position = point(pen.round(), baseline as f32);
            if let Some(outline) = self.scaled.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                let (ox, oy) = (bounds.min.x as i32, bounds.min.y as i32);
                outline.draw(|gx, gy, coverage| {
                    blend(buf, w, h, ox + gx as i32, oy + gy as i32, bgr, coverage);
                });
            }
            pen += self.scaled.h_advance(id);
            previous = Some(id);
        }
    }
}

/// One antialiased pixel of a glyph, over whatever is already there.
fn blend(buf: &mut [u8], w: i32, h: i32, x: i32, y: i32, bgr: [u8; 3], coverage: f32) {
    if x < 0 || y < 0 || x >= w || y >= h {
        return;
    }
    let c = coverage.clamp(0.0, 1.0);
    if c <= 0.0 {
        return;
    }
    let i = ((y * w + x) * 4) as usize;
    for k in 0..3 {
        buf[i + k] = (bgr[k] as f32 * c + buf[i + k] as f32 * (1.0 - c)) as u8;
    }
    buf[i + 3] = (255.0 * c + buf[i + 3] as f32 * (1.0 - c)) as u8;
}

// ---- Wayland handlers ------------------------------------------------------

impl CompositorHandler for Surface {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if new_factor != self.scale && new_factor >= 1 {
            self.scale = new_factor;
            // Dropping the pool is the signal to the next pump that the buffers
            // are the wrong size now.
            self.pool = None;
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    /// The pill's cadence comes from the main loop's timer, not from the
    /// compositor, so a frame callback has nothing to do. None is ever
    /// requested; this is here because the trait needs it.
    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Surface {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for Surface {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.closed = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        _configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        // The size is ours: the pill never resizes, and a compositor that
        // suggests otherwise is told the same 560x44 again on the next commit.
        self.configured = true;
    }
}

impl ShmHandler for Surface {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Surface {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_registry!(Surface);
smithay_client_toolkit::delegate_dispatch2!(Surface);

#[cfg(test)]
mod tests {
    use super::*;

    /// The pill has to be opaque black in the middle and gone at the corners,
    /// whichever platform drew it.
    #[test]
    fn the_pill_is_a_stadium() {
        let (w, h) = (120, 40);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let mut pixmap = PixmapMut::from_bytes(&mut buf, w as u32, h as u32).unwrap();
        let mut paint = Paint::default();
        paint.anti_alias = true;
        paint.set_color(colour(model::COL_BG, 255));
        let path = rounded_rect(0.0, 0.0, w as f32, h as f32, h as f32 / 2.0).unwrap();
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);

        let alpha = |x: i32, y: i32| buf[((y * w + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0, "top-left corner cut away");
        assert_eq!(alpha(w - 1, h - 1), 0, "bottom-right too");
        assert_eq!(alpha(w / 2, h / 2), 255, "middle solid");
        assert_eq!(alpha(w / 2, 0), 255, "top edge is not a corner");
    }

    /// The swizzle is the one thing that would silently turn the coral dot
    /// blue, so it is worth a test rather than a comment.
    #[test]
    fn colours_come_out_in_shm_byte_order() {
        let c = colour(model::COL_ACCENT, 255);
        assert_eq!(c.to_color_u8().red(), model::COL_ACCENT[2], "blue lands first");
        assert_eq!(c.to_color_u8().green(), model::COL_ACCENT[1]);
        assert_eq!(c.to_color_u8().blue(), model::COL_ACCENT[0], "red lands third");
    }

    #[test]
    fn a_plain_rectangle_has_square_corners() {
        let path = rounded_rect(0.0, 0.0, 10.0, 4.0, 0.0).unwrap();
        let b = path.bounds();
        assert_eq!((b.left(), b.top(), b.right(), b.bottom()), (0.0, 0.0, 10.0, 4.0));
    }
}

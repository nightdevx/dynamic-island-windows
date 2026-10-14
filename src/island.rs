use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use windows::{
    core::*,
    Foundation::{IReference, PropertyValue, TimeSpan, TypedEventHandler},
    Media::MediaPlaybackAutoRepeatMode,
    UI::{Color, Composition::*},
    Win32::{Foundation::HWND, System::WinRT::Composition::ICompositorDesktopInterop},
};
use windows_numerics::{Vector2, Vector3};

use crate::{
    blur,
    claude::{Limits, Session},
    media::{Art, Media},
    shelf,
    pages::{self, ART, ART_SOURCE, BAR, BUTTON, BUTTONS_Y, BUTTON_X, HEART, PAGE, TEXT, TIMES},
    render::{Align, Painter},
    text::tr,
};

// Sizes in DIPs; multiplied by the monitor scale at runtime.
pub const TOP: f32 = 8.0; // gap between the screen edge and the pill
const MARGIN: f32 = 16.0; // room inside the window for spring overshoot
pub const COMPACT: (f32, f32) = (330.0, 44.0);
pub const PEEK: (f32, f32) = (360.0, 48.0);
/// A notification's preview: a peek as wide, tall enough for who sent it, a title and a line.
pub const NOTICE: (f32, f32) = (PEEK.0, 64.0);
pub const EXPANDED: (f32, f32) = (380.0, PAGE_Y + PAGE.1);
const MAX_RADIUS: f32 = 32.0;
// The music glow's wider stroke; half of it lies outside the pill, inside the hit region's pad.
const GLOW_WIDTH: f32 = 5.0;
const GLOW_ALPHA: u32 = 0x0E; // per stroke; where both overlap, near the edge, about 11%
// The width is a little springier than the height, and the two move a beat apart: the pill
// stretches sideways, then down (and back up, then in), like Apple's island.
const SPRINGS: [Spring; 2] = [Spring { damping: 0.8, period_ms: 70.0 }, Spring { damping: 0.86, period_ms: 74.0 }];
const STAGGER_MS: u64 = 24;
// How soft the transitions are, DIPs.
const BLUR: f32 = 10.0; // content is this blurred as it comes and goes
const RISE: f32 = 10.0; // and items rise this far into place
const HOVER: (f32, f32) = (6.0, 2.0); // the closed pill grows this much under the mouse
// The pill's shadow: faint black rings on its edge, widest outermost; half of each is under it.
const SHADOW: [f32; 3] = [14.0, 8.0, 3.5];
const SHADOW_ALPHA: u32 = 0x12;

// Compact: a lead (Claude's mark, or the art), two lines of text, and the line's own item on
// the right (the 5-hour limit ring, or the equalizer). Both lines' text surfaces span from
// TEXT_X to the pill's end.
const MARK: f32 = 20.0;
const TEXT_X: f32 = 46.0;
const LINES: (f32, f32) = (COMPACT.0 - TEXT_X, COMPACT.1);
const SMALL_ART: f32 = 30.0;
/// Right padding: the line's own item (the ring) is centered in the pill's round end, as the
/// lead item (mark or art) is in the other end.
const EDGE: f32 = COMPACT.1 / 2.0 - pages::RING / 2.0;
const GAP: f32 = 8.0; // between the text and the item after it
const BARS: usize = 4;
const BAR_W: f32 = 3.0;
const BAR_H: f32 = 14.0;
const BARS_W: f32 = BARS as f32 * (BAR_W + 3.0) - 3.0;

/// Where the line's own item ends, from the pill's left.
const ITEMS_END: f32 = COMPACT.0 - EDGE;

// Microphone/camera in use: the closed pill grows this much and shows a dot at its right end.
const PRIVACY_EXTRA: f32 = 24.0;
const DOT: f32 = 7.0;

// Expanded: tab bar on top, the selected page below it.
const TAB_W: f32 = 82.0;
const TAB_H: f32 = 24.0;
const TAB_Y: f32 = 10.0;
const PAGE_Y: f32 = 40.0;
/// The settings button, right of the tabs and left of the privacy dot.
const GEAR: (f32, f32, f32, f32) = (EXPANDED.0 - 64.0, TAB_Y, 30.0, TAB_H);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum View {
    Compact,
    Peek,
    Expanded,
}

impl View {
    pub fn size(self) -> (f32, f32) {
        match self {
            View::Compact => COMPACT,
            View::Peek => PEEK,
            View::Expanded => EXPANDED,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tab {
    Music,
    Claude,
    Shelf,
}

impl Tab {
    fn index(self) -> usize {
        self as usize
    }
}

/// What the closed island shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Line {
    Claude,
    Music,
}

/// What a click in the open island lands on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Tab(Tab),
    Previous,
    PlayPause,
    Next,
    Shuffle,
    Repeat,
    Seek(f64),        // 0..1 of the track
    Session(usize),   // row on the Claude page
    ShelfItem(usize), // a file on the shelf, by its index in the list
    Segment(usize),   // the shelf's list: files or screenshots
    SelectAll,
    ClearShelf,
    Like,
    Settings,
    ShowPlayer, // art, title or artist: bring the player to the front
    NextPlayer, // the player line, with more than one player
}

// While music plays, the 5-hour limit moves out of the closed pill into a bubble beside it, as a
// second activity does on the iPhone. A circle as tall as the pill.
const BUBBLE: f32 = COMPACT.1;
const BUBBLE_GAP: f32 = 6.0;
/// The widest the island gets: open, or closed at its widest with the bubble on the right
/// (the pill stays centered, so as much room on the left).
const WIDEST: f32 = {
    let closed = COMPACT.0 + PRIVACY_EXTRA + HOVER.0 + 2.0 * (BUBBLE_GAP + BUBBLE);
    if closed > EXPANDED.0 { closed } else { EXPANDED.0 }
};

/// Window size in pixels: the largest view plus room for spring overshoot.
pub fn window_size(scale: f32) -> (i32, i32) {
    (
        ((WIDEST + 2.0 * MARGIN) * scale).ceil() as i32,
        ((TOP + EXPANDED.1 + MARGIN) * scale).ceil() as i32,
    )
}

fn ms(v: i64) -> TimeSpan {
    TimeSpan { Duration: v * 10_000 }
}

fn argb(c: u32) -> Color {
    Color { A: (c >> 24) as u8, R: (c >> 16) as u8, G: (c >> 8) as u8, B: c as u8 }
}

/// The composition tree of the pill. All animation runs in the compositor (DWM); the app
/// only redraws a surface when what it shows changes.
pub struct Island {
    scale: f32,
    window: Vector2,
    compositor: Compositor,
    _target: Desktop::DesktopWindowTarget,
    painter: Painter,
    /// Pill width "W" and height "H" in pixels; everything else is an expression of these.
    size: CompositionPropertySet,
    motion: [Motion; 2], // W, H
    springs: [SpringScalarNaturalMotionAnimation; 2],
    easing: CubicBezierEasingFunction,
    view: View,
    full_motion: bool, // Windows' animation effects are on
    hover: bool,
    /// The three views' layers each blur through their brush while they come or go; the
    /// counter tells the last transition's end from an earlier one's.
    blurs: [CompositionEffectBrush; 3],
    transitions: Arc<AtomicUsize>,
    compact: LayerVisual,
    claude_line: ContainerVisual,
    mark: SpriteVisual,
    mark_surface: CompositionDrawingSurface,
    line_surface: CompositionDrawingSurface,
    music_line: ContainerVisual,
    title_surface: CompositionDrawingSurface,
    title_text: SpriteVisual,
    bars: Vec<SpriteVisual>,
    accent: CompositionColorBrush, // equalizer and progress, from the art
    glow: ShapeVisual,
    glow_brushes: [CompositionColorBrush; 2], // the two strokes, same faint color
    /// The art is drawn into one surface while the other shows, then they cross-fade.
    art_surfaces: [CompositionDrawingSurface; 2],
    art_front: usize,
    art_frames: [ContainerVisual; 3],     // where the art shows: closed pill, peek, music page
    art_layers: [[SpriteVisual; 2]; 3],   // in each frame, one sprite per surface
    tint: SpriteVisual,                   // the music page's wash of the art's color
    compact_tint: SpriteVisual,           // the closed pill's, under its music line
    tint_stops: [CompositionColorGradientStop; 2],
    tinted: bool,                         // the art has a color to wash with
    present: bool,                        // something plays
    peek: LayerVisual,
    peek_surface: CompositionDrawingSurface,
    notice_icon: SpriteVisual, // a notification's icon, which pops in
    notice_surface: CompositionDrawingSurface,
    ripple: ShapeVisual, // a soft ring spreading from the icon to the pill's round end
    expanded: LayerVisual,
    highlight: ShapeVisual,
    pages: [ContainerVisual; 3],
    page_surfaces: [CompositionDrawingSurface; 3],
    page_texts: [SpriteVisual; 3],
    /// Per page, what rises into place when the island opens, group by group.
    staggered: [Vec<Vec<Visual>>; 3],
    times_surface: CompositionDrawingSurface,
    progress_track: ContainerVisual,
    progress: SpriteVisual,
    buttons: [SpriteVisual; 6], // previous, play, pause, next, shuffle, repeat
    button_surfaces: [CompositionDrawingSurface; 6],
    heart: SpriteVisual,
    heart_surface: CompositionDrawingSurface,
    liked: Option<bool>, // None: no heart (nothing plays)
    players: u32,        // media sessions there are
    bubble_surface: CompositionDrawingSurface,
    bubble_limit: Option<(f32, u32)>, // the 5-hour limit it shows: used percent, color
    bubble_out: bool,                 // beside the pill, not inside it
    shelf_view: (usize, usize, usize), // the shelf page's first item, items on screen, all items
    privacy_brush: CompositionColorBrush,
    privacy_dots: [SpriteVisual; 2], // closed pill, tab bar
    extra: f32,                      // width the closed pill gives the privacy dot, DIPs
    peek_h: f32, // a peek's height: one line, or a notification
    tab: Tab,
    line: Line,
    equalizer: bool,
    glowing: bool,
}

impl Island {
    /// `device_lost` is posted to `hwnd` when the drawn surfaces lose their content; the
    /// window then builds a new Island.
    pub fn new(hwnd: HWND, scale: f32, device_lost: u32) -> Result<Self> {
        let (w, h) = window_size(scale);
        let window = Vector2 { X: w as f32, Y: h as f32 };
        let compositor = Compositor::new()?;
        let target = unsafe { compositor.cast::<ICompositorDesktopInterop>()?.CreateDesktopWindowTarget(hwnd, true)? };
        let root = compositor.CreateContainerVisual()?;
        target.SetRoot(&root)?;
        let painter = Painter::new(&compositor, scale)?;
        painter.on_device_replaced(hwnd, device_lost)?;
        let px = |size| painter.px(size);
        let at = |x: f32, y: f32| {
            let p = px((x, y));
            Vector3 { X: p.X, Y: p.Y, Z: 0.0 }
        };
        // Top-left corner of the pill at a given size, in window pixels.
        let corner = |size: (f32, f32)| Vector3 { X: ((window.X - px(size).X) / 2.0).round(), Y: (TOP * scale).round(), Z: 0.0 };
        let container = |offset: Vector3, size: (f32, f32)| -> Result<ContainerVisual> {
            let visual = compositor.CreateContainerVisual()?;
            visual.SetOffset(offset)?;
            visual.SetSize(px(size))?;
            Ok(visual)
        };
        let sprite = |brush: &CompositionBrush, offset: Vector3, size: (f32, f32)| -> Result<SpriteVisual> {
            let visual = compositor.CreateSpriteVisual()?;
            visual.SetBrush(brush)?;
            visual.SetOffset(offset)?;
            visual.SetSize(px(size))?;
            let p = px(size);
            visual.SetCenterPoint(Vector3 { X: p.X / 2.0, Y: p.Y / 2.0, Z: 0.0 })?;
            Ok(visual)
        };
        let surface_brush = |surface: &CompositionDrawingSurface| -> Result<CompositionBrush> {
            compositor.CreateSurfaceBrushWithSurface(surface)?.cast()
        };
        let rounded = |visual: &Visual, size: (f32, f32), radius: f32| -> Result<()> {
            let geometry = compositor.CreateRoundedRectangleGeometry()?;
            geometry.SetSize(px(size))?;
            geometry.SetCornerRadius(px((radius, radius)))?;
            visual.SetClip(&compositor.CreateGeometricClipWithGeometry(&geometry)?)
        };
        // Each view is a layer, so it can blur as a whole while it comes and goes.
        let layer = |offset: Vector3, size: (f32, f32)| -> Result<LayerVisual> {
            let visual = compositor.CreateLayerVisual()?;
            visual.SetOffset(offset)?;
            visual.SetSize(px(size))?;
            Ok(visual)
        };

        // One geometry is both the black pill and the clip of the content. Springs move only
        // W and H (WinRT can't box a Vector2 for a vector spring); Size, Offset and
        // CornerRadius are expressions of them, evaluated in the compositor.
        let compact_px = px(COMPACT);
        let size = compositor.CreatePropertySet()?;
        size.InsertScalar(h!("W"), compact_px.X)?;
        size.InsertScalar(h!("H"), compact_px.Y)?;
        let geometry = compositor.CreateRoundedRectangleGeometry()?;
        for (property, expression) in [
            ("Size", "Vector2(P.W, P.H)"),
            ("Offset", "Vector2((Window - P.W) * 0.5, Top)"),
            ("CornerRadius", "Vector2(Min(P.H * 0.5, R), Min(P.H * 0.5, R))"),
        ] {
            let animation = compositor.CreateExpressionAnimationWithExpression(&HSTRING::from(expression))?;
            animation.SetReferenceParameter(h!("P"), &size)?;
            animation.SetScalarParameter(h!("Window"), window.X)?;
            animation.SetScalarParameter(h!("Top"), (TOP * scale).round())?;
            animation.SetScalarParameter(h!("R"), MAX_RADIUS * scale)?;
            geometry.StartAnimation(&HSTRING::from(property), &animation)?;
        }
        let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
        shape.SetFillBrush(&compositor.CreateColorBrushWithColor(argb(0xFF000000))?)?;
        let pill = compositor.CreateShapeVisual()?;
        pill.SetSize(window)?;
        // A soft shadow under it, a little lower: rings on its own geometry, so they follow every
        // spring. Plain shapes, like the glow; a DropShadow needs a layer, and a layer around
        // the pill drew nothing until its first animation.
        let shadow_brush = compositor.CreateColorBrushWithColor(argb(SHADOW_ALPHA << 24))?;
        for width in SHADOW {
            let ring = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
            ring.SetStrokeBrush(&shadow_brush)?;
            ring.SetStrokeThickness(width * scale)?;
            ring.SetOffset(Vector2 { X: 0.0, Y: scale })?;
            pill.Shapes()?.Append(&ring)?;
        }
        pill.Shapes()?.Append(&shape)?;
        root.Children()?.InsertAtTop(&pill)?;

        // While music plays, a soft glow in the art's color: two faint strokes of different
        // widths on the pill's own geometry, so the color fades outward with no hard line and
        // follows every spring. Static: it costs a frame only when it fades.
        let glow_brushes = [compositor.CreateColorBrush()?, compositor.CreateColorBrush()?];
        let glow = compositor.CreateShapeVisual()?;
        glow.SetSize(window)?;
        for (brush, width) in glow_brushes.iter().zip([GLOW_WIDTH, GLOW_WIDTH * 0.45]) {
            let ring = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
            ring.SetStrokeBrush(brush)?;
            ring.SetStrokeThickness(width * scale)?;
            glow.Shapes()?.Append(&ring)?;
        }
        glow.SetOpacity(0.0)?;
        root.Children()?.InsertAtTop(&glow)?;

        // The limit bubble. "S" in the pill's property set says where it is: 0 inside the pill's
        // right end (hidden), 1 beside it; position, size and opacity follow S and the pill's size.
        size.InsertScalar(h!("S"), 0.0)?;
        let bubble_px = px((BUBBLE, BUBBLE));
        let bubble = compositor.CreateContainerVisual()?;
        bubble.SetSize(bubble_px)?;
        bubble.SetCenterPoint(Vector3 { X: bubble_px.X / 2.0, Y: bubble_px.Y / 2.0, Z: 0.0 })?;
        let disc = compositor.CreateEllipseGeometry()?;
        disc.SetCenter(Vector2 { X: bubble_px.X / 2.0, Y: bubble_px.Y / 2.0 })?;
        disc.SetRadius(Vector2 { X: bubble_px.X / 2.0, Y: bubble_px.Y / 2.0 })?;
        let disc_visual = compositor.CreateShapeVisual()?;
        disc_visual.SetSize(bubble_px)?;
        for width in SHADOW {
            let ring = compositor.CreateSpriteShapeWithGeometry(&disc)?;
            ring.SetStrokeBrush(&shadow_brush)?;
            ring.SetStrokeThickness(width * scale)?;
            ring.SetOffset(Vector2 { X: 0.0, Y: scale })?;
            disc_visual.Shapes()?.Append(&ring)?;
        }
        let disc_fill = compositor.CreateSpriteShapeWithGeometry(&disc)?;
        disc_fill.SetFillBrush(&compositor.CreateColorBrushWithColor(argb(0xFF000000))?)?;
        disc_visual.Shapes()?.Append(&disc_fill)?;
        bubble.Children()?.InsertAtTop(&disc_visual)?;
        let bubble_surface = painter.surface((BUBBLE, BUBBLE))?;
        let bubble_ring = compositor.CreateSpriteVisual()?;
        bubble_ring.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&bubble_surface)?)?;
        bubble_ring.SetSize(bubble_px)?;
        bubble.Children()?.InsertAtTop(&bubble_ring)?;
        for (property, expression) in [
            ("Offset", "Vector3((Window + P.W) * 0.5 + Gap - (1 - P.S) * (Gap + B), Top + (P.H - B) * 0.5, 0)"),
            ("Scale", "Vector3(0.5 + 0.5 * P.S, 0.5 + 0.5 * P.S, 1)"),
            ("Opacity", "Clamp(P.S, 0, 1)"),
        ] {
            let animation = compositor.CreateExpressionAnimationWithExpression(&HSTRING::from(expression))?;
            animation.SetReferenceParameter(h!("P"), &size)?;
            animation.SetScalarParameter(h!("Window"), window.X)?;
            animation.SetScalarParameter(h!("Gap"), px((BUBBLE_GAP, 0.0)).X)?;
            animation.SetScalarParameter(h!("B"), bubble_px.X)?;
            animation.SetScalarParameter(h!("Top"), (TOP * scale).round())?;
            bubble.StartAnimation(&HSTRING::from(property), &animation)?;
        }
        root.Children()?.InsertAtTop(&bubble)?;

        let content = compositor.CreateContainerVisual()?;
        content.SetSize(window)?;
        content.SetClip(&compositor.CreateGeometricClipWithGeometry(&geometry)?)?;
        root.Children()?.InsertAtTop(&content)?;

        // The art is drawn once and scaled by every visual that shows it; two surfaces take
        // turns, so a new track's art can cross-fade with the old one.
        let art_surfaces = [painter.surface((ART_SOURCE, ART_SOURCE))?, painter.surface((ART_SOURCE, ART_SOURCE))?];
        let mut art_brushes = Vec::new();
        for surface in &art_surfaces {
            let brush = compositor.CreateSurfaceBrushWithSurface(surface)?;
            brush.SetStretch(CompositionStretch::UniformToFill)?;
            art_brushes.push(brush.cast::<CompositionBrush>()?);
        }
        let accent = compositor.CreateColorBrushWithColor(argb(TEXT))?;
        let accent_brush: CompositionBrush = accent.cast()?;
        let art_frame = |offset: Vector3, side: f32, radius: f32| -> Result<(ContainerVisual, [SpriteVisual; 2])> {
            let frame = container(offset, (side, side))?;
            rounded(&frame.cast()?, (side, side), radius)?;
            let front = sprite(&art_brushes[0], at(0.0, 0.0), (side, side))?;
            let back = sprite(&art_brushes[1], at(0.0, 0.0), (side, side))?;
            back.SetOpacity(0.0)?;
            frame.Children()?.InsertAtTop(&front)?;
            frame.Children()?.InsertAtTop(&back)?;
            Ok((frame, [front, back]))
        };
        let translatable = |visual: &Visual| visual.Properties()?.InsertVector3(h!("Translation"), Vector3::default());

        // While music plays, a wash of the art's color from the top left, as Apple Music's
        // player has: behind the closed pill's music line and the music page. Drawn by the
        // compositor, once per art.
        let tint_stops = [
            compositor.CreateColorGradientStopWithOffsetAndColor(0.0, argb(0))?,
            compositor.CreateColorGradientStopWithOffsetAndColor(1.0, argb(0))?,
        ];
        let tint_brush = compositor.CreateLinearGradientBrush()?;
        tint_brush.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
        tint_brush.SetEndPoint(Vector2 { X: 0.85, Y: 1.0 })?;
        for stop in &tint_stops {
            tint_brush.ColorStops()?.Append(stop)?;
        }

        // Compact. Its content keeps to the pill's left edge while the pill grows for the
        // privacy dot, but not beyond that width: on the way to a bigger view it fades in place.
        let compact = layer(corner(COMPACT), COMPACT)?;
        let follow = compositor.CreateExpressionAnimationWithExpression(h!("Vector3(Round((Window - Min(P.W, Widest)) * 0.5), Top, 0)"))?;
        follow.SetReferenceParameter(h!("P"), &size)?;
        follow.SetScalarParameter(h!("Window"), window.X)?;
        follow.SetScalarParameter(h!("Widest"), px((COMPACT.0 + PRIVACY_EXTRA, 0.0)).X)?;
        follow.SetScalarParameter(h!("Top"), (TOP * scale).round())?;
        compact.StartAnimation(h!("Offset"), &follow)?;
        let privacy_brush = compositor.CreateColorBrush()?;
        let privacy_brush_base: CompositionBrush = privacy_brush.cast()?;
        let dot = |center: (f32, f32)| -> Result<SpriteVisual> {
            let visual = sprite(&privacy_brush_base, at(center.0 - DOT / 2.0, center.1 - DOT / 2.0), (DOT, DOT))?;
            rounded(&visual.cast()?, (DOT, DOT), DOT / 2.0)?;
            visual.SetOpacity(0.0)?;
            Ok(visual)
        };
        let compact_dot = dot((COMPACT.0 + PRIVACY_EXTRA - COMPACT.1 / 2.0, COMPACT.1 / 2.0))?;
        compact.Children()?.InsertAtTop(&compact_dot)?;
        let claude_line = container(at(0.0, 0.0), COMPACT)?;
        let mark_surface = painter.surface((MARK, MARK))?;
        let mark = sprite(&surface_brush(&mark_surface)?, at((COMPACT.1 - MARK) / 2.0, (COMPACT.1 - MARK) / 2.0), (MARK, MARK))?;
        claude_line.Children()?.InsertAtTop(&mark)?;
        let line_surface = painter.surface(LINES)?;
        claude_line.Children()?.InsertAtTop(&sprite(&surface_brush(&line_surface)?, at(TEXT_X, 0.0), LINES)?)?;
        compact.Children()?.InsertAtTop(&claude_line)?;

        let music_line = container(at(0.0, 0.0), COMPACT)?;
        music_line.SetOpacity(0.0)?;
        let compact_tint = sprite(&tint_brush.cast()?, at(0.0, 0.0), COMPACT)?;
        compact_tint.SetOpacity(0.0)?;
        music_line.Children()?.InsertAtTop(&compact_tint)?;
        let (small_art, small_layers) = art_frame(at((COMPACT.1 - SMALL_ART) / 2.0, (COMPACT.1 - SMALL_ART) / 2.0), SMALL_ART, 9.0)?;
        music_line.Children()?.InsertAtTop(&small_art)?;
        let title_surface = painter.surface(LINES)?;
        let title_text = sprite(&surface_brush(&title_surface)?, at(TEXT_X, 0.0), LINES)?;
        translatable(&title_text.cast()?)?;
        music_line.Children()?.InsertAtTop(&title_text)?;
        let mut bars = Vec::new();
        for i in 0..BARS {
            let x = ITEMS_END - BARS_W + i as f32 * (BAR_W + 3.0);
            let bar = sprite(&accent_brush, at(x, (COMPACT.1 - BAR_H) / 2.0), (BAR_W, BAR_H))?;
            // Grow from the bottom.
            bar.SetCenterPoint(Vector3 { X: 0.0, Y: px((0.0, BAR_H)).Y, Z: 0.0 })?;
            bar.SetScale(Vector3 { X: 1.0, Y: bar_height(i, 0), Z: 1.0 })?;
            music_line.Children()?.InsertAtTop(&bar)?;
            bars.push(bar);
        }
        compact.Children()?.InsertAtTop(&music_line)?;
        content.Children()?.InsertAtTop(&compact)?;

        // Peek: one line, with the art when it is about music.
        let peek = layer(corner(PEEK), NOTICE)?;
        peek.SetOpacity(0.0)?;
        translatable(&peek.cast()?)?;
        let (peek_art, peek_layers) = art_frame(at((PEEK.1 - 30.0) / 2.0, (PEEK.1 - 30.0) / 2.0), 30.0, 10.0)?;
        peek_art.SetOpacity(0.0)?;
        peek.Children()?.InsertAtTop(&peek_art)?;
        let peek_surface = painter.surface(NOTICE)?;
        peek.Children()?.InsertAtTop(&sprite(&surface_brush(&peek_surface)?, at(0.0, 0.0), NOTICE)?)?;
        // A notification's icon on its own visual, so it can pop in, centered in the round end,
        // with a ring under it that spreads to that end's curve and fades: "something came".
        let disc = pages::NOTICE_DISC;
        let disc_at = at(NOTICE.1 / 2.0 - disc / 2.0, NOTICE.1 / 2.0 - disc / 2.0);
        let ring = compositor.CreateEllipseGeometry()?;
        let disc_px = px((disc, disc));
        ring.SetCenter(Vector2 { X: disc_px.X / 2.0, Y: disc_px.Y / 2.0 })?;
        ring.SetRadius(Vector2 { X: disc_px.X / 2.0 - scale, Y: disc_px.Y / 2.0 - scale })?;
        let ring_shape = compositor.CreateSpriteShapeWithGeometry(&ring)?;
        ring_shape.SetStrokeBrush(&compositor.CreateColorBrushWithColor(argb(0xC8FFFFFF))?)?;
        ring_shape.SetStrokeThickness(2.0 * scale)?;
        let ripple = compositor.CreateShapeVisual()?;
        ripple.SetSize(disc_px)?;
        ripple.Shapes()?.Append(&ring_shape)?;
        ripple.SetOffset(disc_at)?;
        ripple.SetCenterPoint(Vector3 { X: disc_px.X / 2.0, Y: disc_px.Y / 2.0, Z: 0.0 })?;
        ripple.SetOpacity(0.0)?;
        peek.Children()?.InsertAtTop(&ripple)?;
        let notice_surface = painter.surface((disc, disc))?;
        let notice_icon = sprite(&surface_brush(&notice_surface)?, disc_at, (disc, disc))?;
        notice_icon.SetOpacity(0.0)?;
        peek.Children()?.InsertAtTop(&notice_icon)?;
        content.Children()?.InsertAtTop(&peek)?;

        // Expanded: tab bar with a sliding highlight, and one page per tab.
        let expanded = layer(corner(EXPANDED), EXPANDED)?;
        let expanded_px = px(EXPANDED);
        expanded.SetCenterPoint(Vector3 { X: expanded_px.X / 2.0, Y: expanded_px.Y / 2.0, Z: 0.0 })?;
        expanded.SetOpacity(0.0)?;

        let tint = sprite(&tint_brush.cast()?, at(0.0, 0.0), EXPANDED)?;
        tint.SetOpacity(0.0)?;
        expanded.Children()?.InsertAtTop(&tint)?;

        let capsule = compositor.CreateRoundedRectangleGeometry()?;
        capsule.SetSize(px((TAB_W, TAB_H)))?;
        capsule.SetCornerRadius(px((TAB_H / 2.0, TAB_H / 2.0)))?;
        let capsule_shape = compositor.CreateSpriteShapeWithGeometry(&capsule)?;
        capsule_shape.SetFillBrush(&compositor.CreateColorBrushWithColor(argb(0x2EFFFFFF))?)?;
        let highlight = compositor.CreateShapeVisual()?;
        highlight.SetSize(px((TAB_W, TAB_H)))?;
        highlight.Shapes()?.Append(&capsule_shape)?;
        highlight.SetOffset(at(tab_left(Tab::Music), TAB_Y))?;
        expanded.Children()?.InsertAtTop(&highlight)?;

        let header_surface = painter.surface((EXPANDED.0, PAGE_Y))?;
        painter.draw(&header_surface, |c| {
            for (tab, label) in [(Tab::Music, tr("Müzik", "Music")), (Tab::Claude, "Claude"), (Tab::Shelf, tr("Raf", "Shelf"))] {
                c.text(label, (tab_left(tab), TAB_Y, TAB_W, TAB_H), 12.5, true, TEXT, Align::Center)?;
            }
            c.icon('\u{E713}', GEAR, 13.0, pages::DIM) // Setting
        })?;
        expanded.Children()?.InsertAtTop(&sprite(&surface_brush(&header_surface)?, at(0.0, 0.0), (EXPANDED.0, PAGE_Y))?)?;
        let expanded_dot = dot((EXPANDED.0 - 24.0, TAB_Y + TAB_H / 2.0))?;
        expanded.Children()?.InsertAtTop(&expanded_dot)?;

        let page_surfaces = [painter.surface(PAGE)?, painter.surface(PAGE)?, painter.surface(PAGE)?];
        let pages = [container(at(0.0, PAGE_Y), PAGE)?, container(at(0.0, PAGE_Y), PAGE)?, container(at(0.0, PAGE_Y), PAGE)?];
        pages[1].SetOpacity(0.0)?;
        pages[2].SetOpacity(0.0)?;
        let mut texts = Vec::new();
        for (page, surface) in pages.iter().zip(&page_surfaces) {
            let text = sprite(&surface_brush(surface)?, at(0.0, 0.0), PAGE)?;
            page.Children()?.InsertAtTop(&text)?;
            expanded.Children()?.InsertAtTop(page)?;
            texts.push(text);
        }
        let page_texts: [SpriteVisual; 3] = texts.try_into().map_err(|_| Error::from(windows::Win32::Foundation::E_FAIL))?;
        let music = &pages[Tab::Music.index()];
        let (page_art, page_layers) = art_frame(at(ART.0, ART.1), ART.2, pages::ART_RADIUS)?;
        // A hairline around the art keeps a dark cover from melting into the black pill.
        let edge = compositor.CreateRoundedRectangleGeometry()?;
        edge.SetSize(px((ART.2, ART.2)))?;
        edge.SetCornerRadius(px((pages::ART_RADIUS, pages::ART_RADIUS)))?;
        let edge_shape = compositor.CreateSpriteShapeWithGeometry(&edge)?;
        edge_shape.SetStrokeBrush(&compositor.CreateColorBrushWithColor(argb(0x26FFFFFF))?)?;
        edge_shape.SetStrokeThickness(2.0 * scale)?; // half of it, inside the frame's clip, shows
        let edge_visual = compositor.CreateShapeVisual()?;
        edge_visual.SetSize(px((ART.2, ART.2)))?;
        edge_visual.Shapes()?.Append(&edge_shape)?;
        page_art.Children()?.InsertAtTop(&edge_visual)?;
        music.Children()?.InsertAtTop(&page_art)?;
        let times_surface = painter.surface((TIMES.2, TIMES.3))?;
        let times = sprite(&surface_brush(&times_surface)?, at(TIMES.0, TIMES.1), (TIMES.2, TIMES.3))?;
        music.Children()?.InsertAtTop(&times)?;
        let progress_track = container(at(BAR.0, BAR.1), (BAR.2, BAR.3))?;
        rounded(&progress_track.cast()?, (BAR.2, BAR.3), BAR.3 / 2.0)?;
        let progress = sprite(&accent_brush, at(0.0, 0.0), (BAR.2, BAR.3))?;
        progress.SetCenterPoint(Vector3::default())?;
        progress.SetScale(Vector3 { X: 0.0, Y: 1.0, Z: 1.0 })?;
        progress_track.Children()?.InsertAtTop(&progress)?;
        progress_track.SetOpacity(0.0)?;
        music.Children()?.InsertAtTop(&progress_track)?;
        let button_surfaces: [CompositionDrawingSurface; 6] = (0..6)
            .map(|_| painter.surface((BUTTON, BUTTON)))
            .collect::<Result<Vec<_>>>()?
            .try_into()
            .map_err(|_| Error::from(windows::Win32::Foundation::E_FAIL))?;
        // Previous, play, pause, next, shuffle, repeat; play and pause share the middle.
        let column = [1, 2, 2, 3, 0, 4];
        let mut made = Vec::new();
        for (surface, column) in button_surfaces.iter().zip(column) {
            let button = sprite(&surface_brush(surface)?, at(BUTTON_X[column], BUTTONS_Y), (BUTTON, BUTTON))?;
            music.Children()?.InsertAtTop(&button)?;
            made.push(button);
        }
        let buttons: [SpriteVisual; 6] = made.try_into().map_err(|_| Error::from(windows::Win32::Foundation::E_FAIL))?;
        buttons[2].SetOpacity(0.0)?; // pause shows while playing
        let heart_surface = painter.surface((HEART.2, HEART.2))?;
        let heart = sprite(&surface_brush(&heart_surface)?, at(HEART.0, HEART.1), (HEART.2, HEART.2))?;
        heart.SetOpacity(0.0)?;
        music.Children()?.InsertAtTop(&heart)?;
        content.Children()?.InsertAtTop(&expanded)?;

        // What rises into place as the island opens: on the music page the art and title
        // first, then the progress, the buttons, the volume.
        let visual = |v: &SpriteVisual| -> Result<Visual> { v.cast() };
        let staggered: [Vec<Vec<Visual>>; 3] = [
            vec![
                vec![page_art.cast()?, visual(&page_texts[0])?, visual(&heart)?],
                vec![progress_track.cast()?, visual(&times)?],
                buttons.iter().map(visual).collect::<Result<_>>()?,
            ],
            vec![vec![visual(&page_texts[1])?]],
            vec![vec![visual(&page_texts[2])?]],
        ];
        for item in staggered.iter().flatten().flatten() {
            translatable(item)?;
        }

        let springs = SPRINGS.map(|spec| -> Result<SpringScalarNaturalMotionAnimation> {
            let spring = compositor.CreateSpringScalarAnimation()?;
            spring.SetDampingRatio(spec.damping)?;
            spring.SetPeriod(ms(spec.period_ms as i64))?;
            Ok(spring)
        });
        let [width_spring, height_spring] = springs;
        let blur_factory = blur::factory(&compositor)?;
        let blurs = [blur_factory.CreateBrush()?, blur_factory.CreateBrush()?, blur_factory.CreateBrush()?];
        let easing = compositor.CreateCubicBezierEasingFunction(Vector2 { X: 0.2, Y: 0.0 }, Vector2 { X: 0.0, Y: 1.0 })?;

        let mut island = Self {
            scale,
            window,
            compositor,
            _target: target,
            painter,
            size,
            motion: [Motion::rest(compact_px.X, SPRINGS[0]), Motion::rest(compact_px.Y, SPRINGS[1])],
            springs: [width_spring?, height_spring?],
            easing,
            view: View::Compact,
            full_motion: true,
            hover: false,
            blurs,
            transitions: Arc::new(AtomicUsize::new(0)),
            compact,
            claude_line,
            mark,
            mark_surface,
            line_surface,
            music_line,
            title_surface,
            title_text,
            bars,
            accent,
            glow,
            glow_brushes,
            art_surfaces,
            art_front: 0,
            art_frames: [small_art, peek_art, page_art],
            art_layers: [small_layers, peek_layers, page_layers],
            tint,
            compact_tint,
            tint_stops,
            tinted: false,
            present: false,
            peek,
            peek_surface,
            notice_icon,
            notice_surface,
            ripple,
            expanded,
            highlight,
            pages,
            page_surfaces,
            page_texts,
            staggered,
            times_surface,
            progress_track,
            progress,
            buttons,
            button_surfaces,
            heart,
            heart_surface,
            liked: None,
            players: 0,
            bubble_surface,
            bubble_limit: None,
            bubble_out: false,
            shelf_view: (0, 0, 0),
            privacy_brush,
            privacy_dots: [compact_dot, expanded_dot],
            extra: 0.0,
            peek_h: PEEK.1,
            tab: Tab::Music,
            line: Line::Claude,
            equalizer: false,
            glowing: false,
        };
        island.set_media(None, 0)?;
        island.set_art(None, false)?;
        island.draw_shelf(&shelf::Shelf::default(), 0, [0, 0], false)?;
        Ok(island)
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// The pill's size in a view, DIPs: the closed pill is wider while it shows the privacy dot.
    pub fn size_of(&self, view: View) -> (f32, f32) {
        let (w, h) = match view {
            View::Compact => (COMPACT.0 + self.extra, COMPACT.1),
            View::Peek => (PEEK.0, self.peek_h),
            _ => view.size(),
        };
        if self.hover && self.full_motion && view != View::Expanded {
            (w + HOVER.0, h + HOVER.1)
        } else {
            (w, h)
        }
    }

    /// The mouse came over the pill or left it; true when that changes its size (the window
    /// springs it there with `animate`).
    pub fn set_hover(&mut self, on: bool) -> bool {
        let before = self.size_of(self.view);
        self.hover = on;
        self.size_of(self.view) != before
    }

    /// Off when Windows' animation effects are: transitions keep their springs and fades,
    /// but nothing blurs, rises or grows under the mouse.
    pub fn set_full_motion(&mut self, on: bool) {
        self.full_motion = on;
    }

    /// The microphone/camera dot in `color`, or none. The window springs the pill to its new
    /// width with `animate`.
    pub fn set_privacy(&mut self, color: Option<u32>) -> Result<()> {
        if let Some(color) = color {
            self.privacy_brush.SetColor(argb(color))?;
        }
        self.extra = if color.is_some() { PRIVACY_EXTRA } else { 0.0 };
        for dot in &self.privacy_dots {
            self.crossfade(&dot.cast()?, color.is_some(), 200)?;
        }
        Ok(())
    }

    /// Starts the transition; the returned batch completes when every animation has settled.
    pub fn animate(&mut self, view: View) -> Result<CompositionScopedBatch> {
        let batch = self.compositor.CreateScopedBatch(CompositionBatchTypes::Animation)?;
        let from = std::mem::replace(&mut self.view, view);
        self.place_bubble()?;
        let full = self.full_motion;
        // A spring started mid-flight continues from the value currently on screen; we hand it
        // the velocity it had there.
        let target = self.painter.px(self.size_of(view));
        let grows = target.X * target.Y > self.motion[0].to * self.motion[1].to;
        let now = Instant::now();
        for (i, (property, value)) in [("W", target.X), ("H", target.Y)].into_iter().enumerate() {
            let motion = &mut self.motion[i];
            let (current, velocity) = motion.at(now);
            let settled = velocity.abs() < 1.0 && (current - motion.to).abs() < 0.5;
            // The height follows the width when the pill grows, and leads it when it shrinks.
            let lags = if grows { i == 1 } else { i == 0 };
            let delay = if full && settled && lags && current != value { STAGGER_MS } else { 0 };
            let spring = &self.springs[i];
            spring.SetDelayTime(ms(delay as i64))?;
            spring.SetInitialVelocity(velocity)?;
            spring.SetFinalValue(&PropertyValue::CreateSingle(value)?.cast::<IReference<f32>>()?)?;
            self.size.StartAnimation(&HSTRING::from(property), spring)?;
            *motion = Motion { from: current, to: value, velocity, start: now + Duration::from_millis(delay), spring: SPRINGS[i] };
        }
        let layers = [(&self.compact, View::Compact), (&self.peek, View::Peek), (&self.expanded, View::Expanded)];
        for (i, (layer, shows)) in layers.into_iter().enumerate() {
            let (show, was) = (shows == view, shows == from);
            self.fade(&layer.cast()?, show)?;
            if full && show != was {
                // Comes in from a blur, or goes out into one.
                let blur = self.compositor.CreateScalarKeyFrameAnimation()?;
                let (start, end) = if show { (BLUR * self.scale, 0.0) } else { (0.0, BLUR * self.scale) };
                blur.InsertKeyFrame(0.0, start)?;
                blur.InsertKeyFrameWithEasingFunction(1.0, end, &self.easing)?;
                blur.SetDuration(ms(if show { 300 } else { 150 }))?;
                if show {
                    blur.SetDelayTime(ms(70))?;
                    blur.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
                }
                layer.SetEffect(&self.blurs[i])?;
                self.blurs[i].StartAnimation(h!("Blur.BlurAmount"), &blur)?;
            }
        }
        if view == View::Expanded {
            let grow = self.compositor.CreateVector3KeyFrameAnimation()?;
            grow.InsertKeyFrame(0.0, Vector3 { X: 0.96, Y: 0.96, Z: 1.0 })?;
            grow.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &self.easing)?;
            grow.SetDuration(ms(300))?;
            grow.SetDelayTime(ms(60))?;
            self.expanded.StartAnimation(h!("Scale"), &grow)?;
        }
        if full && view == View::Expanded && from != View::Expanded {
            for (group, items) in self.staggered[self.tab.index()].iter().enumerate() {
                let rise = self.rise(RISE, 320, 70 + 35 * group as i64)?;
                for item in items {
                    item.StartAnimation(h!("Translation.Y"), &rise)?;
                }
            }
        }
        if full && view == View::Peek && from != View::Peek {
            // News drops out of the island's top edge.
            self.peek.StartAnimation(h!("Translation.Y"), &self.rise(-RISE * 0.7, 340, 40)?)?;
        }
        // Once settled, the layers lose their blur: without an effect they cost nothing at rest.
        let mine = self.transitions.fetch_add(1, Ordering::Relaxed) + 1;
        let (latest, layers) = (self.transitions.clone(), [self.compact.clone(), self.peek.clone(), self.expanded.clone()]);
        batch.Completed(&TypedEventHandler::new(move |_, _| {
            if latest.load(Ordering::Relaxed) == mine {
                for layer in &layers {
                    layer.SetEffect(None::<&CompositionEffectBrush>)?;
                }
            }
            Ok(())
        }))?;
        batch.End()?;
        Ok(batch)
    }

    /// From `by` DIPs below (above when negative) to the place, for "Translation.Y".
    fn rise(&self, by: f32, duration: i64, delay: i64) -> Result<ScalarKeyFrameAnimation> {
        let rise = self.compositor.CreateScalarKeyFrameAnimation()?;
        rise.InsertKeyFrame(0.0, by * self.scale)?;
        rise.InsertKeyFrameWithEasingFunction(1.0, 0.0, &self.easing)?;
        rise.SetDuration(ms(duration))?;
        rise.SetDelayTime(ms(delay))?;
        rise.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
        Ok(rise)
    }

    /// Content appears once the pill has made room for it and leaves before it shrinks.
    fn fade(&self, visual: &CompositionObject, show: bool) -> Result<()> {
        let fade = self.compositor.CreateScalarKeyFrameAnimation()?;
        fade.InsertKeyFrameWithEasingFunction(1.0, if show { 1.0 } else { 0.0 }, &self.easing)?;
        fade.SetDuration(ms(if show { 240 } else { 130 }))?;
        if show {
            fade.SetDelayTime(ms(70))?;
        }
        visual.StartAnimation(h!("Opacity"), &fade)
    }

    fn crossfade(&self, visual: &CompositionObject, show: bool, duration: i64) -> Result<()> {
        let fade = self.compositor.CreateScalarKeyFrameAnimation()?;
        fade.InsertKeyFrameWithEasingFunction(1.0, if show { 1.0 } else { 0.0 }, &self.easing)?;
        fade.SetDuration(ms(duration))?;
        visual.StartAnimation(h!("Opacity"), &fade)
    }

    pub fn set_tab(&mut self, tab: Tab) -> Result<()> {
        if tab == self.tab {
            return Ok(());
        }
        let direction = if tab.index() > self.tab.index() { 1.0 } else { -1.0 };
        let shift = 18.0 * self.scale;

        let left = self.painter.px((tab_left(tab), 0.0)).X;
        if self.full_motion {
            // The highlight springs over, overshooting a little.
            let slide = self.compositor.CreateSpringScalarAnimation()?;
            slide.SetDampingRatio(0.82)?;
            slide.SetPeriod(ms(62))?;
            slide.SetFinalValue(&PropertyValue::CreateSingle(left)?.cast::<IReference<f32>>()?)?;
            self.highlight.StartAnimation(h!("Offset.X"), &slide)?;
        } else {
            let slide = self.compositor.CreateScalarKeyFrameAnimation()?;
            slide.InsertKeyFrameWithEasingFunction(1.0, left, &self.easing)?;
            slide.SetDuration(ms(260))?;
            self.highlight.StartAnimation(h!("Offset.X"), &slide)?;
        }

        let leaving = &self.pages[self.tab.index()];
        let out_fade = self.compositor.CreateScalarKeyFrameAnimation()?;
        out_fade.InsertKeyFrame(1.0, 0.0)?;
        out_fade.SetDuration(ms(120))?;
        leaving.StartAnimation(h!("Opacity"), &out_fade)?;
        let out_move = self.compositor.CreateScalarKeyFrameAnimation()?;
        out_move.InsertKeyFrameWithEasingFunction(1.0, -direction * shift, &self.easing)?;
        out_move.SetDuration(ms(160))?;
        leaving.StartAnimation(h!("Offset.X"), &out_move)?;

        let entering = &self.pages[tab.index()];
        let in_fade = self.compositor.CreateScalarKeyFrameAnimation()?;
        in_fade.InsertKeyFrame(0.0, 0.0)?;
        in_fade.InsertKeyFrameWithEasingFunction(1.0, 1.0, &self.easing)?;
        in_fade.SetDuration(ms(200))?;
        in_fade.SetDelayTime(ms(60))?;
        in_fade.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
        entering.StartAnimation(h!("Opacity"), &in_fade)?;
        let in_move = self.compositor.CreateScalarKeyFrameAnimation()?;
        in_move.InsertKeyFrame(0.0, direction * shift)?;
        in_move.InsertKeyFrameWithEasingFunction(1.0, 0.0, &self.easing)?;
        in_move.SetDuration(ms(260))?;
        entering.StartAnimation(h!("Offset.X"), &in_move)?;

        self.tab = tab;
        self.show_tint()
    }

    /// The art's wash shows behind the closed pill's music line and on the music page, when
    /// something plays with art that has color.
    fn show_tint(&self) -> Result<()> {
        let washed = self.present && self.tinted;
        self.crossfade(&self.compact_tint.cast()?, washed, 300)?;
        self.crossfade(&self.tint.cast()?, washed && self.tab == Tab::Music, 300)
    }

    /// A new track: the titles rise into place as they change.
    pub fn new_track(&self) -> Result<()> {
        if !self.full_motion {
            return Ok(());
        }
        let appear = self.compositor.CreateScalarKeyFrameAnimation()?;
        appear.InsertKeyFrame(0.0, 0.0)?;
        appear.InsertKeyFrameWithEasingFunction(1.0, 1.0, &self.easing)?;
        appear.SetDuration(ms(260))?;
        let rise = self.rise(RISE * 0.8, 320, 0)?;
        for text in [&self.title_text, &self.page_texts[Tab::Music.index()]] {
            text.StartAnimation(h!("Opacity"), &appear)?;
            text.StartAnimation(h!("Translation.Y"), &rise)?;
        }
        Ok(())
    }

    /// What a click at a point in window pixels lands on, when expanded.
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let left = (self.window.X - EXPANDED.0 * self.scale) / 2.0;
        let (x, y) = ((x as f32 - left) / self.scale, y as f32 / self.scale - TOP);
        if (TAB_Y - 4.0..=TAB_Y + TAB_H + 4.0).contains(&y) {
            if (GEAR.0..GEAR.0 + GEAR.2).contains(&x) {
                return Some(Hit::Settings);
            }
            let tab = [Tab::Music, Tab::Claude, Tab::Shelf].into_iter().find(|&tab| (tab_left(tab)..tab_left(tab) + TAB_W).contains(&x));
            return tab.map(Hit::Tab);
        }
        let y = y - PAGE_Y;
        let inside = |(bx, by, bw, bh): (f32, f32, f32, f32)| (bx..bx + bw).contains(&x) && (by..by + bh).contains(&y);
        match self.tab {
            Tab::Claude => {
                let row = ((y - pages::SESSIONS_Y) / pages::ROW_H).floor();
                return (y >= pages::SESSIONS_Y && (12.0..PAGE.0 - 12.0).contains(&x)).then_some(Hit::Session(row as usize));
            }
            Tab::Shelf => {
                if let Some(list) = pages::SEGMENTS.iter().position(|&s| inside(s)) {
                    return Some(Hit::Segment(list));
                }
                let (offset, shown, total) = self.shelf_view;
                if total > 0 && inside(pages::SELECT_ALL) {
                    return Some(Hit::SelectAll);
                }
                if total > 0 && inside(pages::SHELF_CLEAR) {
                    return Some(Hit::ClearShelf);
                }
                return (0..shown).find(|&slot| inside(pages::shelf_tile(slot, shown))).map(|slot| Hit::ShelfItem(offset + slot));
            }
            Tab::Music => {}
        }
        if self.liked.is_some() && inside((HEART.0, HEART.1, HEART.2, HEART.2)) {
            return Some(Hit::Like);
        }
        if self.players > 1 && inside(pages::PLAYER_LINE) {
            return Some(Hit::NextPlayer);
        }
        if self.present && inside(pages::NOW_PLAYING) {
            return Some(Hit::ShowPlayer);
        }
        if (BUTTONS_Y..BUTTONS_Y + BUTTON).contains(&y) {
            let column = BUTTON_X.iter().position(|left| (*left..left + BUTTON).contains(&x))?;
            return Some([Hit::Shuffle, Hit::Previous, Hit::PlayPause, Hit::Next, Hit::Repeat][column]);
        }
        if (BAR.1 - 6.0..=BAR.1 + BAR.3 + 6.0).contains(&y) && (BAR.0..=BAR.0 + BAR.2).contains(&x) {
            return Some(Hit::Seek(((x - BAR.0) / BAR.2) as f64));
        }
        None
    }

    /// A pressed button dips and springs back.
    pub fn press(&self, hit: Hit) -> Result<()> {
        let pressed: &[usize] = match hit {
            Hit::Previous => &[0],
            Hit::PlayPause => &[1, 2],
            Hit::Next => &[3],
            Hit::Shuffle => &[4],
            Hit::Repeat => &[5],
            _ => return Ok(()),
        };
        let dip = self.compositor.CreateVector3KeyFrameAnimation()?;
        dip.InsertKeyFrame(0.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 })?;
        dip.InsertKeyFrameWithEasingFunction(0.35, Vector3 { X: 0.82, Y: 0.82, Z: 1.0 }, &self.easing)?;
        dip.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &self.easing)?;
        dip.SetDuration(ms(260))?;
        for &i in pressed {
            self.buttons[i].StartAnimation(h!("Scale"), &dip)?;
        }
        Ok(())
    }

    /// Texts and buttons for what plays (or nothing). Called when the player reports a change.
    pub fn set_media(&mut self, media: Option<&Media>, now_ms: u64) -> Result<()> {
        self.painter.draw(&self.page_surfaces[Tab::Music.index()], |c| pages::music_page(c, media))?;
        // Title over artist; without a title the player's name leads, and without an artist
        // the player's name goes under the title.
        let (title, detail) = match media {
            Some(m) if m.title.is_empty() => (m.app.as_str(), m.artist.as_str()),
            Some(m) if m.artist.is_empty() => (m.title.as_str(), m.app.as_str()),
            Some(m) => (m.title.as_str(), m.artist.as_str()),
            None => ("", ""),
        };
        let w = ITEMS_END - BARS_W - GAP - TEXT_X;
        self.painter.draw(&self.title_surface, |c| pages::two_lines(c, title, detail, w, COMPACT.1))?;
        self.draw_times(media, now_ms)?;
        let enabled = |f: fn(&Media) -> bool| media.is_some_and(f);
        let glyphs = [
            (pages::PREVIOUS, enabled(|m| m.can_previous)),
            (pages::PLAY, enabled(|m| m.can_toggle)),
            (pages::PAUSE, enabled(|m| m.can_toggle)),
            (pages::NEXT, enabled(|m| m.can_next)),
        ];
        for (surface, (glyph, on)) in self.button_surfaces.iter().zip(glyphs) {
            self.painter.draw(surface, |c| pages::button(c, glyph, on))?;
        }
        let (shuffle, repeat) = (media.and_then(|m| m.shuffle), media.and_then(|m| m.repeat));
        if let Some(on) = shuffle {
            self.painter.draw(&self.button_surfaces[4], |c| pages::toggle(c, pages::SHUFFLE, on))?;
        }
        if let Some(mode) = repeat {
            let glyph = if mode == MediaPlaybackAutoRepeatMode::Track { pages::REPEAT_ONE } else { pages::REPEAT_ALL };
            self.painter.draw(&self.button_surfaces[5], |c| pages::toggle(c, glyph, mode != MediaPlaybackAutoRepeatMode::None))?;
        }
        self.buttons[4].SetOpacity(if shuffle.is_some() { 1.0 } else { 0.0 })?;
        self.buttons[5].SetOpacity(if repeat.is_some() { 1.0 } else { 0.0 })?;
        // With nothing playing the page is just a message: no art, no buttons.
        let present = media.is_some();
        let playing = media.is_some_and(|m| m.playing);
        self.art_frames[2].SetOpacity(if present { 1.0 } else { 0.0 })?;
        for i in [0, 3] {
            self.buttons[i].SetOpacity(if present { 1.0 } else { 0.0 })?;
        }
        self.crossfade(&self.buttons[1].cast()?, present && !playing, 140)?;
        self.crossfade(&self.buttons[2].cast()?, playing, 140)?;
        self.present = present;
        self.players = media.map_or(0, |m| m.sessions);
        self.show_tint()
    }

    pub fn draw_times(&self, media: Option<&Media>, now_ms: u64) -> Result<()> {
        self.painter.draw(&self.times_surface, |c| pages::times(c, media, now_ms))
    }

    /// New art: drawn once for every place it shows, into the surface not on screen; the
    /// accent, the glow and the wash follow it. `crossfade` greets a new track: the old art
    /// shrinks away as the new one grows in.
    pub fn set_art(&mut self, art: Option<&Art>, crossfade: bool) -> Result<()> {
        let back = 1 - self.art_front;
        self.painter.draw(&self.art_surfaces[back], |c| pages::art(c, art))?;
        let colored = art.filter(|a| !pages::blank(a));
        let accent = colored.map_or(TEXT, pages::accent);
        self.accent.SetColor(argb(accent))?;
        for brush in &self.glow_brushes {
            brush.SetColor(argb(accent & 0x00FF_FFFF | GLOW_ALPHA << 24))?;
        }
        self.tint_stops[0].SetColor(argb(accent & 0x00FF_FFFF | 0x40 << 24))?;
        self.tint_stops[1].SetColor(argb(accent & 0x00FF_FFFF))?;
        self.tinted = colored.is_some();
        self.show_tint()?;
        let scaled = |from: f32, to: f32, duration: i64| -> Result<Vector3KeyFrameAnimation> {
            let scale = self.compositor.CreateVector3KeyFrameAnimation()?;
            scale.InsertKeyFrame(0.0, Vector3 { X: from, Y: from, Z: 1.0 })?;
            scale.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: to, Y: to, Z: 1.0 }, &self.easing)?;
            scale.SetDuration(ms(duration))?;
            Ok(scale)
        };
        for pair in &self.art_layers {
            let (old, new) = (&pair[self.art_front], &pair[back]);
            if crossfade && self.full_motion {
                let opacity = |to: f32, duration: i64| -> Result<ScalarKeyFrameAnimation> {
                    let fade = self.compositor.CreateScalarKeyFrameAnimation()?;
                    fade.InsertKeyFrameWithEasingFunction(1.0, to, &self.easing)?;
                    fade.SetDuration(ms(duration))?;
                    Ok(fade)
                };
                new.StartAnimation(h!("Opacity"), &opacity(1.0, 260)?)?;
                new.StartAnimation(h!("Scale"), &scaled(1.12, 1.0, 420)?)?;
                old.StartAnimation(h!("Opacity"), &opacity(0.0, 220)?)?;
                old.StartAnimation(h!("Scale"), &scaled(1.0, 0.86, 260)?)?;
            } else {
                for (visual, shown) in [(new, true), (old, false)] {
                    visual.StopAnimation(h!("Opacity"))?;
                    visual.StopAnimation(h!("Scale"))?;
                    visual.SetOpacity(if shown { 1.0 } else { 0.0 })?;
                    visual.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 })?;
                }
            }
        }
        self.art_front = back;
        Ok(())
    }

    /// Where the track is. Set once a second while the music page is open: a continuously
    /// running fill made DWM compose every frame (measured ~5% of the iGPU), a step a second
    /// costs one frame.
    pub fn set_progress(&self, media: Option<&Media>, now_ms: u64) -> Result<()> {
        let Some(m) = media.filter(|m| m.duration > 0.0) else {
            return self.progress_track.SetOpacity(0.0);
        };
        self.progress_track.SetOpacity(1.0)?;
        let fraction = (m.position_at(now_ms) / m.duration).clamp(0.0, 1.0) as f32;
        self.progress.SetScale(Vector3 { X: fraction, Y: 1.0, Z: 1.0 })
    }

    /// The heart for the song on the music page: liked or not, or none when nothing plays.
    /// Liking pops it.
    pub fn draw_like(&mut self, liked: Option<bool>) -> Result<()> {
        if liked == self.liked {
            return Ok(());
        }
        if let Some(on) = liked {
            self.painter.draw(&self.heart_surface, |c| pages::heart(c, on))?;
        }
        self.heart.SetOpacity(if liked.is_some() { 1.0 } else { 0.0 })?;
        if liked == Some(true) && self.liked == Some(false) {
            let pop = self.compositor.CreateVector3KeyFrameAnimation()?;
            pop.InsertKeyFrame(0.0, Vector3 { X: 0.7, Y: 0.7, Z: 1.0 })?;
            pop.InsertKeyFrameWithEasingFunction(0.5, Vector3 { X: 1.25, Y: 1.25, Z: 1.0 }, &self.easing)?;
            pop.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &self.easing)?;
            pop.SetDuration(ms(340))?;
            self.heart.StartAnimation(h!("Scale"), &pop)?;
        }
        self.liked = liked;
        Ok(())
    }

    /// Which line the closed island shows. `moving` (music playing, island closed and on
    /// screen) keeps the equalizer going; see `bar_heights`. `glow` allows the music glow.
    pub fn set_line(&mut self, line: Line, moving: bool, glow: bool) -> Result<()> {
        if line != self.line {
            self.line = line;
            self.crossfade(&self.claude_line.cast()?, line == Line::Claude, 180)?;
            self.crossfade(&self.music_line.cast()?, line == Line::Music, 180)?;
            self.place_bubble()?;
        }
        let glowing = glow && line == Line::Music;
        if glowing != self.glowing {
            self.glowing = glowing;
            self.crossfade(&self.glow.cast()?, glowing, 400)?;
        }
        if moving == self.equalizer {
            return Ok(());
        }
        self.equalizer = moving;
        // Steps, not a smooth motion: DWM draws a frame only when a value changes, so 6
        // steps a second cost ~0.4% of the iGPU while smooth bars cost ~5% (measured).
        let step = self.compositor.CreateStepEasingFunction()?;
        for (i, bar) in self.bars.iter().enumerate() {
            if !moving {
                bar.StopAnimation(h!("Scale.Y"))?;
                bar.SetScale(Vector3 { X: 1.0, Y: bar_height(i, 0), Z: 1.0 })?;
                continue;
            }
            let wave = self.compositor.CreateScalarKeyFrameAnimation()?;
            wave.InsertKeyFrame(0.0, bar_height(i, 0))?;
            for k in 1..=EQ_STEPS {
                wave.InsertKeyFrameWithEasingFunction(k as f32 / EQ_STEPS as f32, bar_height(i, k % EQ_STEPS), &step)?;
            }
            wave.SetDuration(ms(EQ_STEPS as i64 * 1000 / 6))?;
            wave.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
            bar.StartAnimation(h!("Scale.Y"), &wave)?;
        }
        Ok(())
    }

    /// The shelf page with `shelf`, the list picked by `list` (0 files, 1 screenshots);
    /// `counts` are both lists' sizes, `dropping` while files are dragged over the island.
    pub fn draw_shelf(&mut self, shelf: &shelf::Shelf, list: usize, counts: [usize; 2], dropping: bool) -> Result<()> {
        self.shelf_view = (shelf.offset, shelf.visible().count(), shelf.items.len());
        self.painter.draw(&self.page_surfaces[Tab::Shelf.index()], |c| pages::shelf_page(c, shelf, list, counts, dropping))
    }

    pub fn draw_claude(&self, sessions: &[Session], limits: &Limits, now: u64) -> Result<()> {
        self.painter.draw(&self.page_surfaces[Tab::Claude.index()], |c| pages::claude_page(c, sessions, limits, now))
    }

    /// One line of news, led by a dot of that color, or by the art when `dot` is None.
    pub fn draw_peek(&mut self, text: &str, dot: Option<u32>) -> Result<()> {
        self.peek_h = PEEK.1;
        self.hide_notice()?;
        self.art_frames[1].SetOpacity(if dot.is_none() { 1.0 } else { 0.0 })?;
        self.painter.draw(&self.peek_surface, |c| {
            if let Some(color) = dot {
                c.circle((PEEK.1 / 2.0, PEEK.1 / 2.0), 4.0, color)?;
            }
            let x = if dot.is_none() { 48.0 } else { 36.0 };
            c.text(text, (x, 0.0, PEEK.0 - x - 16.0, PEEK.1), 13.0, true, TEXT, Align::Left)
        })
    }

    pub fn draw_volume(&mut self, level: u32, muted: bool) -> Result<()> {
        self.peek_h = PEEK.1;
        self.hide_notice()?;
        self.art_frames[1].SetOpacity(0.0)?;
        self.painter.draw(&self.peek_surface, |c| pages::volume(c, PEEK, level, muted))
    }

    /// A notification's preview: the sender's icon on a disc in the round end, who sent it,
    /// its title and a line of it.
    pub fn draw_notice(&mut self, notice: &crate::notices::Notice) -> Result<()> {
        self.peek_h = NOTICE.1;
        self.art_frames[1].SetOpacity(0.0)?;
        self.painter.draw(&self.notice_surface, |c| pages::notice_icon(c, notice.icon.as_ref()))?;
        self.painter.draw(&self.peek_surface, |c| pages::notice(c, NOTICE, notice))?;
        if !self.full_motion {
            return self.notice_icon.SetOpacity(1.0);
        }
        // The icon pops in once the pill has made room: small, a little past its size, back.
        let pop = self.compositor.CreateVector3KeyFrameAnimation()?;
        let size = |s: f32| Vector3 { X: s, Y: s, Z: 1.0 };
        pop.InsertKeyFrame(0.0, size(0.4))?;
        pop.InsertKeyFrameWithEasingFunction(0.55, size(1.12), &self.easing)?;
        pop.InsertKeyFrameWithEasingFunction(0.8, size(0.96), &self.easing)?;
        pop.InsertKeyFrameWithEasingFunction(1.0, size(1.0), &self.easing)?;
        pop.SetDuration(ms(560))?;
        pop.SetDelayTime(ms(90))?;
        pop.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
        self.notice_icon.StartAnimation(h!("Scale"), &pop)?;
        let appear = self.compositor.CreateScalarKeyFrameAnimation()?;
        appear.InsertKeyFrame(0.0, 0.0)?;
        appear.InsertKeyFrameWithEasingFunction(1.0, 1.0, &self.easing)?;
        appear.SetDuration(ms(220))?;
        appear.SetDelayTime(ms(90))?;
        appear.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
        self.notice_icon.StartAnimation(h!("Opacity"), &appear)?;
        // Twice, a ring spreads from the disc to the round end's curve and fades.
        let reach = (NOTICE.1 - 2.0) / pages::NOTICE_DISC;
        let spread = self.compositor.CreateVector3KeyFrameAnimation()?;
        spread.InsertKeyFrame(0.0, size(1.0))?;
        spread.InsertKeyFrameWithEasingFunction(1.0, size(reach), &self.easing)?;
        let fade = self.compositor.CreateScalarKeyFrameAnimation()?;
        fade.InsertKeyFrame(0.0, 0.85)?;
        fade.InsertKeyFrameWithEasingFunction(1.0, 0.0, &self.easing)?;
        for wave in [&spread.cast::<KeyFrameAnimation>()?, &fade.cast()?] {
            wave.SetDuration(ms(900))?;
            wave.SetDelayTime(ms(280))?;
            wave.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
            wave.SetIterationBehavior(AnimationIterationBehavior::Count)?;
            wave.SetIterationCount(2)?;
        }
        self.ripple.StartAnimation(h!("Scale"), &spread)?;
        self.ripple.StartAnimation(h!("Opacity"), &fade)?;
        if notice.icon.is_none() {
            // The bell rings.
            let ring = self.compositor.CreateScalarKeyFrameAnimation()?;
            for (at, angle) in [(0.0, 0.0), (0.15, -16.0), (0.35, 13.0), (0.55, -9.0), (0.75, 5.0), (1.0, 0.0)] {
                ring.InsertKeyFrameWithEasingFunction(at, angle, &self.easing)?;
            }
            ring.SetDuration(ms(760))?;
            ring.SetDelayTime(ms(260))?;
            self.notice_icon.StartAnimation(h!("RotationAngleInDegrees"), &ring)?;
        }
        Ok(())
    }

    /// Other peeks have no notification icon.
    fn hide_notice(&self) -> Result<()> {
        for visual in [self.notice_icon.cast::<Visual>()?, self.ripple.cast()?] {
            visual.StopAnimation(h!("Opacity"))?;
            visual.SetOpacity(0.0)?;
        }
        Ok(())
    }

    /// The Claude line: the mark in `mark_color`, a status over what it is about, and the
    /// ring of the 5-hour limit (percent left, its color).
    pub fn draw_compact(&self, status: &str, detail: &str, limit: Option<(f32, u32)>, mark_color: u32) -> Result<()> {
        self.painter.draw(&self.mark_surface, |c| c.spark((MARK / 2.0, MARK / 2.0), MARK / 2.0, mark_color))?;
        // In the surface's coordinates, which start at TEXT_X.
        let end = ITEMS_END - TEXT_X;
        self.painter.draw(&self.line_surface, |c| {
            let text_w = if limit.is_some() { end - pages::RING - GAP } else { end };
            pages::two_lines(c, status, detail, text_w, COMPACT.1)?;
            match limit {
                Some((left, color)) => pages::limit_ring(c, (end - pages::RING / 2.0, COMPACT.1 / 2.0), left, color),
                None => Ok(()),
            }
        })
    }

    /// The 5-hour limit for the bubble (used percent and its color), drawn when it changes.
    pub fn set_bubble(&mut self, limit: Option<(f32, u32)>) -> Result<()> {
        if limit != self.bubble_limit {
            if let Some((used, color)) = limit {
                self.painter.draw(&self.bubble_surface, |c| pages::limit_ring(c, (BUBBLE / 2.0, BUBBLE / 2.0), used, color))?;
            }
            self.bubble_limit = limit;
        }
        self.place_bubble()
    }

    /// The bubble comes out beside the closed pill while it shows music and there is a limit
    /// to show, and goes back into it otherwise (as the pill opens, it takes the bubble in).
    fn place_bubble(&mut self) -> Result<()> {
        let out = self.bubble_limit.is_some() && self.line == Line::Music && self.view == View::Compact;
        if out == self.bubble_out {
            return Ok(());
        }
        self.bubble_out = out;
        let spring = self.compositor.CreateSpringScalarAnimation()?;
        spring.SetDampingRatio(0.72)?;
        spring.SetPeriod(ms(62))?;
        spring.SetFinalValue(&PropertyValue::CreateSingle(if out { 1.0 } else { 0.0 })?.cast::<IReference<f32>>()?)?;
        if out {
            spring.SetDelayTime(ms(160))?; // once the pill has settled
        }
        self.size.StartAnimation(h!("S"), &spring)
    }

    /// DIPs the bubble takes right of the pill, for the window's clickable area.
    pub fn bubble_extent(&self) -> f32 {
        if self.bubble_out {
            BUBBLE_GAP + BUBBLE
        } else {
            0.0
        }
    }

    /// Whether a point in window pixels is on the bubble.
    pub fn on_bubble(&self, x: i32, y: i32) -> bool {
        let left = (self.window.X + self.motion[0].to) / 2.0 + BUBBLE_GAP * self.scale;
        let (x, y) = (x as f32, y as f32 / self.scale - TOP);
        self.bubble_out && (left..left + BUBBLE * self.scale).contains(&x) && (0.0..BUBBLE).contains(&y)
    }

    /// Three beats of the mark when a session starts waiting on the user.
    pub fn pulse_mark(&self) -> Result<()> {
        let beat = self.compositor.CreateVector3KeyFrameAnimation()?;
        beat.InsertKeyFrame(0.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 })?;
        beat.InsertKeyFrameWithEasingFunction(0.4, Vector3 { X: 1.35, Y: 1.35, Z: 1.0 }, &self.easing)?;
        beat.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &self.easing)?;
        beat.SetDuration(ms(650))?;
        beat.SetIterationBehavior(AnimationIterationBehavior::Count)?;
        beat.SetIterationCount(3)?;
        self.mark.StartAnimation(h!("Scale"), &beat)
    }
}

/// Steps in one loop of the equalizer (at 6 a second: a 4 s loop, long enough not to look
/// like a loop).
const EQ_STEPS: usize = 24;

/// A gentle wave: each bar its own speed and phase, heights between 0.35 and 0.9.
fn bar_height(bar: usize, step: usize) -> f32 {
    let t = step as f32 / EQ_STEPS as f32 * std::f32::consts::TAU;
    let speed = [2.0, 3.0, 5.0, 4.0][bar % 4];
    let phase = bar as f32 * 1.7;
    0.625 + 0.2 * (speed * t + phase).sin() + 0.075 * (7.0 * t + phase * 2.0).sin()
}

fn tab_left(tab: Tab) -> f32 {
    EXPANDED.0 / 2.0 + (tab.index() as f32 - 1.5) * TAB_W
}

/// A NaturalMotion spring. Damping stays below 1: Motion models an underdamped spring.
#[derive(Clone, Copy)]
struct Spring {
    damping: f32,
    period_ms: f32,
}

/// Mirrors a compositor spring so a retargeted animation keeps its current velocity;
/// Composition restarts a spring from rest otherwise, which shows as a hard stop.
#[derive(Clone, Copy)]
struct Motion {
    from: f32,
    to: f32,
    velocity: f32, // units per second
    start: Instant, // after any delay; the value holds still until then
    spring: Spring,
}

impl Motion {
    fn rest(value: f32, spring: Spring) -> Self {
        Self { from: value, to: value, velocity: 0.0, start: Instant::now(), spring }
    }

    /// Value and velocity at `now`. Measured on Windows 11: a NaturalMotion spring has
    /// natural frequency 1 / Period (not 2π / Period).
    fn at(&self, now: Instant) -> (f32, f32) {
        self.after(now.saturating_duration_since(self.start).as_secs_f32())
    }

    /// Value and velocity `t` seconds after the start.
    fn after(&self, t: f32) -> (f32, f32) {
        let Spring { damping, period_ms } = self.spring;
        let w0 = 1000.0 / period_ms;
        let a = damping * w0;
        let wd = w0 * (1.0 - damping * damping).sqrt();
        let (d0, v0) = (self.from - self.to, self.velocity);
        let e = (-a * t).exp();
        let (sin, cos) = (wd * t).sin_cos();
        let d = e * (d0 * cos + (v0 + a * d0) / wd * sin);
        let v = e * (v0 * cos - (a * v0 + w0 * w0 * d0) / wd * sin);
        (self.to + d, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_matches_spring() {
        // The spring the island used when its curve was measured on screen.
        let measured = Spring { damping: 0.78, period_ms: 55.0 };
        let m = Motion { from: 43.0, to: 188.0, velocity: 500.0, start: Instant::now(), spring: measured };
        let (x, v) = m.after(0.0);
        assert!((x - 43.0).abs() < 1e-3 && (v - 500.0).abs() < 1e-2);
        // Velocity is the derivative of the value.
        let h = 1e-4;
        let numeric = (m.after(0.1 + h).0 - m.after(0.1 - h).0) / (2.0 * h);
        assert!((numeric - m.after(0.1).1).abs() < 1.0, "{numeric} vs {}", m.after(0.1).1);
        // Settles on the target.
        let (x, v) = m.after(2.0);
        assert!((x - 188.0).abs() < 0.01 && v.abs() < 0.1);
        // Halfway at ~80 ms from rest, as measured on screen (expand curve 43 -> 188).
        let rest = Motion { velocity: 0.0, ..m };
        let half = rest.after(0.082).0;
        assert!((half - 115.5).abs() < 6.0, "{half}");
        // Before a delayed start the value holds still.
        let later = Motion { start: Instant::now() + Duration::from_secs(1), ..rest };
        assert_eq!(later.at(Instant::now()).0, 43.0);
        // Every spring the island uses is underdamped and settles within half a second.
        for spring in SPRINGS {
            assert!(spring.damping < 1.0);
            let (x, _) = Motion { spring, ..rest }.after(0.5);
            assert!((x - 188.0).abs() < 1.0, "{x}");
        }
    }

    #[test]
    fn tabs_sit_side_by_side_in_the_middle() {
        assert_eq!(tab_left(Tab::Claude) + TAB_W / 2.0, EXPANDED.0 / 2.0);
        assert_eq!(tab_left(Tab::Music) + TAB_W, tab_left(Tab::Claude));
        assert_eq!(tab_left(Tab::Claude) + TAB_W, tab_left(Tab::Shelf));
        // Clear of the privacy dot at the right end of the tab bar, and of the settings button.
        assert!(tab_left(Tab::Shelf) + TAB_W <= EXPANDED.0 - 34.0);
        assert!(GEAR.0 >= tab_left(Tab::Shelf) + TAB_W && GEAR.0 + GEAR.2 <= EXPANDED.0 - 24.0 - DOT / 2.0 - 2.0);
        // The bubble beside the widest closed pill stays in the window.
        let closed = COMPACT.0 + PRIVACY_EXTRA + HOVER.0;
        assert!(WIDEST / 2.0 >= closed / 2.0 + BUBBLE_GAP + BUBBLE);
    }

    #[test]
    fn equalizer_wave_stays_in_range_and_loops() {
        for bar in 0..BARS {
            for step in 0..EQ_STEPS {
                let h = bar_height(bar, step);
                assert!((0.34..=0.91).contains(&h), "bar {bar} step {step}: {h}");
            }
            // Whole periods per loop, so the last step leads smoothly into the first.
            let next_after_last = bar_height(bar, EQ_STEPS);
            assert!((next_after_last - bar_height(bar, 0)).abs() < 1e-4);
        }
    }

    #[test]
    fn closed_pill_leaves_room_for_text() {
        // The Claude line's text next to the ring, the music line's next to the equalizer.
        assert!(ITEMS_END - pages::RING - GAP - TEXT_X >= 150.0, "text squeezed by the ring");
        assert!(ITEMS_END - BARS_W - GAP - TEXT_X >= 150.0, "text squeezed by the equalizer");
        // The privacy dot clears the line's items.
        let dot_left = COMPACT.0 + PRIVACY_EXTRA - COMPACT.1 / 2.0 - DOT / 2.0;
        assert!(dot_left >= ITEMS_END + 4.0);
        // Both ends' items share the center of the round end they sit in.
        assert_eq!(ITEMS_END - pages::RING / 2.0, COMPACT.0 - COMPACT.1 / 2.0);
    }
}

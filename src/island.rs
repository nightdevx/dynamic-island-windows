use std::time::Instant;

use windows::{
    core::*,
    Foundation::{IReference, PropertyValue, TimeSpan},
    Media::MediaPlaybackAutoRepeatMode,
    UI::{Color, Composition::*},
    Win32::{Foundation::HWND, System::WinRT::Composition::ICompositorDesktopInterop},
};
use windows_numerics::{Vector2, Vector3};

use crate::{
    claude::{Limits, Session},
    media::{Art, Media},
    pages::{self, ART, ART_SOURCE, BAR, BUTTON, BUTTONS_Y, BUTTON_X, PAGE, TEXT, TIMES},
    render::{Align, Painter},
};

// Sizes in DIPs; multiplied by the monitor scale at runtime.
pub const TOP: f32 = 8.0; // gap between the screen edge and the pill
const MARGIN: f32 = 16.0; // room inside the window for spring overshoot
pub const COMPACT: (f32, f32) = (290.0, 36.0);
pub const PEEK: (f32, f32) = (330.0, 40.0);
pub const EXPANDED: (f32, f32) = (380.0, PAGE_Y + PAGE.1);
const MAX_RADIUS: f32 = 28.0;
// The music glow's wider stroke; half of it lies outside the pill, inside the hit region's pad.
const GLOW_WIDTH: f32 = 8.0;
const GLOW_ALPHA: u32 = 0x16; // per stroke; where both overlap, near the edge, about 17%
// Must stay below 1: Motion models an underdamped spring.
const DAMPING: f32 = 0.78;
const PERIOD_MS: f32 = 55.0;

// Compact: a lead (Claude's mark, or the art), two lines of text, and the line's own item on
// the right (the 5-hour limit ring, or the equalizer). Both lines' text surfaces span from
// TEXT_X to the pill's end.
const MARK: f32 = 18.0;
const TEXT_X: f32 = 38.0;
const LINES: (f32, f32) = (COMPACT.0 - TEXT_X, COMPACT.1);
const SMALL_ART: f32 = 24.0;
const EDGE: f32 = 14.0; // right padding
const GAP: f32 = 8.0; // between the text and the item after it
const BARS: usize = 4;
const BAR_W: f32 = 3.0;
const BAR_H: f32 = 14.0;
const BARS_W: f32 = BARS as f32 * (BAR_W + 3.0) - 3.0;

/// Where the line's own item ends, from the pill's left.
const ITEMS_END: f32 = COMPACT.0 - EDGE;

// Microphone/camera in use: the closed pill grows this much and shows a dot at its right end.
const PRIVACY_EXTRA: f32 = 18.0;
const DOT: f32 = 7.0;

// Expanded: tab bar on top, the selected page below it.
const TAB_W: f32 = 92.0;
const TAB_H: f32 = 24.0;
const TAB_Y: f32 = 8.0;
const PAGE_Y: f32 = 38.0;

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
    Seek(f64),      // 0..1 of the track
    Session(usize), // row on the Claude page
}

/// Window size in pixels: the largest view plus room for spring overshoot.
pub fn window_size(scale: f32) -> (i32, i32) {
    (
        ((EXPANDED.0 + 2.0 * MARGIN) * scale).ceil() as i32,
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
    spring: SpringScalarNaturalMotionAnimation,
    easing: CubicBezierEasingFunction,
    compact: ContainerVisual,
    claude_line: ContainerVisual,
    mark: SpriteVisual,
    mark_surface: CompositionDrawingSurface,
    line_surface: CompositionDrawingSurface,
    music_line: ContainerVisual,
    title_surface: CompositionDrawingSurface,
    bars: Vec<SpriteVisual>,
    accent: CompositionColorBrush, // equalizer and progress, from the art
    glow: ShapeVisual,
    glow_brushes: [CompositionColorBrush; 2], // the two strokes, same faint color
    art_surface: CompositionDrawingSurface,
    arts: Vec<SpriteVisual>, // every place the art shows, for the bounce on a new track
    peek: ContainerVisual,
    peek_art: SpriteVisual,
    peek_surface: CompositionDrawingSurface,
    expanded: ContainerVisual,
    highlight: ShapeVisual,
    pages: [ContainerVisual; 2],
    page_surfaces: [CompositionDrawingSurface; 2],
    times_surface: CompositionDrawingSurface,
    progress_track: ContainerVisual,
    progress: SpriteVisual,
    buttons: [SpriteVisual; 6], // previous, play, pause, next, shuffle, repeat
    button_surfaces: [CompositionDrawingSurface; 6],
    privacy_brush: CompositionColorBrush,
    privacy_dots: [SpriteVisual; 2], // closed pill, tab bar
    extra: f32,                      // width the closed pill gives the privacy dot, DIPs
    tab: Tab,
    line: Line,
    equalizer: bool,
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

        let content = compositor.CreateContainerVisual()?;
        content.SetSize(window)?;
        content.SetClip(&compositor.CreateGeometricClipWithGeometry(&geometry)?)?;
        root.Children()?.InsertAtTop(&content)?;

        // The art is drawn once and scaled by every visual that shows it.
        let art_surface = painter.surface((ART_SOURCE, ART_SOURCE))?;
        let art_brush = compositor.CreateSurfaceBrushWithSurface(&art_surface)?;
        art_brush.SetStretch(CompositionStretch::UniformToFill)?;
        let art_brush: CompositionBrush = art_brush.cast()?;
        let accent = compositor.CreateColorBrushWithColor(argb(TEXT))?;
        let accent_brush: CompositionBrush = accent.cast()?;
        let art_sprite = |offset: Vector3, side: f32, radius: f32| -> Result<SpriteVisual> {
            let visual = sprite(&art_brush, offset, (side, side))?;
            rounded(&visual.cast()?, (side, side), radius)?;
            Ok(visual)
        };

        // Compact. Its content keeps to the pill's left edge while the pill grows for the
        // privacy dot, but not beyond that width: on the way to a bigger view it fades in place.
        let compact = container(corner(COMPACT), COMPACT)?;
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
        let mark = sprite(&surface_brush(&mark_surface)?, at(11.0, (COMPACT.1 - MARK) / 2.0), (MARK, MARK))?;
        claude_line.Children()?.InsertAtTop(&mark)?;
        let line_surface = painter.surface(LINES)?;
        claude_line.Children()?.InsertAtTop(&sprite(&surface_brush(&line_surface)?, at(TEXT_X, 0.0), LINES)?)?;
        compact.Children()?.InsertAtTop(&claude_line)?;

        let music_line = container(at(0.0, 0.0), COMPACT)?;
        music_line.SetOpacity(0.0)?;
        let small_art = art_sprite(at(7.0, (COMPACT.1 - SMALL_ART) / 2.0), SMALL_ART, 6.0)?;
        music_line.Children()?.InsertAtTop(&small_art)?;
        let title_surface = painter.surface(LINES)?;
        music_line.Children()?.InsertAtTop(&sprite(&surface_brush(&title_surface)?, at(TEXT_X, 0.0), LINES)?)?;
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
        let peek = container(corner(PEEK), PEEK)?;
        peek.SetOpacity(0.0)?;
        let peek_art = art_sprite(at(8.0, (PEEK.1 - 28.0) / 2.0), 28.0, 7.0)?;
        peek_art.SetOpacity(0.0)?;
        peek.Children()?.InsertAtTop(&peek_art)?;
        let peek_surface = painter.surface(PEEK)?;
        peek.Children()?.InsertAtTop(&sprite(&surface_brush(&peek_surface)?, at(0.0, 0.0), PEEK)?)?;
        content.Children()?.InsertAtTop(&peek)?;

        // Expanded: tab bar with a sliding highlight, and one page per tab.
        let expanded = container(corner(EXPANDED), EXPANDED)?;
        let expanded_px = px(EXPANDED);
        expanded.SetCenterPoint(Vector3 { X: expanded_px.X / 2.0, Y: expanded_px.Y / 2.0, Z: 0.0 })?;
        expanded.SetOpacity(0.0)?;

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
            for (tab, label) in [(Tab::Music, "Müzik"), (Tab::Claude, "Claude")] {
                c.text(label, (tab_left(tab), TAB_Y, TAB_W, TAB_H), 12.5, true, TEXT, Align::Center)?;
            }
            Ok(())
        })?;
        expanded.Children()?.InsertAtTop(&sprite(&surface_brush(&header_surface)?, at(0.0, 0.0), (EXPANDED.0, PAGE_Y))?)?;
        let expanded_dot = dot((EXPANDED.0 - 24.0, TAB_Y + TAB_H / 2.0))?;
        expanded.Children()?.InsertAtTop(&expanded_dot)?;

        let page_surfaces = [painter.surface(PAGE)?, painter.surface(PAGE)?];
        let pages = [container(at(0.0, PAGE_Y), PAGE)?, container(at(0.0, PAGE_Y), PAGE)?];
        pages[1].SetOpacity(0.0)?;
        for (page, surface) in pages.iter().zip(&page_surfaces) {
            page.Children()?.InsertAtTop(&sprite(&surface_brush(surface)?, at(0.0, 0.0), PAGE)?)?;
            expanded.Children()?.InsertAtTop(page)?;
        }
        let music = &pages[Tab::Music.index()];
        let page_art = art_sprite(at(ART.0, ART.1), ART.2, 10.0)?;
        music.Children()?.InsertAtTop(&page_art)?;
        let times_surface = painter.surface((TIMES.2, TIMES.3))?;
        music.Children()?.InsertAtTop(&sprite(&surface_brush(&times_surface)?, at(TIMES.0, TIMES.1), (TIMES.2, TIMES.3))?)?;
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
        content.Children()?.InsertAtTop(&expanded)?;

        let spring = compositor.CreateSpringScalarAnimation()?;
        spring.SetDampingRatio(DAMPING)?;
        spring.SetPeriod(ms(PERIOD_MS as i64))?;
        let easing = compositor.CreateCubicBezierEasingFunction(Vector2 { X: 0.2, Y: 0.0 }, Vector2 { X: 0.0, Y: 1.0 })?;

        let island = Self {
            scale,
            window,
            compositor,
            _target: target,
            painter,
            size,
            motion: [Motion::rest(compact_px.X), Motion::rest(compact_px.Y)],
            spring,
            easing,
            compact,
            claude_line,
            mark,
            mark_surface,
            line_surface,
            music_line,
            title_surface,
            bars,
            accent,
            glow,
            glow_brushes,
            art_surface,
            arts: vec![small_art, peek_art.clone(), page_art],
            peek,
            peek_art,
            peek_surface,
            expanded,
            highlight,
            pages,
            page_surfaces,
            times_surface,
            progress_track,
            progress,
            buttons,
            button_surfaces,
            privacy_brush,
            privacy_dots: [compact_dot, expanded_dot],
            extra: 0.0,
            tab: Tab::Music,
            line: Line::Claude,
            equalizer: false,
        };
        island.set_media(None, 0)?;
        island.set_art(None, false)?;
        Ok(island)
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// The pill's size in a view, DIPs: the closed pill is wider while it shows the privacy dot.
    pub fn size_of(&self, view: View) -> (f32, f32) {
        match view {
            View::Compact => (COMPACT.0 + self.extra, COMPACT.1),
            _ => view.size(),
        }
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
        // A spring started mid-flight continues from the value currently on screen;
        // we hand it the velocity it had there.
        let target = self.painter.px(self.size_of(view));
        let now = Instant::now();
        for (motion, (property, value)) in self.motion.iter_mut().zip([("W", target.X), ("H", target.Y)]) {
            let (current, velocity) = motion.at(now.duration_since(motion.start).as_secs_f32());
            self.spring.SetInitialVelocity(velocity)?;
            self.spring.SetFinalValue(&PropertyValue::CreateSingle(value)?.cast::<IReference<f32>>()?)?;
            self.size.StartAnimation(&HSTRING::from(property), &self.spring)?;
            *motion = Motion { from: current, to: value, velocity, start: now };
        }
        self.fade(&self.compact.cast()?, view == View::Compact)?;
        self.fade(&self.peek.cast()?, view == View::Peek)?;
        self.fade(&self.expanded.cast()?, view == View::Expanded)?;
        if view == View::Expanded {
            let grow = self.compositor.CreateVector3KeyFrameAnimation()?;
            grow.InsertKeyFrame(0.0, Vector3 { X: 0.96, Y: 0.96, Z: 1.0 })?;
            grow.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &self.easing)?;
            grow.SetDuration(ms(160))?;
            grow.SetDelayTime(ms(80))?;
            self.expanded.StartAnimation(h!("Scale"), &grow)?;
        }
        batch.End()?;
        Ok(batch)
    }

    /// Content appears once the pill has made room for it and leaves before it shrinks.
    fn fade(&self, visual: &CompositionObject, show: bool) -> Result<()> {
        let fade = self.compositor.CreateScalarKeyFrameAnimation()?;
        fade.InsertKeyFrameWithEasingFunction(1.0, if show { 1.0 } else { 0.0 }, &self.easing)?;
        fade.SetDuration(ms(if show { 160 } else { 100 }))?;
        if show {
            fade.SetDelayTime(ms(80))?;
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
        let direction = if tab == Tab::Claude { 1.0 } else { -1.0 };
        let shift = 18.0 * self.scale;

        let slide = self.compositor.CreateScalarKeyFrameAnimation()?;
        slide.InsertKeyFrameWithEasingFunction(1.0, self.painter.px((tab_left(tab), 0.0)).X, &self.easing)?;
        slide.SetDuration(ms(260))?;
        self.highlight.StartAnimation(h!("Offset.X"), &slide)?;

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
        Ok(())
    }

    /// What a click at a point in window pixels lands on, when expanded.
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let left = (self.window.X - EXPANDED.0 * self.scale) / 2.0;
        let (x, y) = ((x as f32 - left) / self.scale, y as f32 / self.scale - TOP);
        if (TAB_Y - 4.0..=TAB_Y + TAB_H + 4.0).contains(&y) {
            let tab = [Tab::Music, Tab::Claude].into_iter().find(|&tab| (tab_left(tab)..tab_left(tab) + TAB_W).contains(&x));
            return tab.map(Hit::Tab);
        }
        let y = y - PAGE_Y;
        if self.tab == Tab::Claude {
            let row = ((y - pages::SESSIONS_Y) / pages::ROW_H).floor();
            return (y >= pages::SESSIONS_Y && (12.0..PAGE.0 - 12.0).contains(&x)).then_some(Hit::Session(row as usize));
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
    pub fn set_media(&self, media: Option<&Media>, now_ms: u64) -> Result<()> {
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
        self.arts[2].SetOpacity(if present { 1.0 } else { 0.0 })?;
        for i in [0, 3] {
            self.buttons[i].SetOpacity(if present { 1.0 } else { 0.0 })?;
        }
        self.crossfade(&self.buttons[1].cast()?, present && !playing, 140)?;
        self.crossfade(&self.buttons[2].cast()?, playing, 140)
    }

    pub fn draw_times(&self, media: Option<&Media>, now_ms: u64) -> Result<()> {
        self.painter.draw(&self.times_surface, |c| pages::times(c, media, now_ms))
    }

    /// New art: drawn once for every place it shows; the accent follows it. `bounce` greets
    /// a new track.
    pub fn set_art(&self, art: Option<&Art>, bounce: bool) -> Result<()> {
        self.painter.draw(&self.art_surface, |c| pages::art(c, art))?;
        let accent = art.filter(|a| !pages::blank(a)).map_or(TEXT, pages::accent);
        self.accent.SetColor(argb(accent))?;
        for brush in &self.glow_brushes {
            brush.SetColor(argb(accent & 0x00FF_FFFF | GLOW_ALPHA << 24))?;
        }
        if bounce {
            let pop = self.compositor.CreateVector3KeyFrameAnimation()?;
            pop.InsertKeyFrame(0.0, Vector3 { X: 0.86, Y: 0.86, Z: 1.0 })?;
            pop.InsertKeyFrameWithEasingFunction(0.55, Vector3 { X: 1.08, Y: 1.08, Z: 1.0 }, &self.easing)?;
            pop.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &self.easing)?;
            pop.SetDuration(ms(380))?;
            for visual in &self.arts {
                visual.StartAnimation(h!("Scale"), &pop)?;
            }
        }
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

    /// Which line the closed island shows. `moving` (music playing, island closed and on
    /// screen) keeps the equalizer going; see `bar_heights`.
    pub fn set_line(&mut self, line: Line, moving: bool) -> Result<()> {
        if line != self.line {
            self.line = line;
            self.crossfade(&self.claude_line.cast()?, line == Line::Claude, 180)?;
            self.crossfade(&self.music_line.cast()?, line == Line::Music, 180)?;
            self.crossfade(&self.glow.cast()?, line == Line::Music, 400)?;
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

    pub fn draw_claude(&self, sessions: &[Session], limits: &Limits, now: u64) -> Result<()> {
        self.painter.draw(&self.page_surfaces[Tab::Claude.index()], |c| pages::claude_page(c, sessions, limits, now))
    }

    /// One line of news, led by a dot of that color, or by the art when `dot` is None.
    pub fn draw_peek(&self, text: &str, dot: Option<u32>) -> Result<()> {
        self.peek_art.SetOpacity(if dot.is_none() { 1.0 } else { 0.0 })?;
        self.painter.draw(&self.peek_surface, |c| {
            if let Some(color) = dot {
                c.circle((20.0, PEEK.1 / 2.0), 4.0, color)?;
            }
            let x = if dot.is_none() { 44.0 } else { 32.0 };
            c.text(text, (x, 0.0, PEEK.0 - x - 16.0, PEEK.1), 13.0, true, TEXT, Align::Left)
        })
    }

    pub fn draw_volume(&self, level: u32, muted: bool) -> Result<()> {
        self.peek_art.SetOpacity(0.0)?;
        self.painter.draw(&self.peek_surface, |c| pages::volume(c, PEEK, level, muted))
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
    EXPANDED.0 / 2.0 + (tab.index() as f32 - 1.0) * TAB_W
}

/// Mirrors a compositor spring so a retargeted animation keeps its current velocity;
/// Composition restarts a spring from rest otherwise, which shows as a hard stop.
#[derive(Clone, Copy)]
struct Motion {
    from: f32,
    to: f32,
    velocity: f32, // units per second
    start: Instant,
}

impl Motion {
    fn rest(value: f32) -> Self {
        Self { from: value, to: value, velocity: 0.0, start: Instant::now() }
    }

    /// Value and velocity `t` seconds after the start. Measured on Windows 11:
    /// a NaturalMotion spring has natural frequency 1 / Period (not 2π / Period).
    fn at(&self, t: f32) -> (f32, f32) {
        let w0 = 1000.0 / PERIOD_MS;
        let a = DAMPING * w0;
        let wd = w0 * (1.0 - DAMPING * DAMPING).sqrt();
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
        let m = Motion { from: 43.0, to: 188.0, velocity: 500.0, start: Instant::now() };
        let (x, v) = m.at(0.0);
        assert!((x - 43.0).abs() < 1e-3 && (v - 500.0).abs() < 1e-2);
        // Velocity is the derivative of the value.
        let h = 1e-4;
        let numeric = (m.at(0.1 + h).0 - m.at(0.1 - h).0) / (2.0 * h);
        assert!((numeric - m.at(0.1).1).abs() < 1.0, "{numeric} vs {}", m.at(0.1).1);
        // Settles on the target.
        let (x, v) = m.at(2.0);
        assert!((x - 188.0).abs() < 0.01 && v.abs() < 0.1);
        // Halfway at ~80 ms from rest, as measured on screen (expand curve 43 -> 188).
        let rest = Motion { velocity: 0.0, ..m };
        let half = rest.at(0.082).0;
        assert!((half - 115.5).abs() < 6.0, "{half}");
    }

    #[test]
    fn tabs_sit_side_by_side_in_the_middle() {
        assert_eq!(tab_left(Tab::Music) + TAB_W, EXPANDED.0 / 2.0);
        assert_eq!(tab_left(Tab::Claude), EXPANDED.0 / 2.0);
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
    }
}

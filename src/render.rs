use windows::{
    core::*,
    Foundation::{Size, TypedEventHandler},
    Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat},
    UI::Composition::*,
    Win32::{
        Foundation::*,
        Graphics::{
            Direct2D::{Common::*, *},
            Direct3D::*,
            Direct3D11::*,
            DirectWrite::*,
            Dxgi::{Common::DXGI_FORMAT_B8G8R8A8_UNORM, *},
        },
        System::WinRT::Composition::*,
        UI::WindowsAndMessaging::PostMessageW,
    },
};
use windows_numerics::Vector2;

/// D2D device for drawing text and art into composition surfaces.
///
/// WARP (software) on purpose: we only draw when content changes, a few small surfaces,
/// while the compositor runs every animation. Measured: the Intel driver costs 42 MB
/// private and 23 threads, WARP 7 MB; WARP also never wakes either GPU.
fn graphics_device(compositor: &Compositor) -> Result<CompositionGraphicsDevice> {
    unsafe {
        let mut d3d = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_WARP,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT
                | D3D11_CREATE_DEVICE_SINGLETHREADED
                | D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS,
            None,
            D3D11_SDK_VERSION,
            Some(&mut d3d),
            None,
            None,
        )?;
        let dxgi: IDXGIDevice = d3d.ok_or_else(|| Error::from(E_FAIL))?.cast()?;
        let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let d2d = factory.CreateDevice(&dxgi)?;
        compositor.cast::<ICompositorInterop>()?.CreateGraphicsDevice(&d2d)
    }
}

/// Draws text and simple shapes into composition surfaces. Coordinates are DIPs.
pub struct Painter {
    graphics: CompositionGraphicsDevice,
    dwrite: IDWriteFactory,
    scale: f32,
}

impl Painter {
    pub fn new(compositor: &Compositor, scale: f32) -> Result<Self> {
        Ok(Self {
            graphics: graphics_device(compositor)?,
            dwrite: unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? },
            scale,
        })
    }

    /// Posts `message` to `hwnd` when the surfaces lose their content: the rendering device
    /// was replaced (DWM restart, driver update). Everything must be drawn again then.
    pub fn on_device_replaced(&self, hwnd: HWND, message: u32) -> Result<()> {
        let window = hwnd.0 as isize;
        self.graphics.RenderingDeviceReplaced(&TypedEventHandler::new(move |_, _| {
            unsafe { PostMessageW(Some(HWND(window as _)), message, WPARAM(0), LPARAM(0))? };
            Ok(())
        }))?;
        Ok(())
    }

    /// DIPs to whole pixels.
    pub fn px(&self, (w, h): (f32, f32)) -> Vector2 {
        Vector2 { X: (w * self.scale).round(), Y: (h * self.scale).round() }
    }

    pub fn surface(&self, size: (f32, f32)) -> Result<CompositionDrawingSurface> {
        let px = self.px(size);
        self.graphics.CreateDrawingSurface(
            Size { Width: px.X, Height: px.Y },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )
    }

    /// Clears and redraws the whole surface.
    pub fn draw(&self, surface: &CompositionDrawingSurface, paint: impl FnOnce(&Canvas) -> Result<()>) -> Result<()> {
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        unsafe {
            let mut offset = POINT::default();
            let dc: ID2D1DeviceContext = interop.BeginDraw(None, &mut offset)?;
            dc.Clear(Some(&D2D1_COLOR_F::default()));
            // Grayscale: ClearType needs an opaque background, ours is transparent.
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let canvas = Canvas {
                dc,
                dwrite: &self.dwrite,
                scale: self.scale,
                origin: Vector2 { X: offset.x as f32, Y: offset.y as f32 },
            };
            let painted = paint(&canvas);
            interop.EndDraw()?; // even when painting failed, or the surface stays locked
            painted
        }
    }
}

pub enum Align {
    Left,
    Center,
    Right,
}

pub struct Canvas<'a> {
    dc: ID2D1DeviceContext,
    dwrite: &'a IDWriteFactory,
    scale: f32,
    origin: Vector2,
}

impl Canvas<'_> {
    fn rect(&self, (x, y, w, h): (f32, f32, f32, f32)) -> D2D_RECT_F {
        let s = self.scale;
        D2D_RECT_F {
            left: self.origin.X + x * s,
            top: self.origin.Y + y * s,
            right: self.origin.X + (x + w) * s,
            bottom: self.origin.Y + (y + h) * s,
        }
    }

    fn brush(&self, argb: u32) -> Result<ID2D1SolidColorBrush> {
        unsafe { self.dc.CreateSolidColorBrush(&color(argb), None) }
    }

    /// One line, vertically centered in `bounds`, cut with "…" when too long. Titles get the
    /// font's display cut, tighter like Apple's, the rest its text cut.
    pub fn text(&self, text: &str, bounds: (f32, f32, f32, f32), size: f32, bold: bool, argb: u32, align: Align) -> Result<()> {
        let family = if size >= 14.0 { w!("Segoe UI Variable Display") } else { w!("Segoe UI Variable Text") };
        self.line(family, text, bounds, size, bold, argb, align)
    }

    /// A filled polygon with its corners rounded by `round` DIPs, which also widens it by
    /// half of that: Apple's symbols have no sharp corners.
    pub fn polygon(&self, points: &[(f32, f32)], round: f32, argb: u32) -> Result<()> {
        let s = self.scale;
        let at = |(x, y): (f32, f32)| Vector2 { X: self.origin.X + x * s, Y: self.origin.Y + y * s };
        unsafe {
            let factory = self.dc.GetFactory()?;
            let path = factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            sink.BeginFigure(at(points[0]), D2D1_FIGURE_BEGIN_FILLED);
            for &point in &points[1..] {
                sink.AddLine(at(point));
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            let brush = self.brush(argb)?;
            let round_join = D2D1_STROKE_STYLE_PROPERTIES { lineJoin: D2D1_LINE_JOIN_ROUND, ..Default::default() };
            let stroke = factory.CreateStrokeStyle(&round_join, None)?;
            self.dc.FillGeometry(&path, &brush, None);
            self.dc.DrawGeometry(&path, &brush, round * s, &stroke);
        }
        Ok(())
    }

    /// Text that wraps onto as many lines as it needs, from the top of `bounds`.
    pub fn paragraph(&self, text: &str, bounds: (f32, f32, f32, f32), size: f32, argb: u32) -> Result<()> {
        unsafe {
            let format = self.dwrite.CreateTextFormat(
                w!("Segoe UI Variable Text"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size * self.scale,
                w!("tr-TR"),
            )?;
            let wide: Vec<u16> = text.encode_utf16().collect();
            self.dc.DrawText(&wide, &format, &self.rect(bounds), &self.brush(argb)?, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
        Ok(())
    }

    /// Paints with everything outside `bounds` cut off.
    pub fn clipped(&self, bounds: (f32, f32, f32, f32), paint: impl FnOnce(&Self) -> Result<()>) -> Result<()> {
        unsafe { self.dc.PushAxisAlignedClip(&self.rect(bounds), D2D1_ANTIALIAS_MODE_ALIASED) };
        let painted = paint(self);
        unsafe { self.dc.PopAxisAlignedClip() };
        painted
    }

    /// A Segoe Fluent Icons glyph, centered in `bounds`.
    pub fn icon(&self, glyph: char, bounds: (f32, f32, f32, f32), size: f32, argb: u32) -> Result<()> {
        self.line(w!("Segoe Fluent Icons"), &glyph.to_string(), bounds, size, false, argb, Align::Center)
    }

    /// Square premultiplied BGRA pixels, scaled into `bounds`.
    pub fn image(&self, pixels: &[u8], size: u32, bounds: (f32, f32, f32, f32)) -> Result<()> {
        unsafe {
            let properties = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let size = D2D_SIZE_U { width: size, height: size };
            let bitmap = self.dc.CreateBitmap(size, Some(pixels.as_ptr() as *const _), size.width * 4, &properties)?;
            self.dc.DrawBitmap(&bitmap, Some(&self.rect(bounds)), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn line(&self, family: PCWSTR, text: &str, bounds: (f32, f32, f32, f32), size: f32, bold: bool, argb: u32, align: Align) -> Result<()> {
        unsafe {
            let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
            let format = self.dwrite.CreateTextFormat(
                family,
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size * self.scale,
                w!("tr-TR"),
            )?;
            format.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            })?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let ellipsis = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            format.SetTrimming(&trimming, &ellipsis)?;
            let wide: Vec<u16> = text.encode_utf16().collect();
            self.dc.DrawText(
                &wide,
                &format,
                &self.rect(bounds),
                &self.brush(argb)?,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
        Ok(())
    }

    pub fn round_rect(&self, bounds: (f32, f32, f32, f32), radius: f32, argb: u32) -> Result<()> {
        let rounded = D2D1_ROUNDED_RECT { rect: self.rect(bounds), radiusX: radius * self.scale, radiusY: radius * self.scale };
        unsafe { self.dc.FillRoundedRectangle(&rounded, &self.brush(argb)?) };
        Ok(())
    }

    /// A rounded rectangle filled from `from` at its top left to `to` at its bottom right.
    pub fn gradient_rect(&self, bounds: (f32, f32, f32, f32), radius: f32, from: u32, to: u32) -> Result<()> {
        let rect = self.rect(bounds);
        // The device context has a CreateGradientStopCollection of its own, with more arguments.
        let target: &ID2D1RenderTarget = &self.dc;
        unsafe {
            let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: color(from) }, D2D1_GRADIENT_STOP { position: 1.0, color: color(to) }];
            let stops = target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let line = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: Vector2 { X: rect.left, Y: rect.top },
                endPoint: Vector2 { X: rect.right, Y: rect.bottom },
            };
            let brush = target.CreateLinearGradientBrush(&line, None, &stops)?;
            let rounded = D2D1_ROUNDED_RECT { rect, radiusX: radius * self.scale, radiusY: radius * self.scale };
            self.dc.FillRoundedRectangle(&rounded, &brush);
        }
        Ok(())
    }

    /// A rounded rectangle's outline, dashed or solid.
    pub fn outline(&self, bounds: (f32, f32, f32, f32), radius: f32, width: f32, argb: u32, dashed: bool) -> Result<()> {
        let rounded = D2D1_ROUNDED_RECT { rect: self.rect(bounds), radiusX: radius * self.scale, radiusY: radius * self.scale };
        unsafe {
            let style = D2D1_STROKE_STYLE_PROPERTIES {
                dashStyle: if dashed { D2D1_DASH_STYLE_DASH } else { D2D1_DASH_STYLE_SOLID },
                dashCap: D2D1_CAP_STYLE_ROUND,
                ..Default::default()
            };
            let stroke = self.dc.GetFactory()?.CreateStrokeStyle(&style, None)?;
            self.dc.DrawRoundedRectangle(&rounded, &self.brush(argb)?, width * self.scale, &stroke);
        }
        Ok(())
    }

    /// Claude's mark: rounded rays of alternating length around a center.
    pub fn spark(&self, (cx, cy): (f32, f32), radius: f32, argb: u32) -> Result<()> {
        const RAYS: usize = 10;
        let s = self.scale;
        let (cx, cy, r) = (self.origin.X + cx * s, self.origin.Y + cy * s, radius * s);
        unsafe {
            let round = D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                ..Default::default()
            };
            let stroke = self.dc.GetFactory()?.CreateStrokeStyle(&round, None)?;
            let brush = self.brush(argb)?;
            for i in 0..RAYS {
                let angle = i as f32 * std::f32::consts::TAU / RAYS as f32 - std::f32::consts::FRAC_PI_2;
                let (sin, cos) = angle.sin_cos();
                let outer = if i % 2 == 0 { r } else { r * 0.72 };
                let point = |d: f32| Vector2 { X: cx + cos * d, Y: cy + sin * d };
                self.dc.DrawLine(point(r * 0.2), point(outer - r * 0.1), &brush, r * 0.22, &stroke);
            }
        }
        Ok(())
    }

    /// A progress ring: a faint full circle and, over it, an arc from 12 o'clock clockwise
    /// for `fraction` (0..1) of the way round.
    pub fn ring(&self, (cx, cy): (f32, f32), radius: f32, width: f32, fraction: f32, argb: u32, track: u32) -> Result<()> {
        let s = self.scale;
        let (cx, cy, r, width) = (self.origin.X + cx * s, self.origin.Y + cy * s, radius * s, width * s);
        let circle = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
        let fraction = fraction.clamp(0.0, 1.0);
        unsafe {
            self.dc.DrawEllipse(&circle, &self.brush(track)?, width, None);
            if fraction >= 0.999 {
                self.dc.DrawEllipse(&circle, &self.brush(argb)?, width, None);
                return Ok(());
            }
            if fraction <= 0.0 {
                return Ok(());
            }
            let factory = self.dc.GetFactory()?;
            let arc = factory.CreatePathGeometry()?;
            let sink = arc.Open()?;
            let angle = fraction * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            sink.BeginFigure(Vector2 { X: cx, Y: cy - r }, D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: Vector2 { X: cx + r * angle.cos(), Y: cy + r * angle.sin() },
                size: D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if fraction > 0.5 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            let round = D2D1_STROKE_STYLE_PROPERTIES { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, ..Default::default() };
            let stroke = factory.CreateStrokeStyle(&round, None)?;
            self.dc.DrawGeometry(&arc, &self.brush(argb)?, width, &stroke);
        }
        Ok(())
    }

    pub fn circle(&self, (cx, cy): (f32, f32), radius: f32, argb: u32) -> Result<()> {
        let s = self.scale;
        let ellipse = D2D1_ELLIPSE {
            point: Vector2 { X: self.origin.X + cx * s, Y: self.origin.Y + cy * s },
            radiusX: radius * s,
            radiusY: radius * s,
        };
        unsafe { self.dc.FillEllipse(&ellipse, &self.brush(argb)?) };
        Ok(())
    }
}

/// 0xAARRGGBB to D2D.
fn color(argb: u32) -> D2D1_COLOR_F {
    let channel = |shift: u32| ((argb >> shift) & 0xFF) as f32 / 255.0;
    D2D1_COLOR_F { r: channel(16), g: channel(8), b: channel(0), a: channel(24) }
}

/// Tests: paints `size` DIPs at `scale` into a PNG file, to look at a drawing without a window.
#[cfg(test)]
pub fn to_png(size: (f32, f32), scale: f32, path: &std::path::Path, paint: impl FnOnce(&Canvas) -> Result<()>) -> Result<()> {
    use windows::Win32::{
        Graphics::Imaging::*,
        System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, STGM_CREATE, STGM_WRITE},
        UI::Shell::SHCreateStreamOnFileEx,
    };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let (w, h) = ((size.0 * scale).round() as u32, (size.1 * scale).round() as u32);
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let bitmap = wic.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
        let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let properties = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let target = factory.CreateWicBitmapRenderTarget(&bitmap, &properties)?;
        let dc: ID2D1DeviceContext = target.cast()?;
        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        dc.BeginDraw();
        dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let painted = paint(&Canvas { dc: dc.clone(), dwrite: &dwrite, scale, origin: Vector2::default() });
        dc.EndDraw(None, None)?;
        painted?;
        let stream = SHCreateStreamOnFileEx(&HSTRING::from(path), (STGM_CREATE | STGM_WRITE).0, 0, true, None)?;
        let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(|| Error::from(E_FAIL))?;
        frame.Initialize(None)?;
        frame.WriteSource(&bitmap, std::ptr::null())?;
        frame.Commit()?;
        encoder.Commit()
    }
}

//! The shelf: files dropped on the island wait there until they are dragged out somewhere
//! else. It holds where the files are, not copies, and forgets a file that is gone.

use std::{cell::RefCell, path::PathBuf};

use windows::{
    core::*,
    Win32::{
        Foundation::{HWND, LPARAM, POINTL, SIZE, WPARAM},
        Graphics::Gdi::*,
        System::{
            Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL},
            Ole::*,
            SystemServices::MODIFIERKEYS_FLAGS,
        },
        UI::{Shell::*, WindowsAndMessaging::PostMessageW},
    },
};

use crate::media::Art;

pub struct Item {
    pub path: PathBuf,
    pub name: String,
    pub icon: Option<Art>, // square, premultiplied BGRA
}

#[derive(Default)]
pub struct Shelf {
    pub items: Vec<Item>,
}

impl Shelf {
    /// Adds files not on the shelf yet, with their thumbnail or icon at `px` pixels.
    pub fn add(&mut self, paths: Vec<PathBuf>, px: u32) {
        for path in paths {
            if self.items.iter().any(|item| item.path == path) {
                continue;
            }
            let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
            let icon = thumbnail(&path, px);
            self.items.push(Item { path, name, icon });
        }
    }

    /// Forgets files that were moved, renamed or deleted since. True when any went.
    pub fn prune(&mut self) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.path.exists());
        self.items.len() != before
    }
}

/// The file's thumbnail, else its icon. A thumbnail Windows has to make is made only for
/// pictures of reasonable size: for a large video it can take seconds.
fn thumbnail(path: &std::path::Path, px: u32) -> Option<Art> {
    const PICTURES: [&str; 7] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "heic"];
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let small = std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() < 30 * 1024 * 1024);
    let make = small && PICTURES.contains(&extension.as_str());
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
        let factory: IShellItemImageFactory = item.cast().ok()?;
        let size = SIZE { cx: px as i32, cy: px as i32 };
        let first = if make { SIIGBF_RESIZETOFIT } else { SIIGBF_RESIZETOFIT | SIIGBF_INCACHEONLY };
        let bitmap = factory.GetImage(size, first).or_else(|_| factory.GetImage(size, SIIGBF_ICONONLY)).ok()?;
        let art = square_pixels(bitmap, px);
        let _ = DeleteObject(bitmap.into());
        art
    }
}

/// The bitmap's pixels, centered in a `px` square (thumbnails keep their aspect ratio).
unsafe fn square_pixels(bitmap: HBITMAP, px: u32) -> Option<Art> {
    let mut info = BITMAP::default();
    if unsafe { GetObjectW(bitmap.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut info as *mut _ as *mut _)) } == 0 {
        return None;
    }
    let (w, h) = (info.bmWidth as usize, info.bmHeight as usize);
    if w == 0 || h == 0 || w > px as usize || h > px as usize {
        return None;
    }
    let mut header = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut rows = vec![0u8; w * h * 4];
    let dc = unsafe { GetDC(None) };
    let copied = unsafe { GetDIBits(dc, bitmap, 0, h as u32, Some(rows.as_mut_ptr().cast()), &mut header, DIB_RGB_COLORS) };
    unsafe { ReleaseDC(None, dc) };
    if copied == 0 {
        return None;
    }
    let side = px as usize;
    let mut pixels = vec![0u8; side * side * 4];
    let (left, top) = ((side - w) / 2, (side - h) / 2);
    for y in 0..h {
        let from = y * w * 4;
        let to = ((top + y) * side + left) * 4;
        pixels[to..to + w * 4].copy_from_slice(&rows[from..from + w * 4]);
    }
    Some(Art { size: px, pixels })
}

/// Lets the user drag a file from the shelf to wherever they like; the shell draws the drag
/// image. Copy or link only: dropping never moves the original away. True when it landed.
pub fn drag_out(hwnd: HWND, path: &std::path::Path) -> bool {
    unsafe {
        let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(path), None) else { return false };
        let Ok(data) = item.BindToHandler::<_, IDataObject>(None, &BHID_DataObject) else { return false };
        OUTGOING.with(|o| o.set(true));
        let effect = SHDoDragDrop(Some(hwnd), &data, None::<&IDropSource>, DROPEFFECT_COPY | DROPEFFECT_LINK);
        OUTGOING.with(|o| o.set(false));
        effect.is_ok_and(|e| e != DROPEFFECT_NONE)
    }
}

thread_local! {
    // Files from the last drop, handed to the window with its message.
    static DROPPED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    // A file from the shelf is being dragged: dropping it back on the island is no drop.
    static OUTGOING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Takes the files of the last drop.
pub fn take_dropped() -> Vec<PathBuf> {
    DROPPED.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

/// The paths in a drag of files from Explorer, the desktop, a browser's downloads…
fn paths(data: &IDataObject) -> Vec<PathBuf> {
    let format = FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    let mut found = Vec::new();
    unsafe {
        let Ok(mut medium) = data.GetData(&format) else { return found };
        let drop = HDROP(medium.u.hGlobal.0);
        for i in 0..DragQueryFileW(drop, u32::MAX, None) {
            let mut buffer = vec![0u16; DragQueryFileW(drop, i, None) as usize + 1];
            let len = DragQueryFileW(drop, i, Some(&mut buffer)) as usize;
            found.push(PathBuf::from(String::from_utf16_lossy(&buffer[..len])));
        }
        ReleaseStgMedium(&mut medium);
    }
    found
}

/// Takes files dragged over the island. Posts `message` with WPARAM 1 when files come over
/// it, 0 when they leave, 2 when they are dropped (`take_dropped` has them).
#[implement(IDropTarget)]
pub struct DropTarget {
    pub window: isize,
    pub message: u32,
    files: std::cell::Cell<bool>,
}

impl DropTarget {
    pub fn new(hwnd: HWND, message: u32) -> Self {
        Self { window: hwnd.0 as isize, message, files: std::cell::Cell::new(false) }
    }

    fn post(&self, what: usize) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.window as _)), self.message, WPARAM(what), LPARAM(0));
        }
    }

    fn effect(&self, out: *mut DROPEFFECT) {
        if let Some(out) = unsafe { out.as_mut() } {
            *out = if self.files.get() { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        }
    }
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let format = FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        // S_FALSE also counts as success; only S_OK means the files are there.
        let files = !OUTGOING.with(|o| o.get())
            && data.as_ref().is_some_and(|d| unsafe { d.QueryGetData(&format) } == windows::Win32::Foundation::S_OK);
        self.files.set(files);
        self.effect(effect);
        if files {
            self.post(1);
        }
        Ok(())
    }

    fn DragOver(&self, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        self.effect(effect);
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        if self.files.get() {
            self.post(0);
        }
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        self.effect(effect);
        if let Some(data) = data.as_ref() {
            let dropped = paths(data);
            if !dropped.is_empty() {
                DROPPED.with(|d| d.borrow_mut().extend(dropped));
                self.post(2);
                return Ok(());
            }
        }
        self.post(0);
        Ok(())
    }
}

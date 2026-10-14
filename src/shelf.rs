//! The shelf: files dropped on the island, and new screenshots, wait there until they are
//! dragged out somewhere else, one or a selection at a time. It holds where the files are,
//! not copies; it never deletes a file, and forgets one that is gone.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    time::SystemTime,
};

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
    pub icon: Option<Option<Art>>, // square, premultiplied BGRA; loaded when first shown
    pub selected: bool,
}

/// Tiles on screen at once; the wheel scrolls through the rest.
pub const VISIBLE: usize = 5;

#[derive(Default)]
pub struct Shelf {
    pub items: Vec<Item>,
    pub offset: usize, // first item on screen
}

impl Shelf {
    /// Adds files not on the shelf yet, under their file names: at the end, or at the front
    /// (`newest_first`). Returns how many were new.
    pub fn add(&mut self, paths: Vec<PathBuf>, newest_first: bool) -> usize {
        let named = paths.into_iter().map(|path| {
            let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
            (path, name)
        });
        self.add_named(named.collect(), newest_first)
    }

    /// Like `add`, each under the given name.
    pub fn add_named(&mut self, files: Vec<(PathBuf, String)>, newest_first: bool) -> usize {
        let mut added = 0;
        for (path, name) in files {
            if self.items.iter().any(|item| item.path == path) {
                continue;
            }
            let item = Item { path, name, icon: None, selected: false };
            if newest_first {
                self.items.insert(added, item);
            } else {
                self.items.push(item);
            }
            added += 1;
        }
        if newest_first {
            self.offset = 0;
        }
        added
    }

    /// Forgets files that were moved, renamed or deleted since. True when any went.
    pub fn prune(&mut self) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.path.exists());
        self.scroll(0);
        self.items.len() != before
    }

    /// The items on screen, with their index.
    pub fn visible(&self) -> impl Iterator<Item = (usize, &Item)> {
        self.items.iter().enumerate().skip(self.offset).take(VISIBLE)
    }

    /// Loads the thumbnails of what is on screen, at `px` pixels.
    pub fn load_icons(&mut self, px: u32) {
        for item in self.items.iter_mut().skip(self.offset).take(VISIBLE) {
            if item.icon.is_none() {
                item.icon = Some(thumbnail(&item.path, px));
            }
        }
    }

    /// Moves the view by `by` tiles, kept within the list.
    pub fn scroll(&mut self, by: isize) {
        let last = self.items.len().saturating_sub(VISIBLE);
        self.offset = self.offset.saturating_add_signed(by).min(last);
    }

    pub fn selected(&self) -> usize {
        self.items.iter().filter(|item| item.selected).count()
    }

    /// Selects everything, or nothing when everything already is.
    pub fn select_all(&mut self) {
        let all = self.selected() < self.items.len();
        self.items.iter_mut().for_each(|item| item.selected = all);
    }

    /// Takes the selected items off, or all of them when none is selected. Files stay.
    pub fn clear(&mut self) {
        let some = self.selected() > 0;
        self.items.retain(|item| some && !item.selected);
        self.scroll(0);
    }

    /// What a drag from item `i` takes: the selection if `i` is in it, else just `i`.
    pub fn dragged(&self, i: usize) -> Vec<PathBuf> {
        match self.items.get(i) {
            Some(item) if item.selected => self.items.iter().filter(|it| it.selected).map(|it| it.path.clone()).collect(),
            Some(item) => vec![item.path.clone()],
            None => Vec::new(),
        }
    }

    pub fn remove(&mut self, paths: &[PathBuf]) {
        self.items.retain(|item| !paths.contains(&item.path));
        self.scroll(0);
    }
}

/// Where Windows saves screenshots (Win+PrtScn, the Snipping Tool's automatic save).
pub fn screenshots_dir() -> Option<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Screenshots, KF_FLAG_DEFAULT, None).ok()?;
        let dir = PathBuf::from(path.to_string().ok()?);
        windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const _));
        Some(dir)
    }
}

/// Pictures created in `dir` after `since`, newest first, each named by the time it was
/// taken ("14:32"): their file names all start alike. Creation, not modification: OneDrive
/// touching an old screenshot does not make it new.
pub fn screenshots_since(dir: &Path, since: SystemTime) -> Vec<(PathBuf, String)> {
    let mut found: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let picture = path.extension().is_some_and(|e| matches!(e.to_string_lossy().to_lowercase().as_str(), "png" | "jpg" | "jpeg"));
            let made = entry.metadata().ok()?.created().ok()?;
            (picture && made > since).then_some((made, path))
        })
        .collect();
    found.sort_by_key(|(made, _)| std::cmp::Reverse(*made));
    found.into_iter().map(|(made, path)| (path, local_clock(made))).collect()
}

/// "14:32" in local time.
fn local_clock(time: SystemTime) -> String {
    use windows::Win32::{Foundation::FILETIME, System::Time::*};
    // FILETIME counts 100 ns from 1601.
    let ticks = time.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64 / 100) + 116_444_736_000_000_000;
    let utc = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut system, mut local) = (Default::default(), Default::default());
    unsafe {
        if FileTimeToSystemTime(&utc, &mut system).is_err() || SystemTimeToTzSpecificLocalTime(None, &system, &mut local).is_err() {
            return String::new();
        }
    }
    let local: windows::Win32::Foundation::SYSTEMTIME = local;
    format!("{:02}:{:02}", local.wHour, local.wMinute)
}

/// The file's thumbnail, else its icon. A thumbnail Windows has to make is made only for
/// pictures of reasonable size: for a large video it can take seconds.
fn thumbnail(path: &Path, px: u32) -> Option<Art> {
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
pub fn drag_out(hwnd: HWND, paths: &[PathBuf]) -> bool {
    unsafe {
        // One shell item array, so several files travel as one drag.
        let ids: Vec<*mut Common::ITEMIDLIST> = paths.iter().map(|p| ILCreateFromPathW(&HSTRING::from(p.as_path()))).filter(|id| !id.is_null()).collect();
        let data = if ids.len() == paths.len() && !ids.is_empty() {
            let constant: Vec<*const Common::ITEMIDLIST> = ids.iter().map(|id| *id as *const _).collect();
            SHCreateShellItemArrayFromIDLists(&constant).and_then(|items| items.BindToHandler::<_, IDataObject>(None, &BHID_DataObject))
        } else {
            Err(Error::from(windows::Win32::Foundation::E_FAIL))
        };
        for id in ids {
            ILFree(Some(id));
        }
        let Ok(data) = data else { return false };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn shelf(n: usize) -> Shelf {
        let mut shelf = Shelf::default();
        shelf.add((0..n).map(|i| PathBuf::from(format!("C:/f{i}"))).collect(), false);
        shelf
    }

    #[test]
    fn screenshots_are_pictures_made_after_a_time() {
        let dir = std::env::temp_dir().join(format!("island-shots-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.png"), b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let since = SystemTime::now();
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(dir.join("new.png"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let found = screenshots_since(&dir, since);
        std::fs::remove_dir_all(&dir).unwrap();
        let names: Vec<_> = found.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["new.png"]);
        assert_eq!(found[0].1.len(), 5); // "14:32"
    }
    #[test]
    fn selection_drags_and_clears() {
        let mut s = shelf(4);
        assert_eq!(s.dragged(1), vec![PathBuf::from("C:/f1")]); // nothing selected: just that one
        s.items[1].selected = true;
        s.items[3].selected = true;
        assert_eq!(s.dragged(3), vec![PathBuf::from("C:/f1"), PathBuf::from("C:/f3")]); // the selection
        assert_eq!(s.dragged(0), vec![PathBuf::from("C:/f0")]); // pressed outside it: just that one
        s.clear(); // takes off the selection only
        assert_eq!(s.items.len(), 2);
        s.select_all();
        assert_eq!(s.selected(), 2);
        s.select_all(); // all selected: selects none
        assert_eq!(s.selected(), 0);
        s.clear(); // none selected: everything
        assert!(s.items.is_empty());
    }

    #[test]
    fn scrolls_within_the_list_and_newest_come_first() {
        let mut s = shelf(8);
        s.scroll(10);
        assert_eq!(s.offset, 8 - VISIBLE);
        assert_eq!(s.visible().map(|(i, _)| i).collect::<Vec<_>>(), vec![3, 4, 5, 6, 7]);
        s.scroll(-10);
        assert_eq!(s.offset, 0);
        s.scroll(2);
        assert_eq!(s.add(vec![PathBuf::from("C:/new"), PathBuf::from("C:/f0")], true), 1);
        assert_eq!((s.items[0].name.as_str(), s.offset), ("new", 0)); // shown at once
        s.remove(&[PathBuf::from("C:/new")]);
        assert_eq!(s.items.len(), 8);
    }
}
//! Extracts an executable's small icon as straight (non-premultiplied) RGBA.
//!
//! Runs on a worker thread: `SHGetFileInfoW` hits the shell's icon cache and can
//! block, which must never happen on the UI thread.

use super::util::{wide, Handle};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

pub const ICON_SIZE: usize = 16;

/// A decoded icon: `ICON_SIZE * ICON_SIZE` RGBA pixels.
pub struct IconPixels {
    pub key: Arc<str>,
    pub rgba: Vec<u8>,
}

/// Background icon loader. Requests are deduplicated by the caller.
pub struct IconLoader {
    tx: Sender<Arc<str>>,
    rx: Receiver<IconPixels>,
}

impl IconLoader {
    pub fn new() -> Self {
        let (tx, job_rx) = std::sync::mpsc::channel::<Arc<str>>();
        let (result_tx, rx) = std::sync::mpsc::channel::<IconPixels>();

        std::thread::Builder::new()
            .name("aztm-icons".into())
            .spawn(move || {
                while let Ok(path) = job_rx.recv() {
                    if let Some(rgba) = extract(&path) {
                        // A closed receiver just means the app is shutting down.
                        if result_tx.send(IconPixels { key: path, rgba }).is_err() {
                            return;
                        }
                    }
                }
            })
            .expect("spawn icon loader");

        Self { tx, rx }
    }

    pub fn request(&self, path: Arc<str>) {
        let _ = self.tx.send(path);
    }

    /// Non-blocking: yields whatever has finished decoding.
    pub fn drain(&self) -> impl Iterator<Item = IconPixels> + '_ {
        self.rx.try_iter()
    }
}

fn extract(path: &str) -> Option<Vec<u8>> {
    use windows_sys::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON};
    use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;

    let wpath = wide(path);
    let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let res = unsafe {
        SHGetFileInfoW(
            wpath.as_ptr(),
            0,
            &mut info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON,
        )
    };
    if res == 0 || info.hIcon.is_null() {
        return None;
    }

    let out = icon_to_rgba(info.hIcon);
    unsafe { DestroyIcon(info.hIcon) };
    out
}

/// Read an `HICON`'s colour bitmap as straight RGBA.
fn icon_to_rgba(hicon: windows_sys::Win32::UI::WindowsAndMessaging::HICON) -> Option<Vec<u8>> {
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    use windows_sys::Win32::Graphics::Gdi::GetObjectW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

    let mut ii: ICONINFO = unsafe { std::mem::zeroed() };
    if unsafe { GetIconInfo(hicon, &mut ii) } == 0 {
        return None;
    }
    // GetIconInfo hands us ownership of both bitmaps.
    struct Bitmaps(ICONINFO);
    impl Drop for Bitmaps {
        fn drop(&mut self) {
            unsafe {
                if !self.0.hbmColor.is_null() {
                    DeleteObject(self.0.hbmColor.cast());
                }
                if !self.0.hbmMask.is_null() {
                    DeleteObject(self.0.hbmMask.cast());
                }
            }
        }
    }
    let ii = Bitmaps(ii);

    if ii.0.hbmColor.is_null() {
        return None;
    }

    let mut bm: BITMAP = unsafe { std::mem::zeroed() };
    if unsafe {
        GetObjectW(
            ii.0.hbmColor.cast(),
            std::mem::size_of::<BITMAP>() as i32,
            (&mut bm as *mut BITMAP).cast(),
        )
    } == 0
    {
        return None;
    }
    let (w, h) = (bm.bmWidth.max(0) as usize, bm.bmHeight.max(0) as usize);
    if w == 0 || h == 0 || w > 512 || h > 512 {
        return None;
    }

    let hdc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
    if hdc.is_null() {
        return None;
    }
    struct Dc(windows_sys::Win32::Graphics::Gdi::HDC);
    impl Drop for Dc {
        fn drop(&mut self) {
            unsafe { DeleteDC(self.0) };
        }
    }
    let hdc = Dc(hdc);

    let mut bi: BITMAPINFO = unsafe { std::mem::zeroed() };
    bi.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w as i32,
        // Negative height requests a top-down DIB so row 0 is the top row.
        biHeight: -(h as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };

    let mut bgra = vec![0u8; w * h * 4];
    let got = unsafe {
        GetDIBits(
            hdc.0,
            ii.0.hbmColor.cast(),
            0,
            h as u32,
            bgra.as_mut_ptr().cast(),
            &mut bi,
            DIB_RGB_COLORS,
        )
    };
    if got == 0 {
        return None;
    }

    // A 24bpp icon leaves the alpha byte at zero. Detecting that and falling
    // back to the AND mask is what keeps legacy icons from rendering invisible.
    let has_alpha = bgra.chunks_exact(4).any(|p| p[3] != 0);
    let mask = (!has_alpha).then(|| read_mask(&hdc.0, ii.0.hbmMask, w, h)).flatten();

    let mut rgba = vec![0u8; w * h * 4];
    for (i, px) in bgra.chunks_exact(4).enumerate() {
        rgba[i * 4] = px[2];
        rgba[i * 4 + 1] = px[1];
        rgba[i * 4 + 2] = px[0];
        rgba[i * 4 + 3] = if has_alpha {
            px[3]
        } else {
            // In an icon AND mask, a set bit means "transparent".
            match &mask {
                Some(m) if m[i] => 0,
                _ => 255,
            }
        };
    }

    Some(scale_to(&rgba, w, h, ICON_SIZE))
}

fn read_mask(
    hdc: &windows_sys::Win32::Graphics::Gdi::HDC,
    hbm: windows_sys::Win32::Graphics::Gdi::HBITMAP,
    w: usize,
    h: usize,
) -> Option<Vec<bool>> {
    use windows_sys::Win32::Graphics::Gdi::{
        GetDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };

    if hbm.is_null() {
        return None;
    }
    let mut bi: BITMAPINFO = unsafe { std::mem::zeroed() };
    bi.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w as i32,
        biHeight: -(h as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    let mut buf = vec![0u8; w * h * 4];
    let got = unsafe {
        GetDIBits(
            *hdc,
            hbm.cast(),
            0,
            h as u32,
            buf.as_mut_ptr().cast(),
            &mut bi,
            DIB_RGB_COLORS,
        )
    };
    if got == 0 {
        return None;
    }
    // The mask comes back as black (opaque) / white (transparent).
    Some(buf.chunks_exact(4).map(|p| p[0] > 127).collect())
}

/// Box-filter down to `size` x `size`. Icons are already close to 16px, so a
/// simple area average is both fast and good enough.
fn scale_to(rgba: &[u8], w: usize, h: usize, size: usize) -> Vec<u8> {
    if w == size && h == size {
        return rgba.to_vec();
    }
    let mut out = vec![0u8; size * size * 4];
    for y in 0..size {
        let sy0 = y * h / size;
        let sy1 = (((y + 1) * h).div_ceil(size)).min(h).max(sy0 + 1);
        for x in 0..size {
            let sx0 = x * w / size;
            let sx1 = (((x + 1) * w).div_ceil(size)).min(w).max(sx0 + 1);

            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let p = (sy * w + sx) * 4;
                    // Weight colour by alpha so transparent pixels do not
                    // darken the edges of the icon.
                    let a = rgba[p + 3] as u32;
                    acc[0] += rgba[p] as u32 * a;
                    acc[1] += rgba[p + 1] as u32 * a;
                    acc[2] += rgba[p + 2] as u32 * a;
                    acc[3] += a;
                    n += 1;
                }
            }
            let o = (y * size + x) * 4;
            // acc[3] is the summed alpha; zero means the whole cell was
            // transparent, and the output is already zeroed.
            if let Some(weight) = std::num::NonZeroU32::new(acc[3]) {
                let w = weight.get();
                out[o] = (acc[0] / w) as u8;
                out[o + 1] = (acc[1] / w) as u8;
                out[o + 2] = (acc[2] / w) as u8;
                out[o + 3] = (w / n.max(1)) as u8;
            }
        }
    }
    out
}

/// Keeps a `Handle` import alive for future use by callers of this module.
#[allow(dead_code)]
fn _unused(_: Handle) {}

//! Program icons for the process and startup lists: the icon Windows shows
//! for a file (an .exe's own icon, or a shortcut's), as a small PNG.

/// RGBA pixels to PNG.
pub fn png(rgba: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
    }
    Some(out)
}

/// Turns Windows' BGRA rows into RGBA. Old icons carry no alpha at all;
/// their transparency comes from the mask (`mask_bgra`, white = see-through).
pub fn bgra_to_rgba(bgra: &[u8], mask_bgra: Option<&[u8]>) -> Vec<u8> {
    let has_alpha = bgra.chunks_exact(4).any(|p| p[3] != 0);
    let mut out = Vec::with_capacity(bgra.len());
    for (i, p) in bgra.chunks_exact(4).enumerate() {
        let a = if has_alpha {
            p[3]
        } else {
            match mask_bgra.and_then(|m| m.get(i * 4)) {
                Some(&m) if m != 0 => 0,
                _ => 255,
            }
        };
        out.extend_from_slice(&[p[2], p[1], p[0], a]);
    }
    out
}

pub use imp::icon_png;

#[cfg(windows)]
mod imp {
    use super::{bgra_to_rgba, png};
    use crate::monitor::win::wide;
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
    };
    use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

    fn pixels(dc: HDC, bmp: HBITMAP) -> Option<(Vec<u8>, u32, u32)> {
        if bmp.is_null() {
            return None;
        }
        let mut info: BITMAP = unsafe { std::mem::zeroed() };
        // SAFETY: a BITMAP-sized buffer.
        if unsafe { GetObjectW(bmp, std::mem::size_of::<BITMAP>() as i32, (&mut info as *mut BITMAP).cast()) } == 0 {
            return None;
        }
        let (w, h) = (info.bmWidth.max(0) as u32, info.bmHeight.max(0) as u32);
        if w == 0 || h == 0 || w > 256 || h > 256 {
            return None;
        }
        let mut bi: BITMAPINFO = unsafe { std::mem::zeroed() };
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // top-down rows
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { std::mem::zeroed() }
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        // SAFETY: a buffer of w*h 32-bit pixels, as asked for.
        let rows = unsafe { GetDIBits(dc, bmp, 0, h, buf.as_mut_ptr().cast(), &mut bi, DIB_RGB_COLORS) };
        (rows > 0).then_some((buf, w, h))
    }

    /// The icon Windows shows for this file, as a PNG (32 px).
    pub fn icon_png(path: &str) -> Option<Vec<u8>> {
        // SAFETY: the shell needs COM on this thread; a second call is harmless.
        unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        let p = wide(path);
        let mut sfi: SHFILEINFOW = unsafe { std::mem::zeroed() };
        // SAFETY: a valid path and a SHFILEINFOW-sized buffer.
        let ok = unsafe {
            SHGetFileInfoW(p.as_ptr(), 0, &mut sfi, std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON)
        };
        if ok == 0 || sfi.hIcon.is_null() {
            return None;
        }
        let mut ii: ICONINFO = unsafe { std::mem::zeroed() };
        // SAFETY: our own icon; its bitmaps are freed below.
        let got = unsafe { GetIconInfo(sfi.hIcon, &mut ii) } != 0;
        let mut out = None;
        if got {
            // SAFETY: a memory DC for reading the bitmaps, deleted below.
            let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
            if let Some((color, w, h)) = pixels(dc, ii.hbmColor) {
                let mask = pixels(dc, ii.hbmMask).filter(|(_, mw, mh)| *mw == w && *mh == h).map(|m| m.0);
                let rgba = bgra_to_rgba(&color, mask.as_deref());
                out = png(&rgba, w, h);
            }
            unsafe {
                DeleteDC(dc);
                if !ii.hbmColor.is_null() {
                    DeleteObject(ii.hbmColor);
                }
                if !ii.hbmMask.is_null() {
                    DeleteObject(ii.hbmMask);
                }
            }
        }
        // SAFETY: the icon SHGetFileInfoW gave us.
        unsafe { DestroyIcon(sfi.hIcon) };
        out
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn icon_png(_path: &str) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_and_encodes() {
        // One opaque blue pixel with alpha, one see-through.
        let rgba = bgra_to_rgba(&[255, 0, 0, 255, 0, 0, 0, 0], None);
        assert_eq!(rgba, vec![0, 0, 255, 255, 0, 0, 0, 0]);
        // No alpha anywhere: the mask decides.
        let rgba = bgra_to_rgba(&[255, 0, 0, 0, 0, 255, 0, 0], Some(&[0, 0, 0, 0, 255, 255, 255, 0]));
        assert_eq!(rgba[3], 255);
        assert_eq!(rgba[7], 0);
        let png = png(&[0u8; 32 * 32 * 4], 32, 32).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }
}

// Camera RAW files through LibRaw (system library, thread-safe variant libraw_r).
// Shows the camera's embedded preview (fast, looks like the camera's own JPEG); files without
// one are developed with LibRaw's default settings.
use std::ffi::{c_char, c_int, c_uint, c_ushort, c_void, CString};
use std::path::Path;

use image::DynamicImage;

#[repr(C)]
struct Processed {
    kind: c_int, // 1 = JPEG, 2 = bitmap
    height: c_ushort,
    width: c_ushort,
    colors: c_ushort,
    bits: c_ushort,
    data_size: c_uint,
    data: [u8; 1],
}

#[link(name = "raw_r")]
unsafe extern "C" {
    fn libraw_init(flags: c_uint) -> *mut c_void;
    fn libraw_open_file(lr: *mut c_void, file: *const c_char) -> c_int;
    fn libraw_unpack_thumb(lr: *mut c_void) -> c_int;
    fn libraw_dcraw_make_mem_thumb(lr: *mut c_void, errc: *mut c_int) -> *mut Processed;
    fn libraw_unpack(lr: *mut c_void) -> c_int;
    fn libraw_dcraw_process(lr: *mut c_void) -> c_int;
    fn libraw_dcraw_make_mem_image(lr: *mut c_void, errc: *mut c_int) -> *mut Processed;
    fn libraw_dcraw_clear_mem(img: *mut Processed);
    fn libraw_close(lr: *mut c_void);
}

struct Handle(*mut c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { libraw_close(self.0) }
    }
}

struct Image(*mut Processed);
impl Drop for Image {
    fn drop(&mut self) {
        unsafe { libraw_dcraw_clear_mem(self.0) }
    }
}

impl Image {
    fn to_dynamic(&self) -> Option<DynamicImage> {
        // SAFETY: LibRaw returned a valid image of data_size bytes
        let p = unsafe { &*self.0 };
        let data = unsafe { std::slice::from_raw_parts(p.data.as_ptr(), p.data_size as usize) };
        match p.kind {
            1 => image::load_from_memory_with_format(data, image::ImageFormat::Jpeg).ok().map(|img| {
                // keep the orientation stored in the preview
                let mut img = img;
                if let Ok(mut d) = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(data)) {
                    use image::ImageDecoder;
                    if let Ok(o) = d.orientation() {
                        img.apply_orientation(o);
                    }
                }
                img
            }),
            2 if p.bits == 8 && p.colors == 3 => {
                image::RgbImage::from_raw(p.width as u32, p.height as u32, data.to_vec()).map(DynamicImage::ImageRgb8)
            }
            2 if p.bits == 8 && p.colors == 1 => {
                image::GrayImage::from_raw(p.width as u32, p.height as u32, data.to_vec()).map(DynamicImage::ImageLuma8)
            }
            _ => None,
        }
    }
}

/// `develop`: develop the RAW data even when there is an embedded preview.
pub fn decode(path: &Path, develop: bool) -> Result<DynamicImage, String> {
    let name = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    let lr = unsafe { libraw_init(0) };
    if lr.is_null() {
        return Err("LibRaw unavailable".into());
    }
    let lr = Handle(lr);
    if unsafe { libraw_open_file(lr.0, name.as_ptr()) } != 0 {
        return Err("not a supported camera RAW file".into());
    }
    let mut err = 0;
    // embedded preview
    if !develop && unsafe { libraw_unpack_thumb(lr.0) } == 0 {
        let p = unsafe { libraw_dcraw_make_mem_thumb(lr.0, &mut err) };
        if !p.is_null() {
            let img = Image(p);
            if let Some(d) = img.to_dynamic() {
                if d.width() >= 640 {
                    return Ok(d);
                }
            }
        }
    }
    // no usable preview: develop the RAW data
    if unsafe { libraw_unpack(lr.0) } != 0 || unsafe { libraw_dcraw_process(lr.0) } != 0 {
        return Err("cannot develop the RAW file".into());
    }
    let p = unsafe { libraw_dcraw_make_mem_image(lr.0, &mut err) };
    if p.is_null() {
        return Err("cannot develop the RAW file".into());
    }
    Image(p).to_dynamic().ok_or_else(|| "unsupported RAW output".into())
}

//! Bounded preprocessing of an already verified game screenshot, frontend only.
use std::ffi::{CString, c_char, c_int, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::NonNull;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub scale: u32,
}

impl Crop {
    pub fn dimensions(self) -> (u32, u32) {
        (self.width * self.scale, self.height * self.scale)
    }

    pub fn map_bounds(
        self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> io::Result<(u32, u32, u32, u32)> {
        if !(1..=3).contains(&self.scale) || self.width > 4096 || self.height > 2160 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid crop scale",
            ));
        }
        let (max_x, max_y) = self.dimensions();
        if width == 0
            || height == 0
            || x == 0
            || y == 0
            || x.saturating_add(width) >= max_x
            || y.saturating_add(height) >= max_y
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "button label was clipped; no click sent",
            ));
        }
        let left = x / self.scale;
        let top = y / self.scale;
        let right = (x + width).div_ceil(self.scale);
        let bottom = (y + height).div_ceil(self.scale);
        Ok((self.x + left, self.y + top, right - left, bottom - top))
    }

    fn validate(self, dimensions: (u32, u32)) -> io::Result<()> {
        if !(1..=3).contains(&self.scale)
            || self.width == 0
            || self.height == 0
            || dimensions.0 > 4096
            || dimensions.1 > 2160
            || self.x.saturating_add(self.width) > dimensions.0
            || self.y.saturating_add(self.height) > dimensions.1
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid button image bounds; no click sent",
            ));
        }
        Ok(())
    }
}

// Leptonica is the image library already used by Tesseract. It is not linked
// into the restricted helper. Only fixed-size PNG preparation is exposed here.
#[link(name = "leptonica")]
unsafe extern "C" {
    fn pixRead(path: *const c_char) -> *mut c_void;
    fn pixDestroy(pix: *mut *mut c_void);
    fn pixGetWidth(pix: *const c_void) -> c_int;
    fn pixGetHeight(pix: *const c_void) -> c_int;
    fn boxCreate(x: c_int, y: c_int, width: c_int, height: c_int) -> *mut c_void;
    fn boxDestroy(rect: *mut *mut c_void);
    fn pixClipRectangle(
        pix: *mut c_void,
        rect: *mut c_void,
        clipped: *mut *mut c_void,
    ) -> *mut c_void;
    fn pixScale(pix: *mut c_void, x: f32, y: f32) -> *mut c_void;
    fn pixWrite(path: *const c_char, pix: *mut c_void, format: c_int) -> c_int;
}

struct Image(NonNull<c_void>);
impl Image {
    fn new(pointer: *mut c_void) -> io::Result<Self> {
        NonNull::new(pointer)
            .map(Self)
            .ok_or_else(|| io::Error::other("could not prepare button image"))
    }
    fn dimensions(&self) -> (u32, u32) {
        // SAFETY: the uniquely owned image remains live until Drop.
        unsafe {
            (
                pixGetWidth(self.0.as_ptr()) as u32,
                pixGetHeight(self.0.as_ptr()) as u32,
            )
        }
    }
}
impl Drop for Image {
    fn drop(&mut self) {
        let mut pointer = self.0.as_ptr();
        // SAFETY: release exactly the owned image reference.
        unsafe {
            pixDestroy(&mut pointer);
        }
    }
}

pub fn prepare(frame: &Path, output: &Path, dimensions: (u32, u32), crop: Crop) -> io::Result<()> {
    crop.validate(dimensions)?;
    let input = CString::new(frame.as_os_str().as_bytes())?;
    let output = CString::new(output.as_os_str().as_bytes())?;
    // SAFETY: both paths are NUL-terminated, inside a private capture directory.
    // The caller has checked the PNG header, complete write, and size limits.
    let image = Image::new(unsafe { pixRead(input.as_ptr()) })?;
    if image.dimensions() != dimensions {
        return Err(io::Error::other(
            "capture dimensions changed during preprocessing",
        ));
    }
    // SAFETY: all rectangle fields were validated against <=4096x2160 bounds.
    let mut rect = unsafe {
        boxCreate(
            crop.x as c_int,
            crop.y as c_int,
            crop.width as c_int,
            crop.height as c_int,
        )
    };
    if rect.is_null() {
        return Err(io::Error::other("could not allocate button image bounds"));
    }
    // SAFETY: live image/rectangle, no output BOX requested. Clipping creates a
    // separately owned PIX; destroy the temporary BOX regardless of success.
    let clipped = unsafe {
        let result = pixClipRectangle(image.0.as_ptr(), rect, std::ptr::null_mut());
        boxDestroy(&mut rect);
        result
    };
    let clipped = Image::new(clipped)?;
    // SAFETY: live image and bounded positive scale. Result owns its reference.
    let scaled =
        Image::new(unsafe { pixScale(clipped.0.as_ptr(), crop.scale as f32, crop.scale as f32) })?;
    if scaled.dimensions() != crop.dimensions() {
        return Err(io::Error::other(
            "unexpected prepared button image dimensions",
        ));
    }
    // SAFETY: live image, valid path, fixed IFF_PNG format from imageio.h.
    if unsafe { pixWrite(output.as_ptr(), scaled.0.as_ptr(), 3) } != 0 {
        return Err(io::Error::other("could not write private button image"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cropped_coordinates_map_back_to_the_same_native_surface() {
        let crop = Crop {
            x: 400,
            y: 371,
            width: 182,
            height: 33,
            scale: 3,
        };
        assert_eq!(crop.dimensions(), (546, 99));
        assert_eq!(
            crop.map_bounds(177, 33, 207, 33).unwrap(),
            (459, 382, 69, 11)
        );
        assert!(crop.map_bounds(0, 33, 207, 33).is_err());
        assert!(crop.map_bounds(177, 33, 400, 33).is_err());
        assert!(Crop { scale: 0, ..crop }.validate((800, 600)).is_err());
        assert!(Crop { x: 790, ..crop }.validate((800, 600)).is_err());
    }

    #[test]
    fn native_image_preparation_produces_a_bounded_scaled_crop() {
        unsafe extern "C" {
            fn pixCreate(width: c_int, height: c_int, depth: c_int) -> *mut c_void;
        }
        let directory =
            std::env::temp_dir().join(format!("soberized-image-test-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let frame = directory.join("source.png");
        let output = directory.join("button.png");
        let image = Image::new(unsafe { pixCreate(800, 600, 32) }).unwrap();
        let path = CString::new(frame.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { pixWrite(path.as_ptr(), image.0.as_ptr(), 3) }, 0);
        let crop = Crop {
            x: 400,
            y: 371,
            width: 182,
            height: 33,
            scale: 3,
        };
        prepare(&frame, &output, (800, 600), crop).unwrap();
        let path = CString::new(output.as_os_str().as_bytes()).unwrap();
        let result = Image::new(unsafe { pixRead(path.as_ptr()) }).unwrap();
        assert_eq!(result.dimensions(), (546, 99));
        assert!(prepare(&frame, &output, (801, 600), crop).is_err());
    }
}

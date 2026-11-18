use std::path::Path;

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::fs::File;
    use std::io::BufWriter;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Gdi::{
        GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HBITMAP, HDC, RGBQUAD,
    };
    use windows_sys::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn DeleteObject(object: HBITMAP) -> i32;
    }

    struct IconResources {
        icon: windows_sys::Win32::UI::WindowsAndMessaging::HICON,
        dc: HDC,
        color: HBITMAP,
        mask: HBITMAP,
    }

    #[repr(C)]
    struct MaskBitmapInfo {
        header: BITMAPINFOHEADER,
        colors: [RGBQUAD; 2],
    }

    impl Drop for IconResources {
        fn drop(&mut self) {
            unsafe {
                if !self.dc.is_null() {
                    ReleaseDC(null_mut::<c_void>() as HWND, self.dc);
                }
                if !self.color.is_null() {
                    DeleteObject(self.color);
                }
                if !self.mask.is_null() {
                    DeleteObject(self.mask);
                }
                if !self.icon.is_null() {
                    DestroyIcon(self.icon);
                }
            }
        }
    }

    pub fn extract_to_png(executable: &Path, destination: &Path) -> Result<(), String> {
        let wide_path: Vec<u16> = executable
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut file_info: SHFILEINFOW = unsafe { std::mem::zeroed() };
        let result = unsafe {
            SHGetFileInfoW(
                wide_path.as_ptr(),
                0,
                &mut file_info,
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON,
            )
        };
        if result == 0 || file_info.hIcon.is_null() {
            return Err(format!(
                "Windows no pudo extraer el icono de {}",
                executable.display()
            ));
        }

        let mut resources = IconResources {
            icon: file_info.hIcon,
            dc: null_mut(),
            color: null_mut(),
            mask: null_mut(),
        };
        let mut icon_info: ICONINFO = unsafe { std::mem::zeroed() };
        if unsafe { GetIconInfo(resources.icon, &mut icon_info) } == 0 {
            return Err(format!(
                "No se pudo leer el icono de {}",
                executable.display()
            ));
        }
        resources.color = icon_info.hbmColor;
        resources.mask = icon_info.hbmMask;
        if resources.color.is_null() {
            return Err(format!(
                "El ejecutable no contiene un icono de color: {}",
                executable.display()
            ));
        }

        let mut bitmap: BITMAP = unsafe { std::mem::zeroed() };
        if unsafe {
            GetObjectW(
                resources.color,
                size_of::<BITMAP>() as i32,
                &mut bitmap as *mut BITMAP as *mut c_void,
            )
        } == 0
            || bitmap.bmWidth <= 0
            || bitmap.bmHeight <= 0
            || bitmap.bmWidth > 512
            || bitmap.bmHeight > 512
        {
            return Err(format!(
                "El icono de {} tiene dimensiones inválidas",
                executable.display()
            ));
        }

        resources.dc = unsafe { GetDC(null_mut::<c_void>() as HWND) };
        if resources.dc.is_null() {
            return Err("No se pudo obtener un contexto gráfico de Windows".to_string());
        }

        let width = bitmap.bmWidth as u32;
        let height = bitmap.bmHeight as u32;
        let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: height as i32,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let mut bgra = vec![0u8; (width * height * 4) as usize];
        let lines = unsafe {
            GetDIBits(
                resources.dc,
                resources.color,
                0,
                height,
                bgra.as_mut_ptr() as *mut c_void,
                &mut info,
                DIB_RGB_COLORS,
            )
        };
        if lines == 0 {
            return Err(format!(
                "No se pudieron leer los píxeles de {}",
                executable.display()
            ));
        }

        let alpha_is_empty = bgra.iter().skip(3).step_by(4).all(|alpha| *alpha == 0);
        let mask_stride = width.div_ceil(32) as usize * 4;
        let mask_pixels = if alpha_is_empty && !resources.mask.is_null() {
            let mut mask_info = MaskBitmapInfo {
                header: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: height as i32,
                    biPlanes: 1,
                    biBitCount: 1,
                    biCompression: BI_RGB,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 2,
                    biClrImportant: 0,
                },
                colors: [RGBQUAD::default(); 2],
            };
            let mut mask = vec![0u8; mask_stride * height as usize];
            let copied = unsafe {
                GetDIBits(
                    resources.dc,
                    resources.mask,
                    0,
                    height,
                    mask.as_mut_ptr() as *mut c_void,
                    &mut mask_info as *mut MaskBitmapInfo as *mut BITMAPINFO,
                    DIB_RGB_COLORS,
                )
            };
            (copied != 0).then_some(mask)
        } else {
            None
        };
        let row_bytes = (width * 4) as usize;
        let mut rgba = vec![0u8; bgra.len()];
        for y in 0..height as usize {
            let source = (height as usize - 1 - y) * row_bytes;
            let target = y * row_bytes;
            for x in 0..width as usize {
                let src = source + x * 4;
                let dst = target + x * 4;
                rgba[dst] = bgra[src + 2];
                rgba[dst + 1] = bgra[src + 1];
                rgba[dst + 2] = bgra[src];
                rgba[dst + 3] = if alpha_is_empty {
                    mask_pixels.as_ref().map_or(255, |mask| {
                        let mask_row = (height as usize - 1 - y) * mask_stride;
                        let transparent = mask[mask_row + x / 8] & (0x80 >> (x % 8)) != 0;
                        if transparent {
                            0
                        } else {
                            255
                        }
                    })
                } else {
                    bgra[src + 3]
                };
            }
        }

        let file = File::create(destination).map_err(|error| error.to_string())?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
        writer
            .write_image_data(&rgba)
            .map_err(|error| error.to_string())
    }
}

#[cfg(windows)]
pub fn extract_to_png(executable: &Path, destination: &Path) -> Result<(), String> {
    windows::extract_to_png(executable, destination)
}

#[cfg(not(windows))]
pub fn extract_to_png(_executable: &Path, _destination: &Path) -> Result<(), String> {
    Err("La extracción de iconos de ejecutables solo está disponible en Windows".to_string())
}

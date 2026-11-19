use std::{collections::HashMap, ptr::null};
use windows::{
    Win32::{
        Foundation::HWND,
        Graphics::{
            Direct2D::{Common::*, *},
            DirectWrite::*,
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Imaging::*,
        },
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    },
    core::{Interface, w},
};

use crate::assets::ImageKey;

pub const BG: u32 = 0x111624;
pub const PANEL: u32 = 0x181f30;
pub const CARD: u32 = 0x20293c;
pub const BORDER: u32 = 0x36445e;
pub const TEXT: u32 = 0xeff3fa;
pub const MUTED: u32 = 0xa9b6cb;
pub const WIN: u32 = 0x83dbc9;
pub const LOSS: u32 = 0xf5a9a8;
pub const ACCENT: u32 = 0x9aafff;

#[derive(Clone, Copy)]
pub enum Font {
    Small,
    Base,
    Strong,
    Title,
}

fn color(rgb: u32) -> D2D1_COLOR_F {
    let channel = |shift| (rgb >> shift & 255u32) as f32 / 255.0;
    D2D1_COLOR_F {
        r: channel(16),
        g: channel(8),
        b: channel(0),
        a: 1.0,
    }
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + w.max(0.0),
        bottom: y + h.max(0.0),
    }
}

pub struct Renderer {
    factory: ID2D1Factory,
    fonts: [IDWriteTextFormat; 4],
    decoder: Option<IWICImagingFactory>,
}

impl Renderer {
    pub fn new() -> windows::core::Result<Self> {
        // Created and used on the GUI thread. The host initializes COM on this thread.
        unsafe {
            let factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let format = |size, weight| {
                dwrite
                    .CreateTextFormat(
                        w!("Microsoft YaHei UI"),
                        None,
                        weight,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        size,
                        w!("zh-CN"),
                    )
                    .and_then(|format| {
                        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                        let sign = dwrite.CreateEllipsisTrimmingSign(&format)?;
                        format.SetTrimming(
                            &DWRITE_TRIMMING {
                                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                                delimiter: 0,
                                delimiterCount: 0,
                            },
                            &sign,
                        )?;
                        Ok(format)
                    })
            };
            Ok(Self {
                factory,
                fonts: [
                    format(12.0, DWRITE_FONT_WEIGHT_NORMAL)?,
                    format(14.0, DWRITE_FONT_WEIGHT_NORMAL)?,
                    format(15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
                    format(22.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
                ],
                decoder: CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                    .ok(),
            })
        }
    }

    pub fn decode(&self, key: ImageKey, bytes: &[u8]) -> Option<Pixels> {
        let factory = self.decoder.as_ref()?;
        let side = match key {
            ImageKey::Champion(_) => 64,
            ImageKey::Item(_) | ImageKey::Spell(_) | ImageKey::Rune(_) => 48,
            ImageKey::Rank(_) => 96,
        };
        // Keep the memory alive until the stream, decoder and frame have been released.
        unsafe {
            let stream = factory.CreateStream().ok()?;
            stream.InitializeFromMemory(bytes).ok()?;
            let stream_base: windows::Win32::System::Com::IStream = stream.cast().ok()?;
            let decoder = factory
                .CreateDecoderFromStream(&stream_base, null(), WICDecodeMetadataCacheOnDemand)
                .ok()?;
            let frame = decoder.GetFrame(0).ok()?;
            let mut width = 0;
            let mut height = 0;
            frame.GetSize(&mut width, &mut height).ok()?;
            if width == 0 || height == 0 || width > 2048 || height > 2048 {
                return None;
            }
            let scaler = factory.CreateBitmapScaler().ok()?;
            scaler
                .Initialize(&frame, side, side, WICBitmapInterpolationModeFant)
                .ok()?;
            let format = factory.CreateFormatConverter().ok()?;
            format
                .Initialize(
                    &scaler,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .ok()?;
            let mut bytes = vec![0; (side * side * 4) as usize];
            format.CopyPixels(null(), side * 4, &mut bytes).ok()?;
            Some(Pixels { side, bytes })
        }
    }

    pub fn target(
        &self,
        hwnd: HWND,
        width: u32,
        height: u32,
        dpi: u32,
    ) -> windows::core::Result<Surface> {
        unsafe {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                ..Default::default()
            };
            let window = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U { width, height },
                ..Default::default()
            };
            let target = self.factory.CreateHwndRenderTarget(&props, &window)?;
            let brush = target.CreateSolidColorBrush(&color(TEXT), None)?;
            Ok(Surface {
                target,
                brush,
                size: (width, height, dpi),
                bitmaps: HashMap::new(),
            })
        }
    }
}

pub struct Pixels {
    side: u32,
    bytes: Vec<u8>,
}

pub struct Surface {
    target: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    size: (u32, u32, u32),
    bitmaps: HashMap<ImageKey, ID2D1Bitmap>,
}

impl Surface {
    pub fn resize(&mut self, width: u32, height: u32, dpi: u32) -> windows::core::Result<()> {
        if self.size != (width, height, dpi) {
            unsafe {
                self.target.Resize(&D2D_SIZE_U { width, height })?;
                self.target.SetDpi(dpi as f32, dpi as f32);
            }
            self.size = (width, height, dpi);
        }
        Ok(())
    }

    pub fn begin(&self, rgb: u32) {
        unsafe {
            self.target.BeginDraw();
            self.target.Clear(Some(&color(rgb)));
        }
    }

    pub fn finish(&self) -> windows::core::Result<()> {
        unsafe { self.target.EndDraw(None, None) }
    }

    pub fn fill(&self, bounds: D2D_RECT_F, rgb: u32, radius: f32) {
        unsafe {
            self.brush.SetColor(&color(rgb));
            self.target.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: bounds,
                    radiusX: radius,
                    radiusY: radius,
                },
                &self.brush,
            );
        }
    }

    pub fn border(&self, bounds: D2D_RECT_F, rgb: u32, radius: f32) {
        unsafe {
            self.brush.SetColor(&color(rgb));
            self.target.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: bounds,
                    radiusX: radius,
                    radiusY: radius,
                },
                &self.brush,
                1.0,
                None,
            );
        }
    }

    pub fn text(&self, renderer: &Renderer, text: &str, bounds: D2D_RECT_F, font: Font, rgb: u32) {
        if text.is_empty() {
            return;
        }
        let utf16: Vec<_> = text.encode_utf16().collect();
        unsafe {
            self.brush.SetColor(&color(rgb));
            self.target.DrawText(
                &utf16,
                &renderer.fonts[font as usize],
                &bounds,
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    pub fn image(&mut self, key: ImageKey, pixels: Option<&Pixels>, bounds: D2D_RECT_F) {
        if pixels.is_none() && !self.bitmaps.contains_key(&key) {
            self.border(bounds, BORDER, 4.0);
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = self.bitmaps.entry(key)
            && let Some(pixels) = pixels
        {
            let side = pixels.side;
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            if let Ok(bitmap) = unsafe {
                self.target.CreateBitmap(
                    D2D_SIZE_U {
                        width: side,
                        height: side,
                    },
                    Some(pixels.bytes.as_ptr().cast()),
                    side * 4,
                    &props,
                )
            } {
                entry.insert(bitmap);
            }
        }
        if let Some(bitmap) = self.bitmaps.get(&key) {
            unsafe {
                self.target.DrawBitmap(
                    bitmap,
                    Some(&bounds),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                )
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

    #[test]
    fn transparent_png_stays_transparent_after_wic_decode() {
        let _com =
            crate::ComApartment(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok());
        let renderer = Renderer::new().unwrap();
        let png=STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DAAAAEAQEARwbK3gAAAABJRU5ErkJggg==").unwrap();
        let pixels = renderer.decode(ImageKey::Rank(6), &png).unwrap();
        assert_eq!(pixels.side, 96);
        assert!(
            pixels
                .bytes
                .chunks_exact(4)
                .all(|p| p[3] == 0 && p[..3] == [0, 0, 0]),
            "alpha must remain transparent and premultiplied"
        );
    }

    #[test]
    fn windows_renders_a_cached_bitmap_on_an_hwnd_target() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WS_POPUP,
        };
        let _com =
            crate::ComApartment(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok());
        let renderer = Renderer::new().unwrap();
        let class: Vec<_> = "STATIC\0".encode_utf16().collect();
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                200,
                160,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                null(),
            )
        };
        assert!(!window.is_null());
        let mut surface = renderer.target(HWND(window), 200, 160, 96).unwrap();
        let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+sJ1sAAAAASUVORK5CYII=").unwrap();
        let key = ImageKey::Champion(22);
        let bitmap = renderer
            .decode(key, &png)
            .expect("WIC decodes the small local PNG");
        assert_eq!(bitmap.bytes.len(), 64 * 64 * 4);
        surface.begin(BG);
        surface.image(key, Some(&bitmap), rect(8.0, 8.0, 40.0, 40.0));
        surface.text(
            &renderer,
            "战绩",
            rect(60.0, 8.0, 100.0, 40.0),
            Font::Strong,
            TEXT,
        );
        surface.finish().unwrap();
        drop(surface);
        unsafe {
            DestroyWindow(window);
        }
    }
}

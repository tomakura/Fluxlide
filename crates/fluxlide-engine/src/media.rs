//! Media sources (photos and videos) turned into GPU textures.

use std::io::BufReader;
use std::path::Path;

use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageReader, metadata::Orientation};

use crate::{EngineError, Renderer};

pub mod audio;
pub mod music;
pub mod video;

/// What a file is played as, decided by its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

impl MediaKind {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            // HEIC and HEIF are made JPEG when imported (`Ffmpeg::picture_to_jpeg`).
            "jpg" | "jpeg" | "png" | "heic" | "heif" => Some(Self::Image),
            "mp4" | "m4v" | "mov" => Some(Self::Video),
            _ => None,
        }
    }
}

/// How the planes of a [`MediaTexture`] are interpreted by the shader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PixelLayout {
    /// One sRGB RGBA texture.
    Rgba,
    /// Three 8-bit planes (Y, U, V), chroma at half resolution,
    /// BT.709 limited range.
    Yuv420,
}

/// A picture on the GPU, ready to be drawn as a layer.
pub struct MediaTexture {
    layout: PixelLayout,
    planes: Vec<wgpu::Texture>,
    views: Vec<wgpu::TextureView>,
    width: u32,
    height: u32,
}

impl MediaTexture {
    /// Creates an sRGB texture from tightly packed RGBA8 pixels.
    pub fn from_rgba(renderer: &Renderer, width: u32, height: u32, pixels: &[u8]) -> Self {
        let texture = create_plane(
            renderer,
            "fluxlide-image",
            width,
            height,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        write_plane(renderer, &texture, width, height, 4, pixels);
        Self::new(PixelLayout::Rgba, vec![texture], width, height)
    }

    /// Creates empty Y, U and V planes for video frames of this size.
    pub(crate) fn yuv420(renderer: &Renderer, width: u32, height: u32) -> Self {
        let (cw, ch) = chroma_size(width, height);
        let planes = vec![
            create_plane(
                renderer,
                "fluxlide-video-y",
                width,
                height,
                wgpu::TextureFormat::R8Unorm,
            ),
            create_plane(
                renderer,
                "fluxlide-video-u",
                cw,
                ch,
                wgpu::TextureFormat::R8Unorm,
            ),
            create_plane(
                renderer,
                "fluxlide-video-v",
                cw,
                ch,
                wgpu::TextureFormat::R8Unorm,
            ),
        ];
        Self::new(PixelLayout::Yuv420, planes, width, height)
    }

    fn new(layout: PixelLayout, planes: Vec<wgpu::Texture>, width: u32, height: u32) -> Self {
        let views = planes
            .iter()
            .map(|plane| plane.create_view(&wgpu::TextureViewDescriptor::default()))
            .collect();
        Self {
            layout,
            planes,
            views,
            width,
            height,
        }
    }

    /// Uploads one planar YUV 4:2:0 frame (Y, then U, then V, no padding).
    pub(crate) fn write_yuv420(&self, renderer: &Renderer, frame: &[u8]) {
        debug_assert_eq!(self.layout, PixelLayout::Yuv420);
        let (cw, ch) = chroma_size(self.width, self.height);
        let luma = (self.width * self.height) as usize;
        let chroma = (cw * ch) as usize;
        if frame.len() < luma + 2 * chroma {
            return;
        }
        write_plane(
            renderer,
            &self.planes[0],
            self.width,
            self.height,
            1,
            &frame[..luma],
        );
        write_plane(
            renderer,
            &self.planes[1],
            cw,
            ch,
            1,
            &frame[luma..luma + chroma],
        );
        write_plane(
            renderer,
            &self.planes[2],
            cw,
            ch,
            1,
            &frame[luma + chroma..luma + 2 * chroma],
        );
    }

    /// Size of the picture in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub(crate) fn layout(&self) -> PixelLayout {
        self.layout
    }

    /// Views bound to the three texture slots of the layer shader.
    /// Single-plane pictures repeat their only view.
    pub(crate) fn plane_views(&self) -> [&wgpu::TextureView; 3] {
        let last = self.views.len() - 1;
        [
            &self.views[0],
            &self.views[1.min(last)],
            &self.views[2.min(last)],
        ]
    }
}

pub(crate) fn chroma_size(width: u32, height: u32) -> (u32, u32) {
    (width.div_ceil(2), height.div_ceil(2))
}

fn create_plane(
    renderer: &Renderer,
    label: &str,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    renderer.device().create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn write_plane(
    renderer: &Renderer,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    data: &[u8],
) {
    renderer.queue().write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * bytes_per_pixel),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

/// Decodes a JPEG or PNG, applies its EXIF orientation and shrinks it to fit
/// within `max_size` (never enlarges). The result is uploaded to the GPU.
///
/// This blocks for decoding and resizing, so call it off the render thread.
pub fn load_image(
    renderer: &Renderer,
    path: &Path,
    max_size: (u32, u32),
) -> Result<MediaTexture, EngineError> {
    let image = decode_image(path, fit_limit(renderer, max_size))?;
    let rgba = image.into_rgba8();
    Ok(MediaTexture::from_rgba(
        renderer,
        rgba.width(),
        rgba.height(),
        rgba.as_raw(),
    ))
}

/// Writes a JPEG of the photo at `path`, turned by its EXIF orientation and
/// shrunk to fit within `max_size`, to `out`. Returns the size of the
/// photo itself after turning, which is also how a damaged file is found
/// when it is imported.
pub fn write_thumbnail(
    path: &Path,
    out: &Path,
    max_size: (u32, u32),
) -> Result<(u32, u32), EngineError> {
    let (image, size) = decode_with_size(path, max_size)?;
    image
        .thumbnail(max_size.0, max_size.1)
        .into_rgb8()
        .save_with_format(out, image::ImageFormat::Jpeg)
        .map_err(|err| EngineError::Media(format!("could not write {}: {err}", out.display())))?;
    Ok(size)
}

/// Caps `max_size` by the largest texture the GPU accepts.
pub(crate) fn fit_limit(renderer: &Renderer, max_size: (u32, u32)) -> (u32, u32) {
    let limit = renderer.device().limits().max_texture_dimension_2d;
    (max_size.0.clamp(1, limit), max_size.1.clamp(1, limit))
}

fn decode_image(path: &Path, max_size: (u32, u32)) -> Result<DynamicImage, EngineError> {
    decode_with_size(path, max_size).map(|(image, _)| image)
}

/// Like [`decode_image`], also returning the size of the photo itself (after
/// turning), which may be larger than the image returned.
fn decode_with_size(
    path: &Path,
    max_size: (u32, u32),
) -> Result<(DynamicImage, (u32, u32)), EngineError> {
    // A JPEG is decoded at a fraction of its size when that is enough: a
    // phone's 12-megapixel photo for a 1080p screen takes a quarter of the
    // memory and time. Anything it cannot read goes the usual way.
    let (mut image, size) = match decode_jpeg_scaled(path, max_size) {
        Some(decoded) => decoded,
        None => {
            let image = decode_full(path)?;
            let size = (image.width(), image.height());
            (image, size)
        }
    };
    if image.width() > max_size.0 || image.height() > max_size.1 {
        // `resize` keeps the aspect ratio and fits inside the bounds.
        image = image.resize(max_size.0, max_size.1, FilterType::Triangle);
    }
    Ok((image, size))
}

/// Decodes a JPEG with `jpeg-decoder`, which can leave out detail while
/// decoding (1/2, 1/4 or 1/8 of the size), no smaller than fits `max_size`
/// whichever way the photo turns. `None` if it is not a JPEG this decoder
/// reads as RGB or grey.
fn decode_jpeg_scaled(path: &Path, max_size: (u32, u32)) -> Option<(DynamicImage, (u32, u32))> {
    let file = std::fs::File::open(path).ok()?;
    let mut decoder = jpeg_decoder::Decoder::new(BufReader::new(file));
    decoder.read_info().ok()?;
    let info = decoder.info()?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    // The size it is shown at, upright or turned a quarter.
    let fit = |(box_w, box_h): (u32, u32)| {
        let scale = (f64::from(box_w) / f64::from(width))
            .min(f64::from(box_h) / f64::from(height))
            .min(1.0);
        (
            (f64::from(width) * scale).ceil() as u32,
            (f64::from(height) * scale).ceil() as u32,
        )
    };
    let (upright, turned) = (fit(max_size), fit((max_size.1, max_size.0)));
    let wanted = (upright.0.max(turned.0), upright.1.max(turned.1));
    decoder
        .scale(
            wanted.0.min(u32::from(u16::MAX)) as u16,
            wanted.1.min(u32::from(u16::MAX)) as u16,
        )
        .ok()?;
    let pixels = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (out_w, out_h) = (u32::from(info.width), u32::from(info.height));
    let mut image = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => {
            DynamicImage::ImageRgb8(image::RgbImage::from_raw(out_w, out_h, pixels)?)
        }
        jpeg_decoder::PixelFormat::L8 => {
            DynamicImage::ImageLuma8(image::GrayImage::from_raw(out_w, out_h, pixels)?)
        }
        _ => return None,
    };
    let orientation = decoder
        .exif_data()
        .and_then(Orientation::from_exif_chunk)
        .unwrap_or(Orientation::NoTransforms);
    image.apply_orientation(orientation);
    let turned_quarter = matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    let size = if turned_quarter {
        (height, width)
    } else {
        (width, height)
    };
    Some((image, size))
}

/// Decodes any supported photo at full size, turned by its EXIF orientation.
fn decode_full(path: &Path) -> Result<DynamicImage, EngineError> {
    let fail = |err: &dyn std::fmt::Display| {
        EngineError::Media(format!("could not read {}: {err}", path.display()))
    };
    let mut decoder = ImageReader::open(path)
        .map_err(|err| fail(&err))?
        .with_guessed_format()
        .map_err(|err| fail(&err))?
        .into_decoder()
        .map_err(|err| fail(&err))?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(|err| fail(&err))?;
    image.apply_orientation(orientation);
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_follows_extension() {
        assert_eq!(
            MediaKind::from_path(Path::new("a/B.JPG")),
            Some(MediaKind::Image)
        );
        assert_eq!(
            MediaKind::from_path(Path::new("clip.mp4")),
            Some(MediaKind::Video)
        );
        assert_eq!(
            MediaKind::from_path(Path::new("IMG_0001.HEIC")),
            Some(MediaKind::Image)
        );
        assert_eq!(MediaKind::from_path(Path::new("notes.txt")), None);
        assert_eq!(MediaKind::from_path(Path::new("noext")), None);
    }

    #[test]
    fn decoded_image_fits_within_bounds() {
        let dir = std::env::temp_dir().join(format!("fluxlide-image-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wide.png");
        image::RgbaImage::from_pixel(400, 100, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .unwrap();

        let image = decode_image(&path, (200, 200)).unwrap();
        assert_eq!((image.width(), image.height()), (200, 50));
        let image = decode_image(&path, (1000, 1000)).unwrap();
        assert_eq!((image.width(), image.height()), (400, 100));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_thumbnail_is_shrunk_and_reports_the_photo_size() {
        let dir =
            std::env::temp_dir().join(format!("fluxlide-thumbnail-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tall.png");
        image::RgbaImage::from_pixel(300, 600, image::Rgba([200, 20, 30, 255]))
            .save(&path)
            .unwrap();
        let out = dir.join("tall.jpg");
        assert_eq!(
            write_thumbnail(&path, &out, (100, 100)).unwrap(),
            (300, 600)
        );
        let thumbnail = image::open(&out).unwrap();
        assert_eq!((thumbnail.width(), thumbnail.height()), (50, 100));

        std::fs::write(dir.join("broken.png"), b"not a picture").unwrap();
        assert!(write_thumbnail(&dir.join("broken.png"), &out, (100, 100)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A JPEG of `width` x `height`, red on the left half and blue on the
    /// right, with the EXIF orientation `exif` (1 = upright, 6 = turn right).
    fn jpeg(path: &Path, width: u32, height: u32, exif: u16) {
        use image::ImageEncoder;
        let pixels = image::RgbImage::from_fn(width, height, |x, _| {
            if x < width / 2 {
                image::Rgb([220, 30, 30])
            } else {
                image::Rgb([30, 30, 220])
            }
        });
        // A TIFF header and one IFD entry: Orientation (0x0112), SHORT, 1.
        let mut tiff = b"II*         ".to_vec();
        tiff.extend_from_slice(&exif.to_le_bytes());
        tiff.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut out = std::fs::File::create(path).unwrap();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
        encoder.set_exif_metadata(tiff).unwrap();
        encoder
            .write_image(&pixels, width, height, image::ExtendedColorType::Rgb8)
            .unwrap();
    }

    #[test]
    fn a_large_jpeg_is_decoded_small_and_turned_upright() {
        let dir = std::env::temp_dir().join(format!("fluxlide-jpeg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // A phone photo for a 720p screen: decoded at 1/4, then fitted.
        let flat = dir.join("flat.jpg");
        jpeg(&flat, 4000, 3000, 1);
        let (image, size) = decode_with_size(&flat, (1280, 720)).unwrap();
        assert_eq!(size, (4000, 3000));
        assert_eq!((image.width(), image.height()), (960, 720));
        let rgb = image.to_rgb8();
        assert!(rgb.get_pixel(100, 360)[0] > 180, "red on the left");
        assert!(rgb.get_pixel(860, 360)[2] > 180, "blue on the right");

        // Held upright: stored on its side with orientation 6 (turn right),
        // so the red half ends up on top.
        let upright = dir.join("upright.jpg");
        jpeg(&upright, 4000, 3000, 6);
        let (image, size) = decode_with_size(&upright, (1280, 720)).unwrap();
        assert_eq!(size, (3000, 4000));
        assert_eq!((image.width(), image.height()), (540, 720));
        let rgb = image.to_rgb8();
        assert!(rgb.get_pixel(270, 100)[0] > 180, "red on top");
        assert!(rgb.get_pixel(270, 620)[2] > 180, "blue below");

        // The same as decoding at full size, for a thumbnail.
        let out = dir.join("thumb.jpg");
        assert_eq!(
            write_thumbnail(&upright, &out, (200, 200)).unwrap(),
            (3000, 4000)
        );
        let thumbnail = image::open(&out).unwrap();
        assert_eq!((thumbnail.width(), thumbnail.height()), (150, 200));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

//! Bounded validation shared by all public avatar, poster and branding uploads.

use std::io::Cursor;
use std::sync::{Arc, LazyLock};

use image::{ImageDecoder, ImageFormat, ImageReader};
use tokio::sync::Semaphore;

use crate::utils::error::{AppError, AppResult};

const MAX_PIXELS: u64 = 4 * 1024 * 1024;
const MAX_DIMENSION: u32 = 4096;
const MAX_DECODE_BYTES: u64 = 32 * 1024 * 1024;
static DECODERS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(2)));

fn supported_format(bytes: &[u8]) -> Option<ImageFormat> {
    image::guess_format(bytes).ok().filter(|format| {
        matches!(
            format,
            ImageFormat::Png
                | ImageFormat::Jpeg
                | ImageFormat::Gif
                | ImageFormat::WebP
                | ImageFormat::Ico
        )
    })
}

/// Detection is used only for the response type of already-authorized public
/// assets. Upload acceptance additionally requires a bounded, successful decode.
pub(crate) fn content_type(prefix: &[u8]) -> Option<&'static str> {
    supported_format(prefix).map(|format| format.to_mime_type())
}

fn decode(bytes: &[u8]) -> AppResult<()> {
    let invalid = || {
        AppError::bad_request("Upload a valid PNG, JPEG, GIF, WebP or ICO image (at most 4 megapixels and 4096 pixels per side)")
    };
    let format = supported_format(bytes).ok_or_else(invalid)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().map_err(|_| invalid())?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(invalid());
    }
    // Account for the output buffer as well as the decoder's working memory,
    // matching ImageReader::decode after the independent pixel-count check.
    limits
        .reserve(decoder.total_bytes())
        .map_err(|_| invalid())?;
    decoder.set_limits(limits).map_err(|_| invalid())?;
    image::DynamicImage::from_decoder(decoder).map_err(|_| invalid())?;
    Ok(())
}

/// Keep original bytes and hashes stable for durable operation replay and GIFs.
/// The permit lives in the blocking task even when the HTTP caller disconnects.
pub(crate) async fn validate(bytes: Vec<u8>) -> AppResult<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > super::upload::IMAGE_FILE_BYTES {
        return Err(AppError::bad_request(
            "Image must be between 1 byte and 3 MiB",
        ));
    }
    let permit = DECODERS
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::overloaded("Image validation is busy; retry shortly", 1))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        decode(&bytes)?;
        Ok(bytes)
    })
    .await
    .map_err(|_| AppError::internal("Image validation task failed"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn png() -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[tokio::test]
    async fn public_images_require_decodable_bounded_pixels_and_preserve_bytes() {
        let bytes = png();
        assert_eq!(content_type(&bytes[..32]), Some("image/png"));
        assert_eq!(validate(bytes.clone()).await.unwrap(), bytes);
        for bytes in [
            b"not an image".to_vec(),
            b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(),
            png()[..32].to_vec(),
        ] {
            assert_eq!(
                validate(bytes).await.unwrap_err().status(),
                axum::http::StatusCode::BAD_REQUEST
            );
        }
        let mut oversized = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(4097, 1)
            .write_to(&mut oversized, ImageFormat::Png)
            .unwrap();
        assert!(validate(oversized.into_inner()).await.is_err());
        let mut pixels = Cursor::new(Vec::new());
        image::DynamicImage::new_luma8(2049, 2048)
            .write_to(&mut pixels, ImageFormat::Png)
            .unwrap();
        assert!(validate(pixels.into_inner()).await.is_err());
    }

    #[test]
    fn passive_formats_are_detected_from_bytes_not_filename_or_declared_mime() {
        for (format, mime) in [
            (ImageFormat::Png, "image/png"),
            (ImageFormat::Jpeg, "image/jpeg"),
            (ImageFormat::Gif, "image/gif"),
            (ImageFormat::WebP, "image/webp"),
            (ImageFormat::Ico, "image/x-icon"),
        ] {
            let mut bytes = Cursor::new(Vec::new());
            let image = if format == ImageFormat::Ico {
                image::DynamicImage::new_rgba8(2, 2)
            } else {
                image::DynamicImage::new_rgb8(2, 2)
            };
            image.write_to(&mut bytes, format).unwrap();
            let bytes = bytes.into_inner();
            decode(&bytes).unwrap_or_else(|error| panic!("{format:?}: {error}"));
            assert_eq!(content_type(&bytes[..bytes.len().min(32)]), Some(mime));
        }
        assert_eq!(
            content_type(b"<!doctype html><script>alert(1)</script>"),
            None
        );
    }
}

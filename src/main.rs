use std::{
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io::{Error as IoError, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use brotli::CompressorReader as BrCompressorReader;
use flate2::{write::DeflateEncoder, Compression as FlateCompression, GzBuilder};
use image::{ImageError, ImageFormat};
use walkdir::{DirEntry, Error as WalkDirError, WalkDir};
use webp::{Encoder as WebPEncoder, WebPEncodingError};

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("This command requires at least one argument!");

    let webp = if std::env::var("WEBP_LOSSLESS").is_ok_and(|v| v != "false" && v != "0") {
        WebPQualityConfig::Lossless
    } else if let Ok(requested_quality) = std::env::var("WEBP_QUALITY") {
        let requested_quality: f32 = requested_quality
            .parse()
            .expect("WEBP_QUALITY must be a float between 0 and 100, inclusive.");
        if !(0.0..=100.0).contains(&requested_quality) {
            panic!("Expected WEBP_QUALITY to be a float between 0 and 100, inclusive.");
        }
        WebPQualityConfig::Lossy(requested_quality)
    } else {
        WebPQualityConfig::Lossy(80.0)
    };

    let existing_files: Vec<DirEntry> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|v| match v {
            Ok(v) => {
                if v.file_type().is_file() {
                    Some(v)
                } else {
                    None
                }
            }
            Err(e) => {
                eprintln!("Error finding file: {e}");
                None
            }
        })
        .collect();

    let config = Config { webp };
    for item in existing_files {
        let path_display = item.path().display().to_string();
        if let Err(e) = process_entry(config, item) {
            eprintln!("Error processing file {path_display}: {e}",);
        }
    }
}

fn process_entry(config: Config, item: DirEntry) -> Result<(), Error> {
    let ext = item.path().extension().ok_or(Error::NoExtension)?;
    match ext.as_encoded_bytes() {
        b"png" | b"jpeg" | b"bmp" | b"avif" | b"webp" => image_compress(config, item)?,
        b"br" | b"gz" | b"zst" | b"zz" => {}
        _ => generic_compress(item)?,
    }
    Ok(())
}

fn generic_compress(item: DirEntry) -> Result<(), Error> {
    let item_path = item.clone().into_path();
    let mut initial = OpenOptions::new().read(true).open(&item_path)?;

    let mut br_file = create_new_extended(&item_path, "br")?;
    let mut br = BrCompressorReader::new(&mut initial, 4096, 9, 21);
    std::io::copy(&mut br, &mut br_file)?;
    drop(br_file);
    initial.seek(SeekFrom::Start(0))?;

    let gz_file = create_new_extended(&item_path, "gz")?;
    let mut gz = GzBuilder::new().write(gz_file, FlateCompression::best());
    std::io::copy(&mut initial, &mut gz)?;
    drop(gz);
    initial.seek(SeekFrom::Start(0))?;

    let zst_file = create_new_extended(&item_path, "zst")?;
    zstd::stream::copy_encode(&mut initial, zst_file, 19)?;
    initial.seek(SeekFrom::Start(0))?;

    let zz_file = create_new_extended(&item_path, "zz")?;
    let mut zz = DeflateEncoder::new(zz_file, FlateCompression::best());
    std::io::copy(&mut initial, &mut zz)?;
    drop(zz);
    initial.seek(SeekFrom::Start(0))?;

    Ok(())
}

fn image_compress(config: Config, item: DirEntry) -> Result<(), Error> {
    let path = item.path();
    let image = image::open(path)?;

    let webp_encoder =
        WebPEncoder::from_image(&image).map_err(|_| Error::UnimplementedWebPImageFormat)?;
    let webp_pixmap = webp_encoder.encode_simple(config.webp.lossless(), config.webp.quality())?;
    let mut webp_out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.with_extension("webp"))?;
    webp_out.write_all(webp_pixmap.as_ref())?;

    image.save_with_format(path.with_extension("avif"), ImageFormat::Avif)?;
    image.save_with_format(path.with_extension("jpeg"), ImageFormat::Jpeg)?;
    image.save_with_format(path.with_extension("png"), ImageFormat::Png)?;
    Ok(())
}

fn create_new_extended(path: &Path, ext: impl AsRef<OsStr>) -> Result<File, IoError> {
    let extended = add_extension(path.to_path_buf(), ext);
    println!("{}", extended.display());
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(extended)
}

pub fn add_extension(path: PathBuf, ext: impl AsRef<OsStr>) -> PathBuf {
    let mut os_string: OsString = path.into();
    os_string.push(".");
    os_string.push(ext.as_ref());
    os_string.into()
}

#[derive(Clone, Copy)]
struct Config {
    webp: WebPQualityConfig,
}

#[derive(Clone, Copy)]
enum WebPQualityConfig {
    Lossless,
    Lossy(f32),
}

impl WebPQualityConfig {
    pub fn lossless(&self) -> bool {
        match self {
            Self::Lossless => true,
            Self::Lossy(_) => false,
        }
    }

    pub fn quality(&self) -> f32 {
        match self {
            Self::Lossless => 75.0,
            Self::Lossy(v) => v.clamp(0.0, 100.0),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] IoError),
    #[error("Directory walking error: {0}")]
    Walkdir(#[from] WalkDirError),
    #[error("Image coding error: {0}")]
    Image(#[from] ImageError),
    #[error("WebP Encoding error")]
    WebP(WebPEncodingError),
    #[error("Encountered a file with no extension")]
    NoExtension,
    #[error("WebP does not support some dynamic image types: https://docs.rs/webp/0.2.6/src/webp/encoder.rs.html#29-45")]
    UnimplementedWebPImageFormat,
}

impl From<WebPEncodingError> for Error {
    fn from(value: WebPEncodingError) -> Self {
        Self::WebP(value)
    }
}

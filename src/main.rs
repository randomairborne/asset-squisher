use std::{
    ffi::{OsStr, OsString},
    fmt::{Debug, Display},
    fs::{File, OpenOptions},
    io::{Error as IoError, Seek, SeekFrom, Write},
    num::NonZeroUsize,
    ops::RangeInclusive,
    path::{Path, PathBuf},
    str::FromStr,
    time::Instant,
};

use brotli::CompressorReader as BrCompressorReader;
use flate2::{write::DeflateEncoder, Compression as FlateCompression, GzBuilder};
use image::{
    codecs::{avif::AvifEncoder, jpeg::JpegEncoder, png::PngEncoder},
    ImageError,
};
use walkdir::{DirEntry, Error as WalkDirError, WalkDir};
use webp::{Encoder as WebPEncoder, WebPEncodingError};

const DEFAULT_PARALLELISM: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(1) };
const DEFAULT_ZSTD_LEVEL: i32 = 7;
const DEFAULT_BROTLI_LEVEL: u32 = 5;
const DEFAULT_GZIP_LEVEL: u32 = 6;
const DEFAULT_DEFLATE_LEVEL: u32 = DEFAULT_GZIP_LEVEL;

const DEFAULT_WEBP_COMPRESSION: f32 = 80.0;

fn main() {
    let mut args = std::env::args_os();
    args.next();
    let indir: PathBuf = args
        .next()
        .expect("This command requires at least two arguments!")
        .into();
    let outdir: PathBuf = args
        .next()
        .expect("This command requires at least two arguments!")
        .into();
    let config = Config::default();

    let threads = std::thread::available_parallelism()
        .unwrap_or(DEFAULT_PARALLELISM)
        .get();

    let existing_files: Vec<DirEntry> = WalkDir::new(indir.clone())
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

    for item in existing_files {
        let path_display = item.path().display().to_string();
        println!("compressing file {path_display}");
        let start = Instant::now();
        if let Err(e) = process_entry(config, &indir, &outdir, item) {
            eprintln!("Error processing file {path_display}: {e}",);
        }
        let end = Instant::now();
        let duration = end.duration_since(start).as_secs_f64();
        println!("compressed {path_display} in {duration:.2} seconds");
    }
}

fn process_entry(config: Config, indir: &Path, outdir: &Path, item: DirEntry) -> Result<(), Error> {
    let ext = item.path().extension().ok_or(Error::NoExtension)?;
    match ext.as_encoded_bytes() {
        b"png" | b"jpg" | b"jpeg" | b"bmp" | b"avif" | b"webp" => {
            image_compress(config, indir, outdir, item)?
        }
        b"br" | b"gz" | b"zst" | b"zz" => {}
        _ => generic_compress(config, item, indir, outdir)?,
    }
    Ok(())
}

fn generic_compress(
    config: Config,
    item: DirEntry,
    indir: &Path,
    outdir: &Path,
) -> Result<(), Error> {
    let item_path = item.clone().into_path();
    let output_path = outdir.join(item_path.strip_prefix(indir)?);
    let mut initial = OpenOptions::new().read(true).open(&item_path)?;

    std::fs::create_dir_all(output_path.parent().unwrap_or(output_path.as_ref()))?;

    let mut br_file = create_new_extended(&output_path, "br")?;
    let mut br = BrCompressorReader::new(&mut initial, 4096, config.brotli, 20);
    std::io::copy(&mut br, &mut br_file)?;
    drop(br_file);
    initial.seek(SeekFrom::Start(0))?;

    let gz_file = create_new_extended(&output_path, "gz")?;
    let mut gz = GzBuilder::new().write(gz_file, FlateCompression::new(config.gzip));
    std::io::copy(&mut initial, &mut gz)?;
    drop(gz);
    initial.seek(SeekFrom::Start(0))?;

    let zst_file = create_new_extended(&output_path, "zst")?;
    zstd::stream::copy_encode(&mut initial, zst_file, config.zstd)?;
    initial.seek(SeekFrom::Start(0))?;

    let zz_file = create_new_extended(&output_path, "zz")?;
    let mut zz = DeflateEncoder::new(zz_file, FlateCompression::new(config.deflate));
    std::io::copy(&mut initial, &mut zz)?;
    drop(zz);
    initial.seek(SeekFrom::Start(0))?;

    std::fs::copy(item_path, output_path)?;

    Ok(())
}

fn image_compress(
    config: Config,
    indir: &Path,
    outdir: &Path,
    item: DirEntry,
) -> Result<(), Error> {
    let path = item.path();
    let output_path = outdir.join(path.strip_prefix(indir)?);
    let image = image::open(path)?;

    std::fs::create_dir_all(output_path.parent().unwrap_or(output_path.as_ref()))?;

    let webp_encoder =
        WebPEncoder::from_image(&image).map_err(|_| Error::UnimplementedWebPImageFormat)?;
    let webp_pixmap = webp_encoder.encode_simple(config.webp.lossless(), config.webp.quality())?;
    let mut webp_out = create_file(output_path.with_extension("webp"))?;
    webp_out.write_all(webp_pixmap.as_ref())?;
    drop(webp_out);

    let avif_out = create_file(output_path.with_extension("avif"))?;
    image.write_with_encoder(AvifEncoder::new(avif_out))?;

    let jpeg_out = create_file(output_path.with_extension("jpeg"))?;
    image.write_with_encoder(JpegEncoder::new(jpeg_out))?;

    let png_out = create_file(output_path.with_extension("png"))?;
    image.write_with_encoder(PngEncoder::new(png_out))?;

    Ok(())
}

fn create_file(path: impl AsRef<Path>) -> Result<File, IoError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.as_ref())
}

fn create_new_extended(path: &Path, ext: impl AsRef<OsStr>) -> Result<File, IoError> {
    let extended = add_extension(path.to_path_buf(), ext);
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

fn cfg_int<T>(name: &str, range: RangeInclusive<T>, default: T) -> T
where
    T: FromStr + Display + PartialEq + PartialOrd,
    T::Err: Debug,
{
    let level: T = std::env::var(name)
        .map(|v| {
            v.parse()
                .unwrap_or_else(|_| panic!("{name} must be a valid integer"))
        })
        .unwrap_or(default);
    if !range.contains(&level) {
        panic!(
            "{name} must be between {} and {}, inclusive.",
            range.start(),
            range.end()
        );
    }
    level
}

#[derive(Clone, Copy)]
struct Config {
    webp: WebPQualityConfig,
    brotli: u32,
    zstd: i32,
    deflate: u32,
    gzip: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            webp: Default::default(),
            zstd: cfg_int(
                "ZSTD_LEVEL",
                zstd::compression_level_range(),
                DEFAULT_ZSTD_LEVEL,
            ),
            brotli: cfg_int("BROTLI_LEVEL", 1..=11, DEFAULT_BROTLI_LEVEL),
            deflate: cfg_int("DEFLATE_LEVEL", 1..=9, DEFAULT_DEFLATE_LEVEL),
            gzip: cfg_int("GZIP_LEVEL", 1..=9, DEFAULT_GZIP_LEVEL),
        }
    }
}

#[derive(Clone, Copy)]
enum WebPQualityConfig {
    Lossless,
    Lossy(f32),
}

impl Default for WebPQualityConfig {
    fn default() -> Self {
        if std::env::var("WEBP_LOSSLESS").is_ok_and(|v| v != "false" && v != "0") {
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
            WebPQualityConfig::Lossy(DEFAULT_WEBP_COMPRESSION)
        }
    }
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
    #[error("Prefix stripping error")]
    StripPrefixError(#[from] std::path::StripPrefixError),
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

use std::{
    ffi::{OsStr, OsString},
    fmt::{Debug, Display},
    fs::{File, OpenOptions},
    io::{Error as IoError, Seek, SeekFrom, Write},
    ops::RangeInclusive,
    path::{Path, PathBuf},
    process::ExitCode,
    str::FromStr,
    sync::{atomic::AtomicBool, Arc},
    time::Instant,
};

use brotli::CompressorReader as BrCompressorReader;
use flate2::{write::DeflateEncoder, Compression as FlateCompression, GzBuilder};
use image::{
    codecs::{avif::AvifEncoder, jpeg::JpegEncoder, png::PngEncoder},
    DynamicImage, EncodableLayout, ImageError,
};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use walkdir::{DirEntry, Error as WalkDirError, WalkDir};
use webp::{Encoder as WebPEncoder, WebPEncodingError};

const DEFAULT_ZSTD_LEVEL: i32 = 7;
const DEFAULT_BROTLI_LEVEL: u32 = 5;
const DEFAULT_GZIP_LEVEL: u32 = 6;
const DEFAULT_DEFLATE_LEVEL: u32 = DEFAULT_GZIP_LEVEL;

const DEFAULT_WEBP_COMPRESSION: f32 = 80.0;

const SMALL_IMAGE_PIXELS: u32 = 256;
const MEDIUM_IMAGE_PIXELS: u32 = 512;
const LARGE_IMAGE_PIXELS: u32 = 1024;

#[derive(argh::FromArgs)]
/// A simple application to compress all web assits in a static file directory.
struct Arguments {
    #[argh(positional)]
    /// input directory.
    indir: PathBuf,
    #[argh(positional)]
    /// input directory.
    outdir: PathBuf,
    /// do you wish to supress the creation of separate files for differently sized images
    #[argh(switch)]
    no_resize_images: bool,
    /// do you wish to not touch images at all, and copy them as-is?
    #[argh(switch)]
    no_compress_images: bool,
}

fn main() -> ExitCode {
    let args: Arguments = argh::from_env();

    let config = Config::new(
        &args.indir,
        &args.outdir,
        args.no_resize_images,
        args.no_compress_images,
    );
    let failed = Arc::new(AtomicBool::new(false));
    let existing_files: Vec<DirEntry> = WalkDir::new(args.indir.clone())
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
                failed.store(true, std::sync::atomic::Ordering::Release);
                None
            }
        })
        .collect();

    existing_files.into_par_iter().for_each(|item| {
        let path_display = item.path().display().to_string();
        println!("compressing file {path_display}");
        let start = Instant::now();
        let processed = process_entry(config.clone(), item);
        let end = Instant::now();
        let duration = end.duration_since(start).as_secs_f64();
        if let Err(e) = processed {
            failed.store(true, std::sync::atomic::Ordering::Release);
            eprintln!("failed to process file {path_display}: {e} (took {duration:.2} seconds)",);
        } else {
            println!("compressed {path_display} in {duration:.2} seconds");
        }
    });
    if failed.load(std::sync::atomic::Ordering::Acquire) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn process_entry(config: Config, item: DirEntry) -> Result<(), Error> {
    let ext = item.path().extension().ok_or(Error::NoExtension)?;
    match ext.as_encoded_bytes() {
        b"png" | b"jpg" | b"jpeg" | b"bmp" | b"avif" | b"webp" => image_compress(config, item)?,
        b"br" | b"gz" | b"zst" | b"zz" => {}
        _ => generic_compress(config, item)?,
    }
    Ok(())
}

fn generic_compress(config: Config, item: DirEntry) -> Result<(), Error> {
    let item_path = item.clone().into_path();
    let output_path = config.out_dir.join(item_path.strip_prefix(config.in_dir)?);
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

fn image_compress(config: Config, item: DirEntry) -> Result<(), Error> {
    let path = item.path();
    let output_path = config.out_dir.join(path.strip_prefix(config.in_dir)?);

    std::fs::create_dir_all(output_path.parent().unwrap_or(output_path.as_ref()))?;

    if !config.no_compress_images {
        let image = image::open(path)?;

        if !config.no_resize_images {
            let small_image = image.thumbnail(SMALL_IMAGE_PIXELS, SMALL_IMAGE_PIXELS);
            let medium_image = image.thumbnail(MEDIUM_IMAGE_PIXELS, MEDIUM_IMAGE_PIXELS);
            let large_image = image.thumbnail(LARGE_IMAGE_PIXELS, LARGE_IMAGE_PIXELS);
            dynamic_render(&config, small_image, &gen_path(&output_path, "-small")?)?;
            dynamic_render(&config, medium_image, &gen_path(&output_path, "-medium")?)?;
            dynamic_render(&config, large_image, &gen_path(&output_path, "-large")?)?;
        }

        dynamic_render(&config, image, &output_path)?;
    }

    if !output_path.try_exists()? {
        std::fs::copy(path, &output_path)?;
    }

    Ok(())
}

fn gen_path(path: &Path, extra_text: &str) -> Result<PathBuf, Error> {
    let old_extension = path.extension().ok_or(Error::NoExtension)?;
    let old_name = path
        .with_extension("")
        .file_name()
        .ok_or(Error::NoFileName)?
        .to_owned();
    let mut new_file_name =
        OsString::with_capacity(old_name.len() + extra_text.len() + 1 + old_extension.len());
    new_file_name.push(old_name);
    new_file_name.push(extra_text);
    new_file_name.push(".");
    new_file_name.push(old_extension);
    Ok(path.with_file_name(new_file_name))
}

fn dynamic_render(config: &Config, image: DynamicImage, output_path: &Path) -> Result<(), Error> {
    let avif_out = create_file(output_path.with_extension("avif"))?;
    image.write_with_encoder(AvifEncoder::new(avif_out))?;

    let jpeg_out = create_file(output_path.with_extension("jpeg"))?;
    let jpeg_quality_dropped_image = image.clone().into_rgb8();
    jpeg_quality_dropped_image.write_with_encoder(JpegEncoder::new(jpeg_out))?;

    let png_out = create_file(output_path.with_extension("png"))?;
    image.write_with_encoder(PngEncoder::new(png_out))?;

    let image_rgba = image.into_rgba8();
    let webp_encoder = WebPEncoder::from_rgba(
        image_rgba.as_bytes(),
        image_rgba.width(),
        image_rgba.height(),
    );
    let webp_pixmap = webp_encoder.encode_simple(config.webp.lossless(), config.webp.quality())?;
    let mut webp_out = create_file(output_path.with_extension("webp"))?;
    webp_out.write_all(webp_pixmap.as_ref())?;
    drop(webp_out);

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
    create_file(extended)
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

#[derive(Clone)]
struct Config<'a> {
    webp: WebPQualityConfig,
    brotli: u32,
    zstd: i32,
    deflate: u32,
    gzip: u32,
    no_resize_images: bool,
    no_compress_images: bool,
    in_dir: &'a Path,
    out_dir: &'a Path,
}

impl<'a> Config<'a> {
    fn new(
        in_dir: &'a Path,
        out_dir: &'a Path,
        no_resize_images: bool,
        no_compress_images: bool,
    ) -> Self {
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
            no_resize_images,
            no_compress_images,
            in_dir,
            out_dir,
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
    #[error("Encountered a file with no name")]
    NoFileName,
    #[error("WebP does not support some dynamic image types: https://docs.rs/webp/0.2.6/src/webp/encoder.rs.html#29-45")]
    UnimplementedWebPImageFormat,
}

impl From<WebPEncodingError> for Error {
    fn from(value: WebPEncodingError) -> Self {
        Self::WebP(value)
    }
}

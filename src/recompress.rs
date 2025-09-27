//! This takes care of mapping our input set of tarballs to the output set of tarballs.
//!
//! Currently rust-lang/rust CI produces .xz tarballs with moderate compression, and this module
//! maps that into the following:
//!
//! * gzip tarballs, with compression=9
//! * xz tarballs, with manually tuned compression settings
//!
//! We have ~500 tarballs as of March 2023, and this recompression takes a considerable amount of
//! time, particularly for the xz outputs. In our infrastructure this runs on a 72 vCPU container to
//! finish in a reasonable amount of time.

/// The maximum XZ dictionary size we're willing to choose. Rustup users will
/// need at least this much free RAM to decompress the archive, and
/// compression will require even more memory.
const MAX_XZ_DICTSIZE: u32 = 128 * 1024 * 1024;

use crate::Context;
use anyhow::Context as _;
use std::convert::TryFrom;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::Path;
use std::time::{Duration, Instant};
use xz2::read::XzDecoder;

pub(crate) fn recompress_file(
    xz_path: &Path,
    recompress_gz: bool,
    gz_compression_level: flate2::Compression,
    recompress_xz: bool,
) -> anyhow::Result<()> {
    println!("recompressing {}...", xz_path.display());
    let file_start = Instant::now();
    let gz_path = xz_path.with_extension("gz");

    let mut in_file = File::open(xz_path).with_context(|| "failed to open XZ-compressed input")?;
    let mut dec_buf = vec![0u8; 4 * 1024 * 1024];
    let mut compression_times = String::new();

    let mut dec_measurements = None;

    // Produce gzip if explicitly enabled or the destination file doesn't exist.
    if recompress_gz || !gz_path.is_file() {
        let gz_out = File::create(gz_path)?;
        let mut gz_encoder = flate2::write::GzEncoder::new(gz_out, gz_compression_level);
        let mut gz_duration = Duration::ZERO;
        dec_measurements = Some(decompress_and_write(
            &mut in_file,
            &mut dec_buf,
            &mut [("gz", &mut gz_encoder, &mut gz_duration)],
        )?);
        format_compression_time(&mut compression_times, "gz", gz_duration, None)?;
    };

    // xz recompression with more aggressive settings than we want to take the time
    // for in rust-lang/rust CI. This cuts 5-15% off of the produced tarballs.
    //
    // Note that this is using a single-threaded compressor as we're parallelizing
    // via rayon already. In rust-lang/rust we were trying to use parallel
    // compression, but the default block size for that is 3*dict_size so we
    // weren't actually using more than one core in most of the builders with
    // <192MB uncompressed tarballs. In promote-release since we're recompressing
    // 100s of tarballs there's no need for each individual compression to be
    // parallel.
    let xz_recompressed = xz_path.with_extension("xz_recompressed");
    if recompress_xz {
        let in_size = match dec_measurements {
            Some((_, size)) => size,
            None => measure_compressed_file(&mut in_file, &mut dec_buf)?.1,
        };
        let dictsize = choose_xz_dictsize(u32::try_from(in_size).unwrap_or(u32::MAX));

        let mut filters = xz2::stream::Filters::new();
        let mut lzma_ops = xz2::stream::LzmaOptions::new_preset(9).unwrap();
        // This sets the overall dictionary size, which is also how much memory (baseline)
        // is needed for decompression.
        lzma_ops.dict_size(dictsize);
        // Use the best match finder for compression ratio.
        lzma_ops.match_finder(xz2::stream::MatchFinder::BinaryTree4);
        lzma_ops.mode(xz2::stream::Mode::Normal);
        // Set nice len to the maximum for best compression ratio
        lzma_ops.nice_len(273);
        // Set depth to a reasonable value, 0 means auto, 1000 is somwhat high but gives
        // good results.
        lzma_ops.depth(1000);
        // 2 is the default and does well for most files
        lzma_ops.position_bits(2);
        // 0 is the default and does well for most files
        lzma_ops.literal_position_bits(0);
        // 3 is the default and does well for most files
        lzma_ops.literal_context_bits(3);

        filters.lzma2(&lzma_ops);

        // FIXME: Do we want a checksum as part of compression?
        let stream =
            xz2::stream::Stream::new_stream_encoder(&filters, xz2::stream::Check::None).unwrap();

        let xz_out = File::create(&xz_recompressed)?;
        let mut xz_encoder = xz2::write::XzEncoder::new_stream(io::BufWriter::new(xz_out), stream);
        let mut xz_duration = Duration::ZERO;
        dec_measurements = Some(decompress_and_write(
            &mut in_file,
            &mut dec_buf,
            &mut [("xz", &mut xz_encoder, &mut xz_duration)],
        )?);
        format_compression_time(&mut compression_times, "xz", xz_duration, Some(dictsize))?;
    }

    drop(in_file);

    print!(
        "recompressed {}: {:.2?} total",
        xz_path.display(),
        file_start.elapsed()
    );
    if let Some((decompress_time, _)) = dec_measurements {
        print!(" {:.2?} decompression", decompress_time);
    }
    println!("{}", compression_times);

    if recompress_xz {
        fs::rename(&xz_recompressed, xz_path)?;
    }

    Ok(())
}

/// Decompresses the given XZ stream and sends it to the given set of destinations.
/// Writes the time taken by each individual destination to the corresponding tuple
/// and returns the total time taken by the decompressor and the total size of the
/// decompressed stream.
fn decompress_and_write(
    src: &mut (impl Read + Seek),
    buf: &mut [u8],
    destinations: &mut [(&str, &mut dyn Write, &mut Duration)],
) -> anyhow::Result<(Duration, u64)> {
    src.rewind().with_context(|| "input file seek failed")?;
    let mut decompressor = XzDecoder::new(src);
    let mut decompress_time = Duration::ZERO;
    let mut total_length = 0_u64;
    loop {
        let start = Instant::now();
        let length = decompressor
            .read(buf)
            .with_context(|| "XZ decompression failed")?;
        decompress_time += start.elapsed();
        total_length += length as u64;
        if length == 0 {
            break;
        }
        // This code assumes that compression with `write_all` will never fail (i.e.,
        // we can take arbitrary amounts of data as input). That seems like a
        // reasonable assumption though.
        for (compname, destination, duration) in destinations.iter_mut() {
            let start = std::time::Instant::now();
            destination
                .write_all(&buf[..length])
                .with_context(|| format!("{compname} compression failed"))?;
            **duration += start.elapsed();
        }
    }
    Ok((decompress_time, total_length))
}

/// Calls `decompress_and_write` solely to measure the file's uncompressed size
/// and the time taken by decompression.
fn measure_compressed_file(
    src: &mut (impl Read + Seek),
    buf: &mut [u8],
) -> anyhow::Result<(Duration, u64)> {
    decompress_and_write(src, buf, &mut [])
}

fn format_compression_time(
    out: &mut String,
    name: &str,
    duration: Duration,
    dictsize: Option<u32>,
) -> std::fmt::Result {
    write!(out, ", {:.2?} {} compression", duration, name)?;
    if let Some(mut dictsize) = dictsize {
        let mut iprefix = 0;
        // Divide by 1024 until the result would be inexact or we run out of prefixes.
        while iprefix < 2 && dictsize.is_multiple_of(1024) {
            iprefix += 1;
            dictsize /= 1024;
        }
        write!(
            out,
            " with {dictsize} {}B dictionary",
            ["", "Ki", "Mi"][iprefix]
        )?;
    }
    Ok(())
}

/// Chooses the smallest XZ dictionary size that is at least as large as the
/// file and will not be rounded by XZ, clipping it to the range of acceptable
/// dictionary sizes.
///
/// XZ's dictionary sizes are the sum of one or two powers of two. As such, this
/// function amounts to finding for some `sz` the smallest integer `d` which
/// upholds all of the following properties:
/// - has the form `2^n` or `2^n + 2^(n-1)`
/// - `d` ≥ minimum XZ dictionary size
/// - `d` ≤ maximum XZ dictionary size
/// - `d` ≥ `sz`, but only if `sz` ≤ maximum XZ dictionary size
fn choose_xz_dictsize(mut sz: u32) -> u32 {
    /// XZ's minimum dictionary size, which is 4 KiB.
    const MIN_XZ_DICTSIZE: u32 = 4096;
    const {
        // This check is to prevent overflow further down the line
        // regardless of the value of MAX_XZ_DICTSIZE.
        assert!(
            MAX_XZ_DICTSIZE <= (1024 + 512) * 1024 * 1024,
            "XZ dictionary size only goes up to 1.5 GiB"
        );
    };
    sz = sz.clamp(MIN_XZ_DICTSIZE, MAX_XZ_DICTSIZE);
    if sz.is_power_of_two() {
        return sz;
    }

    // Copypasted from u32::isolate_most_significant_one() 'cause it's unstable.
    let hi_one = sz & (1_u32 << 31).wrapping_shr(sz.leading_zeros());

    // For a bitstring of the form 01x…, check if 0110…0 (the 2^n + 2^(n-1) form) is
    // greater or equal. For example, for sz = 17M (16M + 1M), hi_one will be 16M and
    // twinbit_form will be 24M (16M + 8M) and the check will succeed, whereas for
    // sz = 25M (16M + 8M + 1M), twinbit_form will also be 24M (16M + 8M) and the check
    // will fail.
    let twinbit_form = hi_one | (hi_one >> 1);
    if twinbit_form >= sz {
        return twinbit_form;
    }

    // Otherwise, we go for the next power of two.
    std::cmp::min(hi_one << 1, MAX_XZ_DICTSIZE)
}

impl Context {
    pub fn recompress(&self, directory: &Path) -> anyhow::Result<()> {
        let mut to_recompress = Vec::new();
        for file in directory.read_dir()? {
            let file = file?;
            let path = file.path();
            match path.extension().and_then(|s| s.to_str()) {
                // Store off the input files for potential recompression.
                Some("xz") => {
                    to_recompress.push(path.to_path_buf());
                }
                Some("gz") if self.config.recompress_gz => {
                    fs::remove_file(&path)?;
                }
                _ => {}
            }
        }

        println!(
            "starting to recompress {} files across {} threads",
            to_recompress.len(),
            to_recompress.len().min(self.config.num_threads),
        );
        println!(
            "gz recompression enabled: {} (note: may occur anyway for missing gz artifacts)",
            self.config.recompress_gz
        );
        println!("xz recompression enabled: {}", self.config.recompress_xz);
        let recompress_start = Instant::now();

        let recompress_gz = self.config.recompress_gz;
        let recompress_xz = self.config.recompress_xz;
        let compression_level = flate2::Compression::new(self.config.gzip_compression_level);

        // Query the length of each file, and sort by length. This puts the smallest files
        // toward the start of the array, which will make us pop them last. Smaller units of work
        // are less likely to lead to a long tail of a single thread doing work while others are
        // idle, so we want to schedule them last (i.e., in the tail of the build).
        to_recompress.sort_by_cached_key(|path| fs::metadata(path).map(|m| m.len()).unwrap_or(0));

        let total_length = to_recompress.len();

        // Manually parallelize across freshly spawned worker threads. rayon is nice, but since we
        // care about the scheduling order and have very large units of work (>500ms, typically 10s
        // of seconds) the more efficient parallelism in rayon isn't desirable. (Scheduling order
        // is the particular problem for us).
        let to_recompress = std::sync::Mutex::new(to_recompress);
        std::thread::scope(|s| {
            // Spawn num_threads workers...
            let mut tasks = Vec::new();
            for _ in 0..self.config.num_threads {
                tasks.push(s.spawn(|| {
                    while let Some(xz_path) = {
                        // Extra block is needed to make sure the lock guard drops before we enter the
                        // loop iteration, because while-let is desugared to a loop + match, and match
                        // scopes live until the end of the match.
                        let path = to_recompress.lock().unwrap().pop();
                        path
                    } {
                        recompress_file(&xz_path, recompress_gz, compression_level, recompress_xz)
                            .with_context(|| {
                                format!("failed to recompress {}", xz_path.display())
                            })?;
                    }

                    Ok::<_, anyhow::Error>(())
                }));
            }

            for task in tasks {
                task.join().expect("no panics")?;
            }

            Ok::<_, anyhow::Error>(())
        })?;

        println!(
            "finished recompressing {} files in {:.2?}",
            total_length,
            recompress_start.elapsed(),
        );
        Ok(())
    }
}

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

use crate::Context;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File};
use std::io::{self, Read, Write};
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

    let mut destinations: Vec<(&str, Box<dyn io::Write>)> = Vec::new();

    // Produce gzip if explicitly enabled or the destination file doesn't exist.
    if recompress_gz || !gz_path.is_file() {
        let gz = File::create(gz_path)?;
        destinations.push((
            "gz",
            Box::new(flate2::write::GzEncoder::new(gz, gz_compression_level)),
        ));
    }

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
        let mut filters = xz2::stream::Filters::new();
        let mut lzma_ops = xz2::stream::LzmaOptions::new_preset(9).unwrap();
        // This sets the overall dictionary size, which is also how much memory (baseline)
        // is needed for decompression.
        lzma_ops.dict_size(128 * 1024 * 1024);
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
        destinations.push((
            "xz",
            Box::new(xz2::write::XzEncoder::new_stream(
                std::io::BufWriter::new(xz_out),
                stream,
            )),
        ));
    }

    // We only decompress once and then write into each of the compressors before
    // moving on.
    //
    // This code assumes that compression with `write_all` will never fail (i.e., we
    // can take arbitrary amounts of data as input). That seems like a reasonable
    // assumption though.
    let mut decompressor = XzDecoder::new(File::open(xz_path)?);
    let mut buffer = vec![0u8; 4 * 1024 * 1024];
    let mut decompress_time = Duration::ZERO;
    let mut time_by_dest = vec![Duration::ZERO; destinations.len()];
    loop {
        let start = Instant::now();
        let length = decompressor.read(&mut buffer)?;
        decompress_time += start.elapsed();
        if length == 0 {
            break;
        }
        for (idx, (_, destination)) in destinations.iter_mut().enumerate() {
            let start = std::time::Instant::now();
            destination.write_all(&buffer[..length])?;
            time_by_dest[idx] += start.elapsed();
        }
    }

    let mut compression_times = String::new();
    for (idx, (name, _)) in destinations.iter().enumerate() {
        write!(
            compression_times,
            ", {:.2?} {} compression",
            time_by_dest[idx], name
        )?;
    }
    println!(
        "recompressed {}: {:.2?} total, {:.2?} decompression{}",
        xz_path.display(),
        file_start.elapsed(),
        decompress_time,
        compression_times
    );

    if recompress_xz {
        fs::rename(&xz_recompressed, xz_path)?;
    }

    Ok(())
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
                        recompress_file(&xz_path, recompress_gz, compression_level, recompress_xz)?;
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

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
use rayon::prelude::*;
use std::fs::{self, File};
use std::io;
use std::path::PathBuf;
use std::time::Instant;
use xz2::read::XzDecoder;

impl Context {
    pub fn recompress(&self, to_recompress: &[PathBuf]) -> anyhow::Result<()> {
        println!(
            "starting to recompress {} files across {} threads",
            to_recompress.len(),
            to_recompress.len().min(rayon::current_num_threads()),
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

        to_recompress
            .par_iter()
            .map(|xz_path| {
                println!("recompressing {}...", xz_path.display());
                let gz_path = xz_path.with_extension("gz");

                // Produce gzip if explicitly enabled or the destination file doesn't exist.
                if recompress_gz || !gz_path.is_file() {
                    let mut xz_orig = XzDecoder::new(File::open(xz_path)?);
                    let gz = File::create(gz_path)?;
                    let mut gz = flate2::write::GzEncoder::new(gz, compression_level);
                    io::copy(&mut xz_orig, &mut gz)?;
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
                if recompress_xz {
                    let mut filters = xz2::stream::Filters::new();
                    let mut lzma_ops = xz2::stream::LzmaOptions::new_preset(9).unwrap();
                    // This sets the overall dictionary size, which is also how much memory (baseline)
                    // is needed for decompression.
                    lzma_ops.dict_size(64 * 1024 * 1024);
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
                        xz2::stream::Stream::new_stream_encoder(&filters, xz2::stream::Check::None)
                            .unwrap();
                    let xz_recompressed = xz_path.with_extension("xz_recompressed");
                    let xz_out = File::create(&xz_recompressed)?;
                    let mut xz_out =
                        xz2::write::XzEncoder::new_stream(std::io::BufWriter::new(xz_out), stream);
                    let mut xz_orig = XzDecoder::new(File::open(xz_path)?);
                    io::copy(&mut xz_orig, &mut xz_out)?;
                    fs::rename(&xz_recompressed, xz_path)?;
                }

                Ok::<(), anyhow::Error>(())
            })
            .collect::<anyhow::Result<Vec<()>>>()?;

        println!(
            "finished recompressing {} files in {:.2?}",
            to_recompress.len(),
            recompress_start.elapsed(),
        );
        Ok(())
    }
}

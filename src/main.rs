extern crate core;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use crossbeam_channel::Receiver;
use memchr::memchr_iter;
use memchr::memmem::Finder;
use std::ffi::OsString;
use std::fs::File;
use std::io::{stdin, Read};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "freq - count the occurrences of a literal pattern")]
struct Args {
    #[arg(required = true, help = "The pattern to search for.")]
    /// The pattern to search for.
    pattern: OsString,

    #[arg(help = "The files to search in. If not provided, stdin is used.")]
    input: Vec<PathBuf>,

    #[clap(
        short,
        long,
        default_value = "1048576",
        help = "The size of the buffer used to read the file. Larger buffers use more memory, but might be faster."
    )]
    buffer_size: usize,
}

struct NeedleCounter {
    // The needle we are looking for.
    needle: Vec<u8>,

    // How many needles we have found.
    count: usize,

    // For holding intermediate data.
    // We keep it around to avoid reallocating it.
    // It is at most n - 1 bytes long.
    tmp_buf: Vec<u8>,

    // The searcher we use to find needles.
    finder: Finder<'static>,
}

impl NeedleCounter {
    pub fn new(needle: &[u8]) -> Self {
        NeedleCounter {
            needle: needle.to_vec(),
            count: 0,
            tmp_buf: Vec::new(),
            finder: Finder::new(needle).into_owned(),
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    fn write(&mut self, buf: Vec<u8>) {
        let n = self.needle.len();

        // Fast case - if the needle has length 1 we can use a simd loop.
        if n == 1 {
            let b = self.needle[0];
            self.count += buf.iter().filter(|&&x| x == b).count();
            return;
        }

        // How many bytes we need to remove from the tmp buffer to get into the new buffer.
        let mut to_remove = self.tmp_buf.len();

        // The number of bytes in the buffer that we have moved to the tmp buffer.
        let mut num_buf_bytes = 0;

        while to_remove > 0 && num_buf_bytes < buf.len() {
            // Add into the tmp buffer until it is at most 2 * n - 1 bytes long.
            let x_len = self.tmp_buf.len();
            let num_buf_bytes_left = buf.len() - num_buf_bytes;
            let y_len = (2 * n - 1).saturating_sub(x_len).min(num_buf_bytes_left);
            let y = &buf[num_buf_bytes..num_buf_bytes + y_len];
            num_buf_bytes += y.len();
            self.tmp_buf.extend(y);

            // Check for a needle in the tmp buffer.
            // This will also count the needle if it is there.
            let cut = self.find_in_tmp_buf();

            // Remove any bytes that are before the next needle.
            self.tmp_buf.drain(..cut);
            to_remove = to_remove.saturating_sub(cut);
        }

        if num_buf_bytes == buf.len() {
            return;
        }

        num_buf_bytes -= self.tmp_buf.len();
        self.tmp_buf.clear();
        // Now we can search the rest of the new buffer for the needle.
        let next_buffer_cut = self.find_in(&buf[num_buf_bytes..]) + num_buf_bytes;

        // Move the rest of the buffer to the temporary buffer.
        self.tmp_buf.extend(&buf[next_buffer_cut..]);
    }

    // Count needles in the buffer.
    // Returns the largest index i such that buf[..i] does not contain any needles.
    fn find_in(&mut self, buf: &[u8]) -> usize {
        if buf.len() < self.needle.len() {
            return 0;
        }

        let n = self.needle.len();
        let mut x = buf.len() - n + 1;
        for i in self.finder.find_iter(buf) {
            self.count += 1;
            x = x.max(i + n);
        }

        first_possible_prefix(&self.needle, &buf[x..]) + x
    }

    // Count needles in the temporary buffer, exploiting its construction.
    // Returns the largest index i such that tmp_buf[..i] does not contain any needles.
    fn find_in_tmp_buf(&mut self) -> usize {
        // Here we can take advantage of the fact that the tmp buffer is at most 2 * n - 1 bytes long.
        // It follows:
        //  - The tmp buffer contains at most one needle.
        //  - That needle must be in the first (t - n) bytes of the tmp buffer.
        let n = self.needle.len();
        let mut l = self.tmp_buf.len().saturating_sub(n - 1);

        if self.tmp_buf.len() < n {
            return first_possible_prefix(&self.needle, &self.tmp_buf);
        }

        // We use memchr instead of the finder here because the tmp buf isn't very large.
        for i in memchr_iter(self.needle[0], &self.tmp_buf[..l]) {
            if self.tmp_buf[i..].starts_with(&self.needle) {
                // We found a needle!
                self.count += 1;

                // We can skip to the next possible needle.
                // The next loop will handle that.
                l = l.max(i + n);
                break;
            }
        }

        first_possible_prefix(&self.needle, &self.tmp_buf[l..]) + l
    }
}

pub fn first_possible_prefix(needle: &[u8], buf: &[u8]) -> usize {
    const N: usize = 16;
    (0..buf.len())
        .filter(|&i| {
            buf[i..]
                .chunks(N)
                .zip(needle.chunks(N))
                .all(|(x, y)| x.iter().zip(y).fold(true, |acc, (a, b)| acc & (a == b)))
        })
        .min()
        .unwrap_or(buf.len())
}

fn get_uninit_vec<T>(len: usize) -> Vec<T> {
    let mut v = Vec::with_capacity(len);
    unsafe {
        v.set_len(len);
    }
    v
}

fn read_chunks<R: Read + Send + 'static>(mut v: Vec<R>, chunk_size: usize) -> Receiver<Vec<u8>> {
    let (s, r) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        v.iter_mut().for_each(|f| {
            loop {
                // Get a buffer.
                let mut v = get_uninit_vec(chunk_size);

                // Try to fill the buffer.
                let bytes_read = f.read(&mut v).expect("failed to read");

                // If we read 0 bytes, we are done.
                if bytes_read == 0 {
                    break;
                }

                // Send the buffer.
                v.truncate(bytes_read);
                s.send(v).expect("failed to send");
            }
        });
        // Sender drops.
    });
    r
}

fn main() {
    let args = Args::parse();

    let needle = args.pattern.as_encoded_bytes();
    if needle.is_empty() {
        let mut cmd = Args::command();
        cmd.error(ErrorKind::ValueValidation, "Pattern must be non-empty")
            .exit();
    }

    let r = if args.input.is_empty() {
        let stdin = stdin();
        read_chunks(vec![stdin], args.buffer_size)
    } else {
        let v = args
            .input
            .iter()
            .map(|p| {
                File::open(p.clone()).expect(format!("failed to open {}", p.display()).as_str())
            })
            .collect::<Vec<_>>();
        read_chunks(v, args.buffer_size)
    };

    // Counting happens in this thread.
    let mut counter = NeedleCounter::new(needle);
    while let Ok(v) = r.recv() {
        counter.write(v);
    }
    println!("{}", counter.count());
}

#[cfg(test)]
mod tests {
    use super::*;

    use memchr::memmem::find_iter;
    use proptest::prelude::ProptestConfig;
    use proptest::string::bytes_regex;
    use proptest::{prop_assert_eq, proptest};

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 1 << 16,
            .. ProptestConfig::default()
        })]

        #[test]
        fn test_count(
            chunk_size in 1..100_usize,
            needle in bytes_regex("((?s-u:.{1,100}))").unwrap(),
            haystack in bytes_regex("((?s-u:.{0,1000}))").unwrap()
        ) {
            let mut counter = NeedleCounter::new(&needle);

            haystack.chunks(chunk_size).for_each(|chunk| {
                counter.write(chunk.to_vec());
            });


            let expected = find_iter(&haystack, &needle).count();
            assert_eq!(counter.count(), expected);
        }

        #[test]
        fn test_aba(
            chunk_size in 1..100_usize,
            needle in bytes_regex("((?s-u:[ab]{1,10}))").unwrap(),
            haystack in bytes_regex("((?s-u:[ab]{0,1000}))").unwrap()
        ) {
            let mut counter = NeedleCounter::new(&needle);

            haystack.chunks(chunk_size).for_each(|chunk| {
                counter.write(chunk.to_vec());
            });


            let expected = find_iter(&haystack, &needle).count();
            prop_assert_eq!(counter.count(), expected);
        }
    }
}

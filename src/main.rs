extern crate core;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use crossbeam_channel::Receiver;
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

    fn write(&mut self, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }

        let n = self.needle.len();

        // Fast case - if the needle has length 1 we can use a simd loop.
        if n == 1 {
            let b = self.needle[0];
            self.count += bytecount::count(&buf, b);
            return;
        }

        // The number of bytes in the buffer that we have moved to the tmp buffer.
        let mut num_buf_bytes = 0;

        if !self.tmp_buf.is_empty() {
            // Add into the tmp buffer until it is at most 2 * n - 1 bytes long.
            let y_len = (2 * n - 1)
                .saturating_sub(self.tmp_buf.len())
                .min(buf.len());
            let y = &buf[..y_len];
            num_buf_bytes = y_len;
            self.tmp_buf.extend(y);

            // Check for a needle in the tmp buffer.
            // This will also count the needle if it is there.
            let (cut, c) = self.find_in(&self.tmp_buf);
            self.count += c;

            // Remove any bytes that are before the next needle.
            self.tmp_buf.drain(..cut);
        }

        if num_buf_bytes == buf.len() {
            return;
        }

        num_buf_bytes -= self.tmp_buf.len();
        self.tmp_buf.clear();
        // Now we can search the rest of the new buffer for the needle.
        let (mut next_buffer_cut, c) = self.find_in(&buf[num_buf_bytes..]);
        self.count += c;
        next_buffer_cut += num_buf_bytes;

        // Move the rest of the buffer to the temporary buffer.
        self.tmp_buf.extend(&buf[next_buffer_cut..]);
    }

    // Count needles in the buffer.
    // Returns (i, c) where `i` is the largest index such that `buf[..i]` does not contain any
    // needles, and `c` is the number of needles found.
    fn find_in(&self, buf: &[u8]) -> (usize, usize) {
        let n = self.needle.len();
        let mut x = 0;
        let mut count = 0;
        while let Some(i) = self.finder.find(&buf[x..]) {
            count += 1;
            x += i + n;
        }

        let l = buf.len().saturating_sub(n - 1).max(x);
        let i = first_possible_prefix(&self.needle, &buf[l..]) + l;
        (i, count)
    }
}

pub fn first_possible_prefix(needle: &[u8], buf: &[u8]) -> usize {
    (0..buf.len())
        .filter(|&i| needle.starts_with(&buf[i..]))
        .next()
        .unwrap_or(buf.len())
}

fn get_uninit_vec<T>(len: usize) -> Vec<T> {
    let mut v = Vec::with_capacity(len);
    unsafe {
        v.set_len(len);
    }
    v
}

fn read_chunks<R: Read + Send + 'static>(mut f: R, chunk_size: usize) -> Receiver<Vec<u8>> {
    let (s, r) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
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

    let v: Vec<Box<dyn Read + Send + 'static>> = if args.input.is_empty() {
        vec![Box::new(stdin())]
    } else {
        args
            .input
            .iter()
            .map(|p| {
                File::open(p.clone()).expect(format!("failed to open {}", p.display()).as_str())
            })
            .map(|f| Box::new(f) as _)
            .collect()
    };

    // Counting happens in this thread.
    let mut total_count = 0;
    for f in v {
        let r = read_chunks(f, args.buffer_size);
        let mut counter = NeedleCounter::new(needle);
        while let Ok(v) = r.recv() {
            counter.write(&v);
        }
        total_count += counter.count();
    }
    println!("{}", total_count);
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
                counter.write(chunk);
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
                counter.write(chunk);
            });


            let expected = find_iter(&haystack, &needle).count();
            prop_assert_eq!(counter.count(), expected);
        }
    }
}

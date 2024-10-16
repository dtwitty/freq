use clap::Parser;
use memchr::memmem::Finder;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::File;
use std::io::{stdin, Read};
use std::path::PathBuf;
use crossbeam_channel::Receiver;

#[derive(Parser, Clone)]
struct Args {
    pattern: OsString,

    #[clap()]
    input: Option<PathBuf>,

    #[clap(short, long, default_value = "1048576")]
    buffer_size: usize,
}

struct NeedleCounter {
    needle: Vec<u8>,
    buffer_size: usize,
    count: usize,
    buffer: VecDeque<u8>,
    finder: Finder<'static>,
}

impl NeedleCounter {
    pub fn new(needle: &[u8], buffer_size: usize) -> Self {
        if needle.len() > buffer_size {
            panic!("needle is longer than buffer_size");
        }

        NeedleCounter {
            buffer_size,
            needle: needle.to_vec(),
            count: 0,
            // Invariant: the buffer does not contain any instance of the needle.
            buffer: VecDeque::with_capacity(buffer_size),
            finder: Finder::new(needle).into_owned(),
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    fn write(&mut self, buf: &[u8]) {
        let mut buf = buf;
        while !buf.is_empty() {
            // Take a chunk out of the haystack up to the maximum buffer size.
            let chunk_size = self
                .buffer_size
                .saturating_sub(self.buffer.len())
                .min(buf.len());
            let (chunk, rest) = buf.split_at(chunk_size);
            buf = rest;

            // Append the chunk to the buffer.
            self.buffer.extend(chunk);

            // Push everything in the buffer to the front.
            self.buffer.make_contiguous();

            let (buffer, _) = self.buffer.as_slices();

            // Search for the needle in the buffer.
            let n = self.needle.len();
            let mut cut_at = buffer.len().saturating_sub(n);
            for i in self.finder.find_iter(buffer) {
                self.count += 1;
                cut_at = cut_at.max(i + n);
            }
            self.buffer.drain(..cut_at);
        }
    }
}

fn get_uninit_vec<T>(len: usize) -> Vec<T> {
    let mut v = Vec::with_capacity(len);
    unsafe {
        v.set_len(len);
    }
    v
}

fn read_chunks<R: Read + Send + 'static>(f: R, chunk_size: usize) -> Receiver<Vec<u8>> {
    let (s, r) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let mut f = f;
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

    let r = match args.input {
        Some(f) if f != PathBuf::from("-") => {
            let f = File::open(f.clone()).expect(format!("failed to open {}", f.display()).as_str());
            read_chunks(f, args.buffer_size)
        }
        _ => {
            let stdin = stdin();
            read_chunks(stdin, args.buffer_size)
        }
    };

    // Counting happens in this thread.
    let mut counter = NeedleCounter::new(needle, args.buffer_size);
    while let Ok(v) = r.recv() {
        counter.write(&v);
    }
    println!("{}", counter.count());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_needle_counter() {
        let needle = b"needle";
        let buffer_size = 10;
        let mut counter = NeedleCounter::new(needle, buffer_size);

        counter.write(b"haystackneedle");
        assert_eq!(counter.count(), 1);

        counter.write(b"haystackneedlehaystackneedle");
        assert_eq!(counter.count(), 3);

        counter.write(b"haystackneedlehaystackneedlehaystackneedle");
        assert_eq!(counter.count(), 6);

        counter.write(b"haystackneedlehaystackneedlehaystackneedlehaystackneedle");
        assert_eq!(counter.count(), 10);

        counter.write(b"need");
        assert_eq!(counter.count(), 10);

        counter.write(b"le");
        assert_eq!(counter.count(), 11);
    }

    #[test]
    fn test_needle_counter_overlap() {
        let needle = "aba";
        let buffer_size = 20;

        let mut counter = NeedleCounter::new(needle.as_bytes(), buffer_size);

        for i in 1..1000 {
            counter.write(b"ab");
            assert_eq!(counter.count(), i / 2);
        }
    }
}

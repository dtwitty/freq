use clap::Parser;
use crossbeam_channel::Receiver;
use memchr::memmem::Finder;
use std::ffi::OsString;
use std::fs::File;
use std::io::{stdin, Read};
use std::path::PathBuf;

#[derive(Parser, Clone)]
struct Args {
    pattern: OsString,

    #[clap()]
    input: Option<PathBuf>,

    #[clap(short, long, default_value = "1048576")]
    buffer_size: usize,
}

struct NeedleCounter {
    // The needle we are looking for.
    needle: Vec<u8>,

    // How many needles we have found.
    count: usize,

    // The previous buffer we searched.
    prev_buffer: Vec<u8>,

    // How far into the previous buffer we are sure contains no needles.
    // That is, prev_buffer[..prev_buffer_cut] does not contain any needles.
    // After each `write()` call, `prev_buffer.len() - prev_buffer_cut` is at most `needle.len() - 1`
    prev_buffer_cut: usize,

    // For holding intermediate data.
    // We keep it around so we don't have to keep allocating it.
    tmp_buf: Vec<u8>,

    // The searcher we use to find needles.
    finder: Finder<'static>,
}

impl NeedleCounter {
    pub fn new(needle: &[u8]) -> Self {
        NeedleCounter {
            needle: needle.to_vec(),
            count: 0,
            // Invariant: the buffer does not contain any instance of the needle.
            prev_buffer: Vec::new(),
            prev_buffer_cut: 0,
            tmp_buf: Vec::new(),
            finder: Finder::new(needle).into_owned(),
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    fn write(&mut self, buf: Vec<u8>) {
        let n = self.needle.len();

        // Construct z, which has length 2 * n - 1, and is the concatenation of the last bytes of
        // the previous buffer and the first n bytes of the current buffer.
        let x = &self.prev_buffer[self.prev_buffer_cut..];
        let x_len = x.len();
        let y_len = (2 * n - 1 - x.len()).min(buf.len());
        let y = &buf[..y_len];
        self.tmp_buf.clear();
        self.tmp_buf.extend(x);
        self.tmp_buf.extend(y);
        assert!(self.tmp_buf.len() <= 2 * n - 1);

        // See if z contains a needle.
        // By construction, z contains at most one needle.
        let next_buffer_cut = self.find_in_tmp_buf() - x_len;

        // Now we can search the rest of the new buffer for the needle.
        let next_buffer_cut = self.find_in(&buf[next_buffer_cut..]) + next_buffer_cut;

        // Update the previous buffer.
        self.prev_buffer = buf;
        self.prev_buffer_cut = next_buffer_cut;
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
        x
    }

    // Count needles in the temporary buffer.
    // Returns the largest index i such that tmp_buf[..i] does not contain any needles.
    fn find_in_tmp_buf(&mut self) -> usize {
        if self.tmp_buf.len() < self.needle.len() {
            return 0;
        }

        let n = self.needle.len();
        let mut x = self.tmp_buf.len() - n + 1;
        if let Some(i) = self.finder.find(&self.tmp_buf) {
            self.count += 1;
            x = x.max(i + n);
        }
        x
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
            let f =
                File::open(f.clone()).expect(format!("failed to open {}", f.display()).as_str());
            read_chunks(f, args.buffer_size)
        }
        _ => {
            let stdin = stdin();
            read_chunks(stdin, args.buffer_size)
        }
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

    #[test]
    fn test_needle_counter() {
        let needle = b"needle";
        let mut counter = NeedleCounter::new(needle);

        counter.write(b"haystackneedle".to_vec());
        assert_eq!(counter.count(), 1);

        counter.write(b"haystackneedlehaystackneedle".to_vec());
        assert_eq!(counter.count(), 3);

        counter.write(b"haystackneedlehaystackneedlehaystackneedle".to_vec());
        assert_eq!(counter.count(), 6);

        counter.write(b"haystackneedlehaystackneedlehaystackneedlehaystackneedle".to_vec());
        assert_eq!(counter.count(), 10);

        counter.write(b"need".to_vec());
        assert_eq!(counter.count(), 10);

        counter.write(b"le".to_vec());
        assert_eq!(counter.count(), 11);
    }

    #[test]
    fn test_needle_counter_overlap() {
        let needle = "aba";

        let mut counter = NeedleCounter::new(needle.as_bytes());

        for i in 1..1000 {
            counter.write(b"ab".to_vec());
            assert_eq!(counter.count(), i / 2);
        }
    }
}

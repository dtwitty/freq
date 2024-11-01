# freq - A CLI for counting occurrences

`freq` counts the number of non-overlapping substrings in a file or stdin.

It was written when `grep -F <PATTERN> | wc -l` was found to be too slow for counting strings in multi-GB log files.
`freq` is also typically faster than `wc -l` for counting the lines in a file.

Depending on the exact inputs used, `freq` is usually IO-bound. It uses several tricks to increase performance:
  - Input is read in a separate thread, and aggressively buffered.
  - The `bytecount` crate is used for single-character patterns.
  - The `memchr` crate (specifically `memchr::memmem`) is used for processing longer patterns.

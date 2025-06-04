//! no-std utils 
use alloc::{fmt, vec::Vec};
use crate::error::InternalError;

/// Alternative for `std::io::Write`
pub trait Write {
    /// Same as `std::io::Write::write`
    fn write(&mut self, buf: &[u8]) -> Result<usize, InternalError>;

    /// Same as `std::io::Write::write_all`
    fn write_all(&mut self, mut buf: &[u8]) -> Result<(), InternalError> {
        while !buf.is_empty() {
            match self.write(buf) {
                Ok(0) => {
                    return Err(InternalError::WriteAllEof);
                }
                Ok(n) => buf = &buf[n..],
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Same as `std::io::Write::write_fmt`
    fn write_fmt(&mut self, fmt: fmt::Arguments<'_>) -> Result<(), InternalError> {
        // Create a shim which translates a Write to a fmt::Write and saves
        // off I/O errors. instead of discarding them
        struct Adapter<'a, T: ?Sized + 'a> {
            inner: &'a mut T,
            error: Result<(), InternalError>,
        }

        impl<T: Write + ?Sized> fmt::Write for Adapter<'_, T> {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                match self.inner.write_all(s.as_bytes()) {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        self.error = Err(e);
                        Err(fmt::Error)
                    }
                }
            }
        }

        let mut output = Adapter { inner: self, error: Ok(()) };
        match fmt::write(&mut output, fmt) {
            Ok(()) => Ok(()),
            Err(..) => {
                // check if the error came from the underlying `Write` or not
                if output.error.is_err() {
                    output.error
                } else {
                    // This shouldn't happen: the underlying stream did not error, but somehow
                    // the formatter still errored?
                    panic!(
                        "a formatting trait implementation returned an error when the underlying stream did not"
                    );
                }
            }
        }
    }
}

impl Write for Vec<u8> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, InternalError> {
        for b in buf {
            self.push(*b);
        };
        Ok(buf.len())
    }
}
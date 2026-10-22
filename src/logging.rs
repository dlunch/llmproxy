use std::{
    io::{self, BufWriter, Write},
    sync::mpsc,
};

use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};

pub struct LogDrain {
    writer: NonBlocking,
    flushed: mpsc::Receiver<io::Result<()>>,
    _guard: WorkerGuard,
}

impl LogDrain {
    pub fn finish(mut self) -> io::Result<()> {
        // JSON escapes NUL; the queued marker is an ordered flush barrier, never a log record.
        self.writer.write_all(&[0])?;
        self.flushed.recv().map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Logging worker stopped before flushing",
            )
        })?
    }
}

pub fn writer(output: impl Write + Send + 'static) -> (NonBlocking, LogDrain) {
    let (flushed, receiver) = mpsc::channel();
    let (writer, guard) = NonBlockingBuilder::default()
        .buffered_lines_limit(256)
        .lossy(false)
        .finish(DrainWriter {
            output: BufWriter::new(output),
            flushed,
            error: None,
        });
    let drain = LogDrain {
        writer: writer.clone(),
        flushed: receiver,
        _guard: guard,
    };
    (writer, drain)
}

struct DrainWriter<W> {
    output: W,
    flushed: mpsc::Sender<io::Result<()>>,
    error: Option<io::Error>,
}

impl<W: Write> Write for DrainWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes == [0] {
            let _ = self.flush();
            let result = self.error.take().map_or(Ok(()), Err);
            let _ = self.flushed.send(result);
            return Ok(1);
        }
        let result = self.output.write(bytes).and_then(|written| {
            if written == 0 && !bytes.is_empty() {
                Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "Log output made no progress",
                ))
            } else {
                Ok(written)
            }
        });
        if let Err(error) = &result
            && error.kind() != io::ErrorKind::Interrupted
            && self.error.is_none()
        {
            self.error = Some(io::Error::new(error.kind(), error.to_string()));
        }
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        let result = self.output.flush();
        if let Err(error) = &result
            && self.error.is_none()
        {
            self.error = Some(io::Error::new(error.kind(), error.to_string()));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };

    struct GatedOutput {
        bytes: Arc<Mutex<Vec<u8>>>,
        entered: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
        caller: thread::ThreadId,
    }

    impl Write for GatedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            assert_ne!(thread::current().id(), self.caller);
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.release.recv().unwrap();
            }
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn worker_preserves_records_under_backpressure_and_drains_slow_output() {
        for saturate in [true, false] {
            let bytes = Arc::new(Mutex::new(Vec::new()));
            let (entered, ready) = mpsc::channel();
            let (release, blocked) = mpsc::channel();
            let (mut writer, drain) = writer(GatedOutput {
                bytes: bytes.clone(),
                entered: Some(entered),
                release: blocked,
                caller: thread::current().id(),
            });
            let record = serde_json::to_string(&serde_json::json!({"body":"literal\u{0}content"}))
                .unwrap()
                + "\n";
            writer.write_all(record.as_bytes()).unwrap();
            ready.recv().unwrap();
            if saturate {
                for _ in 0..256 {
                    writer.write_all(record.as_bytes()).unwrap();
                }
                let (sent, sent_receiver) = mpsc::channel();
                let copy = record.clone();
                let producer = thread::spawn(move || {
                    writer.write_all(copy.as_bytes()).unwrap();
                    sent.send(()).unwrap();
                });
                assert!(
                    sent_receiver
                        .recv_timeout(Duration::from_millis(50))
                        .is_err()
                );
                release.send(()).unwrap();
                producer.join().unwrap();
                drain.finish().unwrap();
                assert_eq!(*bytes.lock().unwrap(), record.repeat(258).as_bytes());
            } else {
                let (finished, completion) = mpsc::channel();
                let draining = thread::spawn(move || finished.send(drain.finish()).unwrap());
                assert!(
                    completion
                        .recv_timeout(Duration::from_millis(1200))
                        .is_err()
                );
                release.send(()).unwrap();
                completion
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                draining.join().unwrap();
                assert_eq!(*bytes.lock().unwrap(), record.as_bytes());
            }
        }
    }

    #[test]
    fn drain_reports_write_zero_and_flush_errors() {
        struct BrokenOutput(u8);
        impl Write for BrokenOutput {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                match self.0 {
                    0 => Err(io::Error::other("write failed")),
                    1 => Ok(0),
                    _ => Ok(bytes.len()),
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                if self.0 == 2 {
                    Err(io::Error::other("flush failed"))
                } else {
                    Ok(())
                }
            }
        }
        for mode in 0..3 {
            let (mut writer, drain) = writer(BrokenOutput(mode));
            writer.write_all(b"record\n").unwrap();
            assert!(drain.finish().is_err());
        }
        struct InterruptedOnce(bool);
        impl Write for InterruptedOnce {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if std::mem::replace(&mut self.0, false) {
                    Err(io::Error::from(io::ErrorKind::Interrupted))
                } else {
                    Ok(bytes.len())
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (mut writer, drain) = writer(InterruptedOnce(true));
        writer.write_all(b"retried record\n").unwrap();
        drain.finish().unwrap();
    }
}

//! One bounded, nonblocking input queue per PTY. Only its actor touches the writer.
use crate::{Error, Result, agent::SUBMIT_PAUSE};
use parking_lot::Mutex;
use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, SyncSender, TrySendError},
    },
};

const CAPACITY: usize = 64;
enum Command {
    Input(Vec<u8>),
    Prompt(Vec<u8>),
}

pub(crate) struct Writer {
    sender: SyncSender<Command>,
    fault: Arc<Mutex<Option<Error>>>,
}
impl Writer {
    pub(crate) fn start(mut output: Box<dyn Write + Send>, cwd: PathBuf) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(CAPACITY);
        let fault = Arc::new(Mutex::new(None));
        let failed = Arc::clone(&fault);
        let path = cwd.clone();
        std::thread::Builder::new()
            .name("qream-pane-writer".into())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    let result = (|| {
                        let (Command::Input(bytes) | Command::Prompt(bytes)) = &command;
                        output.write_all(bytes)?;
                        output.flush()?;
                        if matches!(command, Command::Prompt(_)) {
                            std::thread::sleep(SUBMIT_PAUSE);
                            output.write_all(b"\r")?;
                            output.flush()?;
                        }
                        Ok::<_, std::io::Error>(())
                    })();
                    if let Err(e) = result {
                        *failed.lock() = Some(Error::io("write PTY", &path, e));
                        break;
                    }
                }
            })
            .map_err(|e| Error::io("start PTY writer", cwd, e))?;
        Ok(Self { sender, fault })
    }
    fn enqueue(&self, command: Command) -> Result<()> {
        match self.sender.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(Error::Busy),
            Err(TrySendError::Disconnected(_)) => Err(Error::Lost("PTY writer stopped".into())),
        }
    }
    pub(crate) fn write(&self, bytes: &[u8]) -> Result<()> {
        self.enqueue(Command::Input(bytes.into()))
    }
    pub(crate) fn prompt(&self, text: &str) -> Result<()> {
        self.enqueue(Command::Prompt(text.as_bytes().into()))
    }
    pub(crate) fn take_error(&self) -> Option<Error> {
        self.fault.lock().take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::time::{Duration, Instant};

    struct Blocked {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }
    impl Write for Blocked {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.entered.send(()).expect("test receiver alive");
            self.release
                .recv()
                .map_err(|_| io::Error::other("test release closed"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn full_queue_returns_busy_without_waiting() {
        let (entered, started) = mpsc::channel();
        let (release, unblock) = mpsc::channel();
        let writer = Writer::start(
            Box::new(Blocked {
                entered,
                release: unblock,
            }),
            PathBuf::from("test"),
        )
        .unwrap();
        writer.write(b"blocked").unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..CAPACITY {
            writer.prompt("queued").unwrap();
        }
        let start = Instant::now();
        assert!(matches!(writer.prompt("overflow"), Err(Error::Busy)));
        assert!(start.elapsed() < Duration::from_millis(250));
        drop(release);
    }
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("injected write failure"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn write_fault_is_retained_for_observation() {
        let writer = Writer::start(Box::new(Broken), PathBuf::from("test")).unwrap();
        writer.prompt("failed").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(error) = writer.take_error() {
                assert!(error.to_string().contains("injected write failure"));
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    type Captured = Arc<Mutex<Vec<(Vec<u8>, Instant)>>>;
    struct Recorded(Captured);
    impl Write for Recorded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().push((bytes.into(), Instant::now()));
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn prompt_pause_and_submit_are_atomic_with_following_input() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let writer = Writer::start(
            Box::new(Recorded(Arc::clone(&captured))),
            PathBuf::from("test"),
        )
        .unwrap();
        let start = Instant::now();
        writer.prompt("text").unwrap();
        writer.write(b"key").unwrap();
        assert!(start.elapsed() < Duration::from_millis(250));
        let deadline = Instant::now() + Duration::from_secs(2);
        while captured.lock().len() < 3 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let captured = captured.lock();
        assert_eq!(
            captured
                .iter()
                .map(|(bytes, _)| bytes.as_slice())
                .collect::<Vec<_>>(),
            vec![b"text".as_slice(), b"\r", b"key"]
        );
        assert!(captured[1].1.duration_since(captured[0].1) >= SUBMIT_PAUSE);
    }
}

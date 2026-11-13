//! Background capture thread. Keeps only the latest frame pair so analysis
//! never lags behind the camera, and doubles throughput compared with
//! capturing and analysing in turn.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};

use crate::camera::{Camera, CameraInfo, FramePair};

type Slot = Arc<(Mutex<Option<Result<FramePair, String>>>, Condvar)>;

pub struct CameraFeed {
    slot: Slot,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl CameraFeed {
    pub fn start(info: &CameraInfo, fps: u32) -> Result<Self> {
        let slot: Slot = Arc::new((Mutex::new(None), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
        let info = info.clone();
        let (slot2, stop2) = (slot.clone(), stop.clone());
        let handle = std::thread::Builder::new().name("ir-capture".into()).spawn(move || {
            // The camera is opened on this thread: mmap'ed buffers stay here.
            let mut cam = match Camera::open(&info, fps) {
                Ok(c) => {
                    let _ = ready_tx.send(Ok(()));
                    c
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("{e:#}")));
                    return;
                }
            };
            let (lock, cv) = &*slot2;
            while !stop2.load(Ordering::Relaxed) {
                let res = cam.next_pair(Duration::from_secs(2)).map_err(|e| format!("{e:#}"));
                let failed = res.is_err();
                *lock.lock().unwrap() = Some(res);
                cv.notify_all();
                if failed {
                    break;
                }
            }
        })?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { slot, stop, handle: Some(handle) }),
            Ok(Err(e)) => bail!(e),
            Err(_) => bail!("capture thread died"),
        }
    }

    /// Waits for a frame pair newer than the last one returned.
    pub fn next(&self, timeout: Duration) -> Result<FramePair> {
        let deadline = Instant::now() + timeout;
        let (lock, cv) = &*self.slot;
        let mut guard = lock.lock().unwrap();
        loop {
            if let Some(res) = guard.take() {
                return res.map_err(|e| anyhow!(e));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("timed out waiting for IR frames");
            }
            guard = cv.wait_timeout(guard, left).unwrap().0;
        }
    }
}

impl Drop for CameraFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

//! Runs the audio helper in tools/audio_tap and keeps the most recent samples.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, sleep};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const HELPER_APP: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tools/audio_tap/FoxkitAudioTap.app"
);
const CHANNELS: usize = 2;
const SAMPLE_BYTES: usize = 4;
const PERMISSION_WAIT: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(50);

#[derive(Default)]
struct Shared {
    rate: u32,
    device: Option<String>,
    pid: Option<u32>,
    errors: Vec<String>,
    samples: Vec<f32>,
}

pub struct AudioSource {
    shared: Arc<Mutex<Shared>>,
    folder: PathBuf,
}

impl AudioSource {
    /// Starts the helper. Launched with open so it's its own app to macOS, which means no
    /// direct pipes, so it writes to named ones. It's given this app's pid, and ends if this
    /// app dies.
    pub fn start(window: usize) -> Result<Self, String> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_nanos());
        let folder =
            std::env::temp_dir().join(format!("foxkit-audio-{}-{stamp}", std::process::id()));
        fs::create_dir(&folder)
            .map_err(|error| format!("Couldn't prepare audio capture: {error}"))?;
        let samples_path = folder.join("samples");
        let status_path = folder.join("status");
        let source = Self {
            shared: Arc::new(Mutex::new(Shared {
                samples: vec![0.0; window],
                ..Shared::default()
            })),
            folder,
        };
        for path in [&samples_path, &status_path] {
            let made = Command::new("/usr/bin/mkfifo").arg(path).status();
            if !made.is_ok_and(|status| status.success()) {
                return Err("Couldn't prepare audio capture.".to_string());
            }
        }
        spawn_status_reader(status_path.clone(), Arc::clone(&source.shared));
        spawn_sample_reader(samples_path.clone(), Arc::clone(&source.shared));
        let opened = Command::new("/usr/bin/open")
            .args(["-n", "-g", "--stdout"])
            .arg(&samples_path)
            .arg("--stderr")
            .arg(&status_path)
            .arg(HELPER_APP)
            .arg("--args")
            .arg(std::process::id().to_string())
            .status();
        if !opened.is_ok_and(|status| status.success()) {
            // Both readers wait for a writer, so opening the pipes here lets them finish.
            let _ = File::create(&samples_path);
            let _ = File::create(&status_path);
            return Err(
                "Couldn't start the audio helper. Build it with tools/audio_tap/build.sh."
                    .to_string(),
            );
        }
        Ok(source)
    }

    /// Waits for the helper to report in, which it only does once the macOS permission
    /// prompt is answered.
    pub fn wait_until_ready(&self, stop: &AtomicBool) -> Result<(), String> {
        let started = Instant::now();
        while started.elapsed() < PERMISSION_WAIT {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let shared = self.lock();
            if let Some(error) = shared.errors.first() {
                return Err(format!("Audio capture didn't start: {error}"));
            }
            if shared.rate != 0 {
                return Ok(());
            }
            drop(shared);
            sleep(POLL);
        }
        Err(
            "Audio capture didn't start. Check System Audio Recording in System Settings."
                .to_string(),
        )
    }

    pub fn rate(&self) -> u32 {
        self.lock().rate
    }

    pub fn device(&self) -> Option<String> {
        self.lock().device.clone()
    }

    pub fn latest(&self) -> Vec<f32> {
        self.lock().samples.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for AudioSource {
    fn drop(&mut self) {
        if let Some(pid) = self.lock().pid {
            // If it already stopped, this fails harmlessly.
            let _ = Command::new("/bin/kill")
                .args(["-TERM", &pid.to_string()])
                .stderr(Stdio::null())
                .status();
        }
        let _ = fs::remove_dir_all(&self.folder);
    }
}

fn spawn_status_reader(path: PathBuf, shared: Arc<Mutex<Shared>>) {
    thread::spawn(move || {
        let Ok(file) = File::open(&path) else { return };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let mut shared = shared.lock().unwrap_or_else(PoisonError::into_inner);
            apply_status_line(&mut shared, line.trim());
        }
    });
}

fn apply_status_line(shared: &mut Shared, line: &str) {
    if let Some((device, rate)) = line
        .strip_prefix("device ")
        .and_then(|rest| rest.rsplit_once(" rate "))
    {
        if let Ok(rate) = rate.parse() {
            shared.rate = rate;
            shared.device = Some(device.to_string());
            return;
        }
    }
    if let Some(pid) = line.strip_prefix("pid ").and_then(|pid| pid.parse().ok()) {
        shared.pid = Some(pid);
        return;
    }
    shared.errors.push(line.to_string());
}

fn spawn_sample_reader(path: PathBuf, shared: Arc<Mutex<Shared>>) {
    thread::spawn(move || read_samples(&path, &shared));
}

fn read_samples(path: &Path, shared: &Mutex<Shared>) {
    let Ok(mut file) = File::open(path) else {
        return;
    };
    let frame_bytes = CHANNELS * SAMPLE_BYTES;
    let mut buffer = vec![0; 65536];
    let mut held = 0;
    loop {
        let size = match file.read(&mut buffer[held..]) {
            Ok(0) | Err(_) => return,
            Ok(size) => size,
        };
        let usable = (held + size) / frame_bytes * frame_bytes;
        let mono = mono_samples(&buffer[..usable]);
        // A read can end part-way through a sample. Keep the rest for the next one.
        buffer.copy_within(usable..held + size, 0);
        held = held + size - usable;
        let mut shared = shared.lock().unwrap_or_else(PoisonError::into_inner);
        let window = shared.samples.len();
        let recent = &mono[mono.len().saturating_sub(window)..];
        shared.samples.copy_within(recent.len().., 0);
        shared.samples[window - recent.len()..].copy_from_slice(recent);
    }
}

fn mono_samples(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(CHANNELS * SAMPLE_BYTES)
        .map(|frame| {
            let sum: f32 = frame
                .chunks_exact(SAMPLE_BYTES)
                .map(|sample| f32::from_ne_bytes(sample.try_into().expect("chunk is one sample")))
                .sum();
            sum / CHANNELS as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averages_channels() {
        let bytes: Vec<u8> = [0.5_f32, 0.25, -1.0, 1.0]
            .iter()
            .flat_map(|sample| sample.to_ne_bytes())
            .collect();
        assert_eq!(mono_samples(&bytes), [0.375, 0.0]);
    }

    #[test]
    fn reads_helper_status_lines() {
        let mut shared = Shared::default();
        apply_status_line(&mut shared, "pid 42");
        apply_status_line(&mut shared, "device Stone 358 Pro rate 44100");
        assert_eq!(shared.pid, Some(42));
        assert_eq!(shared.rate, 44100);
        assert_eq!(shared.device.as_deref(), Some("Stone 358 Pro"));
        assert!(shared.errors.is_empty());
        apply_status_line(
            &mut shared,
            "error System Audio Recording permission not granted",
        );
        assert_eq!(shared.errors.len(), 1);
    }
}

// mic.rs — Record from microphone and transcribe with bundled moonshine-voice sidecar
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{WavSpec, WavWriter};
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;

/// Record from the default microphone for up to `max_secs` seconds,
/// stopping early when `max_secs` is reached or the caller drops the handle.
/// Returns raw WAV bytes at 16 kHz mono 16-bit.
fn record_mic_wav(max_secs: u64) -> Result<Vec<u8>, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("No input device found")?;

    let config = device
        .default_input_config()
        .map_err(|e| format!("Failed to get input config: {e}"))?;

    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let samples_clone = samples.clone();
    let stop_flag = Arc::new(Mutex::new(false));
    let stop_clone = stop_flag.clone();

    let stream = device
        .build_input_stream(
            &config.into(),
            move |data: &[f32], _| {
                if *stop_clone.lock().unwrap() {
                    return;
                }
                // Mix down to mono
                let mut mono = Vec::with_capacity(data.len() / channels);
                for chunk in data.chunks(channels) {
                    let sum: f32 = chunk.iter().sum();
                    mono.push(sum / channels as f32);
                }
                samples_clone.lock().unwrap().extend_from_slice(&mono);
            },
            |e| eprintln!("[mic] stream error: {e}"),
            None,
        )
        .map_err(|e| format!("Failed to build input stream: {e}"))?;

    stream.play().map_err(|e| format!("Failed to play stream: {e}"))?;

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(max_secs) {
        std::thread::sleep(Duration::from_millis(50));
    }
    *stop_flag.lock().unwrap() = true;
    drop(stream);

    let raw_samples = samples.lock().unwrap().clone();
    if raw_samples.is_empty() {
        return Err("No audio recorded".into());
    }

    // Resample to 16 kHz if needed (simple linear interpolation)
    let resampled = if sample_rate != 16000 {
        resample(&raw_samples, sample_rate, 16000)
    } else {
        raw_samples
    };

    // Encode to WAV
    let mut cursor = Cursor::new(Vec::new());
    let spec = WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = WavWriter::new(&mut cursor, spec)
        .map_err(|e| format!("WAV writer error: {e}"))?;
    for s in &resampled {
        let clamped = s.clamp(-1.0, 1.0);
        writer
            .write_sample((clamped * i16::MAX as f32) as i16)
            .map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;
    Ok(cursor.into_inner())
}

fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate {
        return samples.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = (samples.len() as f64 / ratio).ceil() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 * ratio;
        let idx = src as usize;
        let frac = src - idx as f64;
        let a = samples.get(idx).copied().unwrap_or(0.0);
        let b = samples.get(idx + 1).copied().unwrap_or(0.0);
        out.push(a + (b - a) * frac as f32);
    }
    out
}

/// Tauri command: record mic for `duration_secs`, transcribe, return text.
#[tauri::command]
pub async fn mic_record_and_transcribe(
    app: AppHandle,
    duration_secs: Option<u64>,
) -> Result<String, String> {
    let secs = duration_secs.unwrap_or(10).min(60);

    // Record in a blocking thread so we don't block the async runtime
    let wav_bytes =
        tokio::task::spawn_blocking(move || record_mic_wav(secs))
            .await
            .map_err(|e| format!("Task panicked: {e}"))??;

    // Write WAV to a temp file the sidecar can read
    let tmp_path = std::env::temp_dir().join(format!("f1ck_mic_{}.wav", uuid::Uuid::new_v4()));
    std::fs::write(&tmp_path, &wav_bytes)
        .map_err(|e| format!("Failed to write temp WAV: {e}"))?;

    let tmp_str = tmp_path
        .to_str()
        .ok_or("Invalid temp path")?
        .to_string();

    // Run moonshine-voice sidecar
    let result = app
        .shell()
        .sidecar("moonshine-voice")
        .map_err(|e| format!("Sidecar not found: {e}"))?
        .args(["transcribe", "--language", "en", "--wav-path", &tmp_str, "--quiet"])
        .output()
        .await
        .map_err(|e| format!("Sidecar execution failed: {e}"))?;

    // Clean up temp file
    let _ = std::fs::remove_file(&tmp_path);

    if !result.status.success() {
        let err = String::from_utf8_lossy(&result.stderr).to_string();
        return Err(format!("Transcription failed: {err}"));
    }

    // moonshine-voice --quiet writes transcript to stderr
    let transcript = String::from_utf8_lossy(&result.stderr).trim().to_string();
    let transcript = if transcript.is_empty() {
        String::from_utf8_lossy(&result.stdout).trim().to_string()
    } else {
        transcript
    };

    Ok(transcript)
}

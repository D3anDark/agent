use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

const MODEL: &str = ".agent/models/ggml-large-v3-turbo-q5_0.bin";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin";

pub struct Recording {
    ffmpeg: Child,
    wav: PathBuf,
    model: PathBuf,
    pub started: Instant,
}

pub fn start(home: &Path) -> Result<Recording, String> {
    let model = home.join(MODEL);
    if !model.exists() {
        return Err(format!("No whisper model at {}. Download it: curl -L --create-dirs -o {} {MODEL_URL}", model.display(), model.display()));
    }
    let wav = std::env::temp_dir().join(format!("agent-voice-{}.wav", std::process::id()));
    let ffmpeg = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y"])
        .args(if cfg!(target_os = "macos") { ["-f", "avfoundation", "-i", ":default"] } else { ["-f", "pulse", "-i", "default"] })
        .args(["-ar", "16000", "-ac", "1"])
        .arg(&wav)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Cannot run ffmpeg: {e}"))?;
    Ok(Recording { ffmpeg, wav, model, started: Instant::now() })
}

impl Recording {
    pub fn cancel(mut self) {
        let _ = self.ffmpeg.kill();
        let _ = self.ffmpeg.wait();
        let _ = std::fs::remove_file(&self.wav);
    }

    /// Blocks until whisper-cli finishes, so call it off the UI thread.
    pub fn transcribe(mut self) -> Result<String, String> {
        // ffmpeg writes the WAV header sizes only when it quits on "q", not on kill.
        let quit = self.ffmpeg.stdin.take().expect("stdin is piped").write_all(b"q");
        let status = self.ffmpeg.wait().map_err(|e| format!("ffmpeg: {e}"))?;
        if quit.is_err() || !status.success() {
            let _ = std::fs::remove_file(&self.wav);
            return Err("Recording failed. Check that the terminal may use the microphone.".into());
        }
        let output = Command::new("whisper-cli")
            .args(["-l", "auto", "-nt", "-np", "-m"])
            .arg(&self.model)
            .arg("-f")
            .arg(&self.wav)
            .output();
        let _ = std::fs::remove_file(&self.wav);
        let output = output.map_err(|e| format!("Cannot run whisper-cli: {e}. Install it: brew install whisper-cpp"))?;
        if !output.status.success() {
            return Err(format!("whisper-cli failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
        }
        spoken_text(&String::from_utf8_lossy(&output.stdout))
    }
}

fn spoken_text(stdout: &str) -> Result<String, String> {
    let text = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() || text == "[BLANK_AUDIO]" {
        return Err("Heard nothing.".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_segments_and_rejects_silence() {
        assert_eq!(spoken_text(" Cześć,\n sprawdź testy.\n").unwrap(), "Cześć, sprawdź testy.");
        assert!(spoken_text("\n [BLANK_AUDIO]\n").is_err());
        assert!(spoken_text("").is_err());
    }
}

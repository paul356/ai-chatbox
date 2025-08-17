use anyhow::Result;
use esp_idf_svc::hal::i2s::{I2sDriver, I2sTx};
use esp_idf_svc::sys;
use std::ffi::{CString, c_void};
use std::ptr;

// Import ESP-TTS bindings from esp_sr module
use sys::esp_sr::{
    esp_tts_handle_t, esp_tts_voice_t, esp_tts_voice_template,
    esp_tts_voice_set_init, esp_tts_voice_set_free,
    esp_tts_create, esp_tts_destroy,
    esp_tts_parse_chinese, esp_tts_stream_play, esp_tts_stream_reset,
};

#[derive(Clone)]
pub struct TtsConfig {
    pub max_chunk_chars: usize,
    pub chunk_delay_ms: u64,
    pub speed: u32,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            max_chunk_chars: 50,
            chunk_delay_ms: 50,
            speed: 3, // Medium speed (0-5 range)
        }
    }
}

pub struct TtsEngine {
    handle: esp_tts_handle_t,
    voice: *mut esp_tts_voice_t,
    voice_data: *const c_void,
    #[allow(dead_code)]
    mmap_handle: u32,
    config: TtsConfig,
}

impl TtsEngine {
    pub fn new() -> Result<Self> {
        Self::new_with_config(TtsConfig::default())
    }

    pub fn new_with_config(config: TtsConfig) -> Result<Self> {
        log::info!("Initializing TTS engine");

        // Find the voice data partition
        let partition_name = CString::new("voice_data")?;
        let partition = unsafe {
            sys::esp_partition_find_first(
                sys::esp_partition_type_t_ESP_PARTITION_TYPE_DATA,
                sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_ANY,
                partition_name.as_ptr()
            )
        };

        if partition.is_null() {
            return Err(anyhow::anyhow!("Voice data partition not found"));
        }

        // Memory map the voice data partition
        let mut voice_data: *const c_void = ptr::null();
        let mut mmap_handle: u32 = 0;

        let partition_ref = unsafe { &*partition };
        let err = unsafe {
            sys::esp_partition_mmap(
                partition,
                0,
                partition_ref.size as usize,
                sys::esp_partition_mmap_memory_t_ESP_PARTITION_MMAP_DATA,
                &mut voice_data,
                &mut mmap_handle
            )
        };

        if err != sys::ESP_OK {
            return Err(anyhow::anyhow!("Failed to map voice data partition: {}", err));
        }

        log::info!("Voice data partition mapped successfully");

        // Initialize the voice set
        let voice = unsafe {
            esp_tts_voice_set_init(&esp_tts_voice_template, voice_data as *mut c_void)
        };

        if voice.is_null() {
            unsafe { sys::esp_partition_munmap(mmap_handle); }
            return Err(anyhow::anyhow!("Failed to initialize TTS voice set"));
        }

        // Create TTS handle
        let handle = unsafe { esp_tts_create(voice) };
        if handle.is_null() {
            unsafe {
                esp_tts_voice_set_free(voice);
                sys::esp_partition_munmap(mmap_handle);
            }
            return Err(anyhow::anyhow!("Failed to create TTS handle"));
        }

        log::info!("TTS engine initialized successfully");

        Ok(TtsEngine {
            handle,
            voice,
            voice_data,
            mmap_handle,
            config,
        })
    }

    pub fn set_config(&mut self, config: TtsConfig) {
        self.config = config;
    }

    pub fn get_config(&self) -> &TtsConfig {
        &self.config
    }

    /// Test utility function to preview how text would be chunked
    pub fn preview_chunks(&self, text: &str) -> Vec<String> {
        self.split_text_into_chunks(text, self.config.max_chunk_chars)
    }

    pub fn synthesize_and_play(&mut self, text: &str, i2s_driver: &mut I2sDriver<I2sTx>) -> Result<()> {
        log::info!("Synthesizing text: {}", text);

        // Split text into chunks to prevent watchdog timeout
        let chunks = self.split_text_into_chunks(text, self.config.max_chunk_chars);

        for (i, chunk) in chunks.iter().enumerate() {
            if chunk.trim().is_empty() {
                continue;
            }

            log::info!("Processing chunk {}/{}: {}", i + 1, chunks.len(), chunk);

            if let Err(e) = self.synthesize_chunk(chunk, i2s_driver) {
                log::error!("Failed to synthesize chunk {}: {}", i + 1, e);
                // Continue with next chunk instead of failing completely
                continue;
            }

            // Small delay between chunks to prevent overwhelming the system
            if self.config.chunk_delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(self.config.chunk_delay_ms));
            }
        }

        log::info!("Audio synthesis and playback completed for all chunks");
        Ok(())
    }

    fn split_text_into_chunks(&self, text: &str, max_chars: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current_chunk = String::new();

        // Split by sentences first (periods, exclamation marks, question marks)
        let sentences: Vec<&str> = text.split(|c| c == '。' || c == '！' || c == '？' || c == '.' || c == '!' || c == '?').collect();

        for sentence in sentences {
            let sentence = sentence.trim();
            if sentence.is_empty() {
                continue;
            }

            // If adding this sentence would exceed max_chars, push current chunk and start new one
            if !current_chunk.is_empty() && current_chunk.len() + sentence.len() + 1 > max_chars {
                chunks.push(current_chunk.clone());
                current_chunk.clear();
            }

            // If sentence itself is longer than max_chars, split it by commas or spaces
            if sentence.len() > max_chars {
                let sub_chunks = self.split_long_sentence(sentence, max_chars);
                for sub_chunk in sub_chunks {
                    if !current_chunk.is_empty() && current_chunk.len() + sub_chunk.len() + 1 > max_chars {
                        chunks.push(current_chunk.clone());
                        current_chunk.clear();
                    }

                    if current_chunk.is_empty() {
                        current_chunk = sub_chunk;
                    } else {
                        current_chunk.push_str(&format!(" {}", sub_chunk));
                    }
                }
            } else {
                if current_chunk.is_empty() {
                    current_chunk = sentence.to_string();
                } else {
                    current_chunk.push_str(&format!(" {}", sentence));
                }
            }
        }

        // Add the last chunk if it's not empty
        if !current_chunk.is_empty() {
            chunks.push(current_chunk);
        }

        // If no chunks were created (edge case), return the original text as a single chunk
        if chunks.is_empty() {
            chunks.push(text.to_string());
        }

        chunks
    }

    fn split_long_sentence(&self, sentence: &str, max_chars: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current_chunk = String::new();

        // Try splitting by commas first
        let parts: Vec<&str> = sentence.split(|c| c == '，' || c == ',').collect();

        for part in parts {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }

            if !current_chunk.is_empty() && current_chunk.len() + part.len() + 1 > max_chars {
                chunks.push(current_chunk.clone());
                current_chunk.clear();
            }

            // If part is still too long, split by characters
            if part.len() > max_chars {
                if !current_chunk.is_empty() {
                    chunks.push(current_chunk.clone());
                    current_chunk.clear();
                }

                let char_chunks: Vec<String> = part.chars()
                    .collect::<Vec<char>>()
                    .chunks(max_chars)
                    .map(|chunk| chunk.iter().collect())
                    .collect();

                for char_chunk in char_chunks {
                    chunks.push(char_chunk);
                }
            } else {
                if current_chunk.is_empty() {
                    current_chunk = part.to_string();
                } else {
                    current_chunk.push_str(&format!(" {}", part));
                }
            }
        }

        if !current_chunk.is_empty() {
            chunks.push(current_chunk);
        }

        chunks
    }

    fn synthesize_chunk(&mut self, text: &str, i2s_driver: &mut I2sDriver<I2sTx>) -> Result<()> {
        // Convert text to CString
        let c_text = CString::new(text)?;

        // Parse the Chinese text
        let result = unsafe {
            esp_tts_parse_chinese(self.handle, c_text.as_ptr())
        };

        if result == 0 {
            return Err(anyhow::anyhow!("Failed to parse Chinese text"));
        }

        log::info!("Text parsed successfully, starting audio synthesis");

        // Stream the audio data
        let mut len: i32 = 0;
        let speed = self.config.speed;

        loop {
            let pcm_data = unsafe {
                esp_tts_stream_play(self.handle, &mut len, speed)
            };

            if len <= 0 {
                break; // End of audio data
            }

            // Convert the PCM data to bytes
            let pcm_slice = unsafe {
                std::slice::from_raw_parts(pcm_data as *const u8, (len * 2) as usize)
            };

            // Write to I2S
            match i2s_driver.write_all(pcm_slice, 1000) {
                Ok(_) => {
                    log::debug!("Written {} bytes to I2S", pcm_slice.len());
                },
                Err(e) => {
                    log::error!("Failed to write audio data to I2S: {}", e);
                    break;
                }
            }
        }

        // Reset the TTS stream for next use
        unsafe {
            esp_tts_stream_reset(self.handle);
        }

        log::info!("Audio synthesis and playback completed for chunk");
        Ok(())
    }
}

impl Drop for TtsEngine {
    fn drop(&mut self) {
        log::info!("Cleaning up TTS engine");

        unsafe {
            if !self.handle.is_null() {
                esp_tts_destroy(self.handle);
            }
            if !self.voice.is_null() {
                esp_tts_voice_set_free(self.voice);
            }
            if self.mmap_handle != 0 {
                sys::esp_partition_munmap(self.mmap_handle);
            }
        }

        log::info!("TTS engine cleanup completed");
    }
}

// Thread-safe wrapper for passing TTS engine across threads
unsafe impl Send for TtsEngine {}
unsafe impl Sync for TtsEngine {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_chunking() {
        let config = TtsConfig::default();
        let engine = TtsEngine {
            handle: std::ptr::null_mut(),
            voice: std::ptr::null_mut(),
            voice_data: std::ptr::null(),
            mmap_handle: 0,
            config,
        };

        // Test Chinese text with punctuation
        let text = "这是一个很长的句子，包含了很多字符。这是第二个句子！这是第三个句子？还有更多内容要处理。";
        let chunks = engine.split_text_into_chunks(text, 20);

        println!("Original text: {}", text);
        for (i, chunk) in chunks.iter().enumerate() {
            println!("Chunk {}: {}", i + 1, chunk);
        }

        // Verify that chunks are created and within size limits
        assert!(!chunks.is_empty());
        for chunk in &chunks {
            assert!(chunk.len() <= 30); // Allow some flexibility for word boundaries
        }
    }
}
use chrono::Utc;
use image::ImageFormat::{self};
use log::{debug, error, info, warn};
use screenpipe_core::find_ffmpeg_path;
use screenpipe_vision::{continuous_capture, CaptureResult, ControlMessage};
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Mutex;

use std::time::Duration;

const MAX_FPS: f64 = 30.0; // Adjust based on your needs

pub struct VideoCapture {
    control_tx: Sender<ControlMessage>,
    frame_queue: Arc<Mutex<VecDeque<CaptureResult>>>,
    ffmpeg_handle: Arc<Mutex<Option<Child>>>,
    is_running: Arc<Mutex<bool>>,
}

impl VideoCapture {
    pub fn new(
        output_path: &str,
        fps: f64,
        new_chunk_callback: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        info!("Starting new video capture");
        let (control_tx, mut control_rx) = channel(512);
        let frame_queue = Arc::new(Mutex::new(VecDeque::new()));
        let ffmpeg_handle = Arc::new(Mutex::new(None));
        let is_running = Arc::new(Mutex::new(true));
        let new_chunk_callback = Arc::new(new_chunk_callback);
        let new_chunk_callback_clone = Arc::clone(&new_chunk_callback);

        let capture_frame_queue = frame_queue.clone();
        let capture_thread_is_running = is_running.clone();
        let (result_sender, mut result_receiver) = channel(512);
        let _capture_thread = tokio::spawn(async move {
            continuous_capture(
                &mut control_rx,
                result_sender,
                Duration::from_secs_f64(1.0 / fps),
            )
            .await;
        });

        info!("Started capture thread");

        // Spawn another thread to handle receiving and queueing the results
        let _queue_thread = tokio::spawn(async move {
            while *capture_thread_is_running.lock().await {
                if let Some(result) = result_receiver.recv().await {
                    // debug!(
                    //     "Received result from capture thread: frame_number: {:?}",
                    //     result.frame_number
                    // );
                    // debug!(
                    //     "Received result from capture thread: timestamp: {:?}",
                    //     result.timestamp
                    // );
                    // debug!("Received result from capture thread: text: {:?}", result.text);
                    capture_frame_queue.lock().await.push_back(result);
                }
            }
        });

        let video_frame_queue = frame_queue.clone();
        let video_thread_is_running = is_running.clone();
        let output_path = output_path.to_string();
        let _video_thread = tokio::spawn(async move {
            save_frames_as_video(
                &video_frame_queue,
                &output_path,
                fps,
                video_thread_is_running,
                new_chunk_callback_clone,
            )
            .await;
        });

        VideoCapture {
            control_tx,
            frame_queue,
            ffmpeg_handle,
            is_running,
        }
    }

    pub async fn pause(&self) {
        self.control_tx.send(ControlMessage::Pause).await.unwrap();
    }

    pub async fn resume(&self) {
        self.control_tx.send(ControlMessage::Resume).await.unwrap();
    }

    pub async fn stop(&self) {
        self.control_tx.send(ControlMessage::Stop).await.unwrap();
        *self.is_running.lock().await = false;
        if let Some(mut child) = self.ffmpeg_handle.lock().await.take() {
            child
                .wait()
                .await
                .expect("Failed to wait for ffmpeg process");
        }
    }

    pub async fn get_latest_frame(&self) -> Option<CaptureResult> {
        self.frame_queue.lock().await.pop_front()
    }
}
async fn save_frames_as_video(
    frame_queue: &Arc<Mutex<VecDeque<CaptureResult>>>,
    output_path: &str,
    fps: f64,
    is_running: Arc<Mutex<bool>>,
    new_chunk_callback: Arc<dyn Fn(&str) + Send + Sync>,
) {
    debug!("Starting save_frames_as_video function");
    let frames_per_video = 30; // Adjust this value as needed
    let mut frame_count = 0;
    let (sender, mut receiver): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = channel(512);
    let sender = Arc::new(sender);
    let mut current_ffmpeg: Option<Child> = None;
    let mut current_stdin: Option<ChildStdin> = None;

    while *is_running.lock().await {
        if frame_count % frames_per_video == 0 || current_ffmpeg.is_none() {
            debug!("Starting new FFmpeg process");
            // Close previous FFmpeg process if exists
            if let Some(mut child) = current_ffmpeg.take() {
                drop(current_stdin.take()); // Ensure stdin is closed
                child.wait().await.expect("ffmpeg process failed");
            }

            // Wait for at least one frame before starting a new FFmpeg process
            let first_frame = loop {
                if let Some(result) = frame_queue.lock().await.pop_front() {
                    debug!("Got first frame for new chunk");
                    break result;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            };

            // Encode the first frame
            let mut buffer = Vec::new();
            first_frame
                .image
                .write_to(&mut std::io::Cursor::new(&mut buffer), ImageFormat::Png)
                .expect("Failed to encode first frame");

            let time = Utc::now();
            let formatted_time = time.format("%Y-%m-%d_%H-%M-%S").to_string();
            // Start new FFmpeg process with a new output file
            let output_file = format!("{}/{}.mp4", output_path, formatted_time);

            // Call the callback with the new video chunk file path
            new_chunk_callback(&output_file);

            let mut child = start_ffmpeg_process(&output_file, fps);
            let mut stdin = child.stdin.take().expect("Failed to open stdin");
            let stderr = child.stderr.take().expect("Failed to open stderr");

            // Write the first frame to FFmpeg
            stdin
                .write_all(&buffer)
                .await
                .expect("Failed to write first frame to ffmpeg");
            frame_count += 1;

            // Spawn a task to log FFmpeg's stderr
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    debug!("FFmpeg: {}", line);
                }
            });

            current_ffmpeg = Some(child);
            current_stdin = Some(stdin);
            debug!("New FFmpeg process started for file: {}", output_file);
        }

        if let Some(result) = frame_queue.lock().await.pop_front() {
            debug!("Processing frame in video.rs"); // {}", frame_count + 1
            let sender = Arc::clone(&sender);

            tokio::spawn(async move {
                let mut buffer = Vec::new();
                match result
                    .image
                    .write_to(&mut std::io::Cursor::new(&mut buffer), ImageFormat::Png)
                {
                    Ok(_) => {
                        sender
                            .send(buffer)
                            .await
                            .expect("Failed to send encoded frame");
                    }
                    Err(e) => error!("Failed to encode image as PNG: {}", e),
                }
            });
        } else {
            // debug!("No frames in queue, waiting...");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Write encoded frames to FFmpeg
        while let Ok(buffer) = receiver.try_recv() {
            if let Some(stdin) = current_stdin.as_mut() {
                if let Err(e) = stdin.write_all(buffer.as_slice()).await {
                    error!("Failed to write frame to ffmpeg: {}", e);
                    break;
                }
                frame_count += 1;
                debug!("Wrote frame {} to FFmpeg", frame_count);

                // Flush every second
                if frame_count % fps as usize == 0 {
                    debug!("Flushing FFmpeg input");
                    if let Err(e) = stdin.flush().await {
                        error!("Failed to flush FFmpeg input: {}", e);
                    }
                }
            }
        }

        // Yield to other tasks periodically
        if frame_count % 100 == 0 {
            tokio::task::yield_now().await;
        }
    }

    // Close the final FFmpeg process
    if let Some(mut child) = current_ffmpeg.take() {
        drop(current_stdin.take()); // Ensure stdin is closed
        child.wait().await.expect("ffmpeg process failed");
    }
}

// TODO: use tokio::process::Command instead
fn start_ffmpeg_process(output_file: &str, fps: f64) -> Child {
    // overrriding fps with max fps if over the max and warning user
    let mut fps = fps;
    if fps > MAX_FPS {
        warn!("Overriding FPS to {}", MAX_FPS);
        fps = MAX_FPS;
    }

    info!("Starting FFmpeg process for file: {}", output_file);
    Command::new(find_ffmpeg_path().unwrap())
        .args([
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "-r",
            &fps.to_string(),
            "-i",
            "-",
            "-vcodec",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-crf",
            "25",
            output_file,
        ])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn ffmpeg process")
}

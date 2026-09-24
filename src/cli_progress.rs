use std::{
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};

use tokio::{sync::mpsc, time::MissedTickBehavior};

use crate::processing::TranscriptionProgress;

/// Render a spinner in a terminal and durable progress lines in redirected output.
pub async fn show(mut receiver: mpsc::UnboundedReceiver<TranscriptionProgress>) {
    let interactive = io::stderr().is_terminal();
    let mut ticker = tokio::time::interval(Duration::from_millis(120));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut frame = 0;
    let mut total = None;
    let mut completed = 0;
    let mut started = Instant::now();

    loop {
        tokio::select! {
            event = receiver.recv() => match event {
                Some(TranscriptionProgress::Started { total_chunks }) => {
                    total = Some(total_chunks);
                    started = Instant::now();
                    eprintln!("Transcribing {total_chunks} audio chunks...");
                }
                Some(TranscriptionProgress::ChunkCompleted { completed: count, total_chunks }) => {
                    completed = count;
                    if interactive {
                        clear_line();
                    }
                    eprintln!("Completed chunk {completed}/{total_chunks} ({} elapsed)", elapsed(started));
                }
                None => break,
            },
            _ = ticker.tick(), if interactive && total.is_some_and(|count| completed < count) => {
                let total_chunks = total.expect("ticker requires a chunk count");
                let next = completed + 1;
                eprint!("\r\x1b[2K{} Transcribing chunk {next}/{total_chunks} ({} elapsed)", frames[frame], elapsed(started));
                let _ = io::stderr().flush();
                frame = (frame + 1) % frames.len();
            }
        }
    }
    if interactive {
        clear_line();
    }
}

fn clear_line() {
    eprint!("\r\x1b[2K");
    let _ = io::stderr().flush();
}

fn elapsed(started: Instant) -> String {
    let seconds = started.elapsed().as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

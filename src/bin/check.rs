#![allow(clippy::cast_precision_loss, clippy::cast_sign_loss, clippy::cast_possible_truncation)]
use std::{
    cmp::Ordering as SortOrder,
    collections::{HashMap, HashSet},
    fs,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering as LoadOrder},
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use rayon::prelude::*;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
struct Block {
    name: String,
    start: u32,
    end: u32,
}
#[derive(Debug)]
enum ProgressMsg {
    Started(String, usize, Instant),
    Progress(String, usize),
    Finished(String),
    Done,
}

#[derive(Debug)]
struct VersionInfo {
    codepoints: HashMap<u32, String>,
    latest: String,
}

fn version_compare(a: &str, b: &str) -> SortOrder {
    let a_parts = a.split('.').map(|s| s.parse().unwrap_or(0)).collect::<Vec<_>>();
    let b_parts = b.split('.').map(|s| s.parse().unwrap_or(0)).collect::<Vec<_>>();
    a_parts.cmp(&b_parts)
}
fn fetch_version_info() -> VersionInfo {
    let response = reqwest::blocking::get("https://www.unicode.org/Public/UNIDATA/DerivedAge.txt")
        .unwrap()
        .text()
        .unwrap();
    let mut codepoints = HashMap::new();
    let mut all_versions = HashSet::new();
    for line in response.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(';').collect();
        assert!(parts.len() >= 2);
        let range_part = parts[0].trim();
        let version = parts[1].split_whitespace().next().unwrap().to_string();
        all_versions.insert(version.clone());
        if let Some((start, end)) = range_part.split_once("..") {
            let start_cp = u32::from_str_radix(start.trim(), 16).unwrap();
            let end_cp = u32::from_str_radix(end.trim(), 16).unwrap();
            for cp in start_cp..=end_cp {
                codepoints.insert(cp, version.clone());
            }
        } else {
            let cp = u32::from_str_radix(range_part.trim(), 16).unwrap();
            codepoints.insert(cp, version.clone());
        }
    }
    let latest = all_versions.into_iter().max_by(|a, b| version_compare(a, b)).unwrap();
    VersionInfo { codepoints, latest }
}

fn fetch_blocks() -> Vec<Block> {
    let response = reqwest::blocking::get("https://www.unicode.org/Public/UNIDATA/Blocks.txt")
        .unwrap()
        .text()
        .unwrap();
    response
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let parts = line.split(';').collect::<Vec<_>>();
            let range = parts[0].trim().split("..").collect::<Vec<_>>();
            let start = u32::from_str_radix(range[0], 16).unwrap();
            let end = u32::from_str_radix(range[1], 16).unwrap();
            Block { name: parts[1].trim().to_string(), start, end }
        })
        .collect()
}

fn chunk_block(block: &Block) -> Vec<Block> {
    const CHUNK_SIZE: u32 = 2048;
    let len = block.end - block.start + 1;
    if len <= CHUNK_SIZE {
        return vec![block.clone()];
    }
    let chunks = len.div_ceil(CHUNK_SIZE);
    (0..chunks)
        .map(|i| {
            let chunk_start = block.start + i * CHUNK_SIZE;
            let chunk_end = std::cmp::min(chunk_start + CHUNK_SIZE - 1, block.end);
            Block {
                name: format!("{} (chunk {}/{chunks})", block.name, i + 1),
                start: chunk_start,
                end: chunk_end,
            }
        })
        .collect()
}

fn to_hex(num: u32) -> String { format!("{num:04x}") }

fn get_font_families(hex: &str) -> Vec<String> {
    let output = Command::new("fc-list").arg(format!(":charset={hex}")).output().unwrap();
    let families: HashSet<String> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let parts = line.split(',').collect::<Vec<_>>();
            if let Some(first_part) = parts.first() {
                let font_parts = first_part.split(':').collect::<Vec<_>>();
                if let Some(font_name) = font_parts.get(1) {
                    let cleaned = font_name.trim().replace('\\', "");
                    if !cleaned.is_empty() {
                        return Some(cleaned);
                    }
                }
            }
            None
        })
        .collect();
    let mut families = families.into_iter().collect::<Vec<_>>();
    families.sort();
    families
}

fn process_block(block: &Block, tx: &Sender<ProgressMsg>) -> Value {
    let total_chars = (block.end - block.start + 1) as usize;
    tx.send(ProgressMsg::Started(block.name.clone(), total_chars, Instant::now())).ok();
    let start_hex = to_hex(block.start);
    let end_hex = to_hex(block.end);
    let chars = (block.start..=block.end)
        .enumerate()
        .map(|(i, codepoint)| {
            tx.send(ProgressMsg::Progress(block.name.clone(), i + 1)).ok();
            let hex = to_hex(codepoint);
            let families = get_font_families(&hex);
            json!({
                "codepoint": hex,
                "families": families
            })
        })
        .collect::<Vec<_>>();
    tx.send(ProgressMsg::Finished(block.name.clone())).ok();
    json!({
        "name": block.name,
        "start": start_hex,
        "startdec": block.start,
        "end": end_hex,
        "chars": chars
    })
}

fn make_progress_bar(current: usize, total: usize, width: usize) -> String {
    let progress = (current as f64 / total as f64) * width as f64;
    let full_blocks = progress.floor() as usize;
    let fractional = progress.fract();
    let mut bar = "█".repeat(full_blocks);
    if full_blocks < width {
        let fractional_char = match (fractional * 8.) as u32 {
            0 => ' ',
            1 => '▏',
            2 => '▎',
            3 => '▍',
            4 => '▌',
            5 => '▋',
            6 => '▊',
            7 => '▉',
            _ => '█',
        };
        bar.push(fractional_char);
    }
    format!("{bar:width$}")
}

fn block_has_recent_chars(block: &Block, version_info: &VersionInfo) -> bool {
    (block.start..=block.end)
        .any(|cp| version_info.codepoints.get(&cp).is_some_and(|v| v == &version_info.latest))
}

fn print_status(
    block_progress: &HashMap<String, (usize, usize, Instant)>,
    block_order: &[String],
    finished: i32,
    recent_blocks: &HashSet<String>,
) {
    if block_progress.is_empty() {
        return;
    }
    let lines: Vec<String> = block_order
        .iter()
        .filter_map(|name| {
            block_progress.get(name).map(|(current, total, start)| {
                let percentage =
                    if *total > 0 { (*current as f64 / *total as f64) * 100.0 } else { 0.0 };
                (name.clone(), *current, *total, percentage, start)
            })
        })
        .clone()
        .map(|(name, current, total, percentage, start)| {
            let is_recent = recent_blocks.contains(&name);
            let just_started = start.elapsed() <= Duration::from_secs(1);
            let (color_start, color_end) = if just_started {
                ("\x1b[92m", "\x1b[0m")
            } else if is_recent {
                ("\x1b[93m", "\x1b[m")
            } else {
                ("", "")
            };
            format!(
                "{percentage:3.0}% \x1b[40m{}\x1b[m {color_start}{name}{color_end}",
                make_progress_bar(current, total, 12)
            )
        })
        .collect();
    eprintln!(
        "\x1b[1;34mcurrently processing ({finished} chunks finished):\x1b[0m\n{}",
        lines.join("\n")
    );
}
fn main() {
    let start_time = Instant::now();
    let version_info = fetch_version_info();
    println!("latest unicode version: {}", version_info.latest);
    let (tx, rx) = mpsc::channel();
    let last_line_count = Arc::new(AtomicUsize::new(0));
    let blocks = fetch_blocks();
    let chunks: Vec<Block> = blocks.iter().flat_map(chunk_block).collect();
    println!("there are {} blocks and {} chunks", blocks.len(), chunks.len());
    let recent_blocks: HashSet<String> = chunks
        .iter()
        .filter(|chunk| block_has_recent_chars(chunk, &version_info))
        .map(|chunk| chunk.name.clone())
        .collect();
    let progress_handle = thread::spawn(move || {
        let mut block_progress: HashMap<String, (usize, usize, Instant)> = HashMap::new();
        let mut block_order = vec![];
        let mut finished = 0;
        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(ProgressMsg::Started(name, total, now)) => {
                    block_progress.insert(name.clone(), (0, total, now));
                    block_order.push(name);
                }
                Ok(ProgressMsg::Progress(name, current)) => {
                    if let Some((_, total, start)) = block_progress.get(&name) {
                        block_progress.insert(name, (current, *total, *start));
                    }
                }
                Ok(ProgressMsg::Finished(name)) => {
                    block_progress.remove(&name);
                    block_order.retain(|n| n != &name);
                    finished += 1;
                }
                Ok(ProgressMsg::Done) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if last_line_count.load(LoadOrder::Relaxed) > 0 {
                eprint!("\x1b[{}A\x1b[J", last_line_count.load(LoadOrder::Relaxed));
            }
            if block_progress.is_empty() {
                last_line_count.store(0, LoadOrder::Relaxed);
            } else {
                print_status(&block_progress, &block_order, finished, &recent_blocks);
                last_line_count.store(block_progress.len() + 1, LoadOrder::Relaxed);
            }
        }
    });
    let mut data = chunks.par_iter().map(|c| process_block(c, &tx)).collect::<Vec<_>>();
    tx.send(ProgressMsg::Done).ok();
    progress_handle.join().unwrap();
    data.sort_by(|a, b| {
        let a_start = a["startdec"].as_u64().unwrap();
        let b_start = b["startdec"].as_u64().unwrap();
        a_start.cmp(&b_start)
    });
    let json_data = serde_json::to_string_pretty(&data).unwrap() + "\n";
    fs::write("data.json", &json_data).unwrap();
    let elapsed = start_time.elapsed();
    println!("done :3 (took {elapsed:?})");
}

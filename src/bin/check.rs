#![allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use std::{
    cmp::Ordering as SortOrder,
    collections::{HashMap, HashSet},
    fs,
    process::Command,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicUsize, Ordering as LoadOrder},
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use rayon::prelude::*;
use reqwest::blocking::Client;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
struct Block {
    name: String,
    start: u32,
    end: u32,
}
#[derive(Debug)]
enum ProgressMsg {
    Started(String, usize),
    Progress(String, usize),
    Finished(String),
    Done,
}

#[derive(Debug)]
struct VersionInfo {
    codepoints: HashMap<u32, String>,
    latest: String,
}

fn try_fetch(url: &str) -> String {
    static CLIENT: LazyLock<Client> =
        LazyLock::new(|| Client::builder().timeout(Duration::from_secs(60)).build().unwrap());
    const RETRY_MAX: i32 = 3;
    for attempt in 0..RETRY_MAX {
        match CLIENT.get(url).send() {
            Ok(resp) => return resp.text().unwrap(),
            Err(e) if attempt < RETRY_MAX - 1 => {
                println!("retry {}/{RETRY_MAX} for {url}: {e}", attempt + 1);
                thread::sleep(Duration::from_secs(1));
            }
            Err(e) => panic!("failed after 3 attempts: {e}"),
        }
    }
    unreachable!()
}

fn version_compare(a: &str, b: &str) -> SortOrder {
    let a_parts = a.split('.').map(|s| s.parse().unwrap_or(0)).collect::<Vec<_>>();
    let b_parts = b.split('.').map(|s| s.parse().unwrap_or(0)).collect::<Vec<_>>();
    a_parts.cmp(&b_parts)
}
fn fetch_version_info() -> VersionInfo {
    let response = try_fetch("https://www.unicode.org/Public/UNIDATA/DerivedAge.txt");
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
    let response = try_fetch("https://www.unicode.org/Public/UNIDATA/Blocks.txt");
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
    const CHUNK_SIZE: u32 = 1024;
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
    tx.send(ProgressMsg::Started(block.name.clone(), total_chars)).ok();
    let start_hex = format!("{:04x}", block.start);
    let end_hex = format!("{:04x}", block.end);
    let chars = (block.start..=block.end)
        .enumerate()
        .map(|(i, codepoint)| {
            tx.send(ProgressMsg::Progress(block.name.clone(), i + 1)).ok();
            let hex = format!("{codepoint:04x}");
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

fn print_status(
    block_progress: &HashMap<String, (usize, usize)>,
    block_order: &[String],
    finished_chunks: i32,
    total_chunks: i32,
    recent_blocks: &HashSet<String>,
) {
    if block_progress.is_empty() {
        return;
    }
    let lines: Vec<String> = block_order
        .iter()
        .filter_map(|name| {
            block_progress.get(name).map(|(current, total)| {
                let percentage =
                    if *total > 0 { (*current as f64 / *total as f64 * 100.).floor() } else { 0.0 };
                (name.clone(), *current, *total, percentage)
            })
        })
        .clone()
        .map(|(name, current, total, percentage)| {
            let color_start = if recent_blocks.contains(&name) { "\x1b[93m" } else { "" };
            format!(
                "{percentage:3.0}% {} {color_start}{name}\x1b[m",
                make_progress_bar(current, total, 12),
            )
        })
        .collect();
    eprintln!(
        "\x1b[1;34m{:3.0}% {} {finished_chunks} chunk{} completed\x1b[m\n{}",
        (f64::from(finished_chunks) / f64::from(total_chunks) * 100.).floor(),
        make_progress_bar(finished_chunks as usize, total_chunks as usize, 12),
        if finished_chunks == 1 { "" } else { "s" },
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
    let total_chunks = chunks.len();
    println!("there are {} blocks and {total_chunks} chunks", blocks.len());
    let recent_blocks: HashSet<String> = chunks
        .iter()
        .filter(|chunk| {
            (chunk.start..=chunk.end).any(|cp| {
                version_info.codepoints.get(&cp).is_some_and(|v| v == &version_info.latest)
            })
        })
        .map(|chunk| chunk.name.clone())
        .collect();
    let progress_handle = thread::spawn(move || {
        let mut block_progress: HashMap<String, (usize, usize)> = HashMap::new();
        let mut block_order = vec![];
        let mut finished = 0;
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(ProgressMsg::Started(name, total)) => {
                    block_progress.insert(name.clone(), (0, total));
                    block_order.push(name);
                }
                Ok(ProgressMsg::Progress(name, current)) => {
                    if let Some((_, total)) = block_progress.get(&name) {
                        block_progress.insert(name, (current, *total));
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
                print_status(
                    &block_progress,
                    &block_order,
                    finished,
                    total_chunks as i32,
                    &recent_blocks,
                );
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

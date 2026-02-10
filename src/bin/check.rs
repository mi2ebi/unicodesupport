#![allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use std::{
    collections::{HashMap, HashSet},
    fs,
    process::Command,
    sync::LazyLock,
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

fn try_fetch(url: &str) -> String {
    static CLIENT: LazyLock<Client> =
        LazyLock::new(|| Client::builder().timeout(Duration::from_mins(1)).build().unwrap());
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

fn build_codepoint_map() -> HashMap<u32, Vec<String>> {
    println!("building font map from fc-list...");
    let output = Command::new("fc-list").args([":", "family", "charset"]).output().unwrap();
    let mut map: HashMap<u32, HashSet<String>> = HashMap::new();
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        let Some((families_part, charset_part)) = line.split_once(":charset=") else {
            continue;
        };
        let base_family = families_part.split(',').next().unwrap().trim().replace('\\', "");
        if base_family.is_empty() {
            continue;
        }
        for range in charset_part.split_whitespace() {
            if let Some((start, end)) = range.split_once('-') {
                let start_cp = u32::from_str_radix(start, 16).unwrap();
                let end_cp = u32::from_str_radix(end, 16).unwrap();
                for cp in start_cp..=end_cp {
                    map.entry(cp).or_default().insert(base_family.clone());
                }
            } else {
                let cp = u32::from_str_radix(range, 16).unwrap();
                map.entry(cp).or_default().insert(base_family.clone());
            }
        }
    }
    map.into_iter()
        .map(|(cp, families)| {
            let mut families = families.into_iter().collect::<Vec<_>>();
            families.sort();
            (cp, families)
        })
        .collect()
}

fn process_block(block: &Block, font_map: &HashMap<u32, Vec<String>>) -> Value {
    let start_hex = format!("{:04x}", block.start);
    let end_hex = format!("{:04x}", block.end);
    let chars = (block.start..=block.end)
        .map(|codepoint| {
            let hex = format!("{codepoint:04x}");
            let families = font_map.get(&codepoint).cloned().unwrap_or_default();
            json!({
                "codepoint": hex,
                "families": families
            })
        })
        .collect::<Vec<_>>();
    json!({
        "name": block.name,
        "start": start_hex,
        "startdec": block.start,
        "end": end_hex,
        "chars": chars
    })
}

fn main() {
    let start_time = Instant::now();
    let font_map = build_codepoint_map();
    println!("font map built with {} codepoints", font_map.len());
    let blocks = fetch_blocks();
    println!("there are {} blocks", blocks.len());
    let mut data = blocks.par_iter().map(|c| process_block(c, &font_map)).collect::<Vec<_>>();
    data.sort_by(|a, b| {
        let a_start = a["startdec"].as_u64().unwrap();
        let b_start = b["startdec"].as_u64().unwrap();
        a_start.cmp(&b_start)
    });
    let json_data = serde_json::to_string_pretty(&data).unwrap() + "\n";
    fs::write("data.json", &json_data).unwrap();
    let elapsed = start_time.elapsed();
    println!("finished in {elapsed:?}");
}

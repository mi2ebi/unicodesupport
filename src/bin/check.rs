#![allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "annoying"
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
use serde_json::{Map, Value, json};

#[derive(Debug, Clone)]
struct Block {
    name: String,
    start: u32,
    end: u32,
}
#[derive(Debug, Clone)]
struct Script {
    name: String,
    start: u32,
    end: u32,
}

fn try_fetch(url: &str) -> Option<String> {
    static CLIENT: LazyLock<Client> =
        LazyLock::new(|| Client::builder().timeout(Duration::from_mins(1)).build().unwrap());
    const RETRY_MAX: i32 = 3;
    for attempt in 0 .. RETRY_MAX {
        match CLIENT.get(url).send() {
            Ok(resp) => return Some(resp.text().unwrap()),
            Err(e) if attempt < RETRY_MAX - 1 => {
                println!("retry {}/{RETRY_MAX} for {url}: {e}", attempt + 1);
                thread::sleep(Duration::from_secs(1));
            }
            Err(_) => return None,
        }
    }
    unreachable!()
}

fn fetch_blocks() -> Vec<Block> {
    let response =
        try_fetch("https://www.unicode.org/Public/draft/ucd/Blocks.txt").unwrap_or_else(|| {
            try_fetch("https://www.unicode.org/Public/latest/ucd/Blocks.txt")
                .unwrap_or_else(|| panic!("ohno"))
        });
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

fn fetch_scripts() -> Vec<Script> {
    let response = try_fetch("https://www.unicode.org/Public/draft/ucd/Scripts.txt")
        .unwrap_or_else(|| {
            try_fetch("https://www.unicode.org/Public/latest/ucd/Scripts.txt")
                .unwrap_or_else(|| panic!("ohno"))
        });
    response
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| line.split('#').next().unwrap().trim())
        .filter(|line| !line.is_empty())
        .map(|line| {
            let parts = line.split(';').collect::<Vec<_>>();
            let range = parts[0].trim().split("..").collect::<Vec<_>>();
            let start = u32::from_str_radix(range[0], 16).unwrap();
            let end =
                if range.len() == 2 { u32::from_str_radix(range[1], 16).unwrap() } else { start };
            Script { name: parts[1].trim().to_string(), start, end }
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
                for cp in start_cp ..= end_cp {
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
    let chars = (block.start ..= block.end)
        .filter_map(|codepoint| {
            let families = font_map.get(&codepoint)?;
            Some((format!("{codepoint:04x}"), json!(families)))
        })
        .collect::<Map<String, Value>>();
    json!({
        "name": block.name,
        "start": start_hex,
        "startdec": block.start,
        "end": end_hex,
        "chars": chars
    })
}

fn invert_map(cp_map: &HashMap<u32, Vec<String>>) -> HashMap<String, Vec<u32>> {
    let mut font_map: HashMap<String, Vec<u32>> = HashMap::new();
    #[allow(clippy::iter_over_hash_type, reason = "making another hashmap")]
    for (&cp, families) in cp_map {
        for family in families {
            font_map.entry(family.clone()).or_default().push(cp);
        }
    }
    font_map
}

fn to_ranges(mut cps: Vec<u32>) -> Vec<(u32, u32)> {
    cps.sort_unstable();
    let mut ranges = Vec::new();
    let mut start = cps[0];
    let mut prev = cps[0];
    for &cp in &cps[1 ..] {
        if cp != prev + 1 {
            ranges.push((start, prev));
            start = cp;
        }
        prev = cp;
    }
    ranges.push((start, prev));
    ranges
}

fn font_ranges_to_json(map: &HashMap<String, Vec<(u32, u32)>>) -> Value {
    let mut fonts = map.iter().collect::<Vec<_>>();
    fonts.sort_by_key(|(a, _)| a.to_lowercase());
    let fonts = fonts
        .into_iter()
        .map(|(font, ranges)| {
            let ranges_json = ranges
                .iter()
                .map(|(start, end)| {
                    if start == end {
                        json!(format!("{start:04x}"))
                    } else {
                        json!([format!("{start:04x}"), format!("{end:04x}")])
                    }
                })
                .collect::<Vec<_>>();
            json!({
                "font": font,
                "ranges": ranges_json
            })
        })
        .collect::<Vec<_>>();
    json!(fonts)
}

fn build_script_totals(scripts: &[Script]) -> HashMap<String, usize> {
    let mut totals = HashMap::new();
    for script in scripts {
        let count =
            (script.start ..= script.end).filter(|cp| char::from_u32(*cp).is_some()).count();
        *totals.entry(script.name.clone()).or_default() += count;
    }
    totals
}

fn build_font_script_coverage(scripts: &[Script], cp_map: &HashMap<u32, Vec<String>>) -> Value {
    let script_totals = build_script_totals(scripts);
    let mut per_font: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for script in scripts {
        for cp in script.start ..= script.end {
            if char::from_u32(cp).is_none() {
                continue;
            }
            let Some(fonts) = cp_map.get(&cp) else {
                continue;
            };
            for font in fonts {
                *per_font
                    .entry(font.clone())
                    .or_default()
                    .entry(script.name.clone())
                    .or_default() += 1;
            }
        }
    }
    let mut fonts = per_font.into_iter().collect::<Vec<_>>();
    fonts.sort_by_key(|(name, _)| name.to_lowercase());
    json!(
        fonts
            .into_iter()
            .map(|(font, counts)| {
                let mut scripts_json = Map::new();
                let mut scripts_sorted = counts.into_iter().collect::<Vec<_>>();
                scripts_sorted.sort_by_key(|(name, _)| name.clone());
                for (script, supported) in scripts_sorted {
                    let total = script_totals[&script];
                    if supported == 0 || total == 0 {
                        continue;
                    }
                    let pct = supported as f64 / total as f64 * 100.;
                    scripts_json.insert(script, json!(pct));
                }
                json!({
                    "font": font,
                    "scripts": scripts_json
                })
            })
            .collect::<Vec<_>>()
    )
}

fn main() {
    let start_time = Instant::now();
    let cp_map = build_codepoint_map();
    println!("codepoint map built with {} codepoints", cp_map.len());
    let blocks = fetch_blocks();
    let scripts = fetch_scripts();
    println!(
        "there are {} blocks and {} scripts",
        blocks.len(),
        scripts.iter().map(|s| &s.name).collect::<HashSet<_>>().len()
    );
    let mut data = blocks.par_iter().map(|c| process_block(c, &cp_map)).collect::<Vec<_>>();
    data.sort_by(|a, b| {
        let a_start = a["startdec"].as_u64().unwrap();
        let b_start = b["startdec"].as_u64().unwrap();
        a_start.cmp(&b_start)
    });
    for b in &mut data {
        b.as_object_mut().unwrap().remove("startdec");
    }
    let bjson = serde_json::to_string_pretty(&data).unwrap() + "\n";
    fs::write("blocks.json", &bjson).unwrap();
    let bjson_min = serde_json::to_string(&data).unwrap() + "\n";
    fs::write("blocks.min.json", &bjson_min).unwrap();
    let font_ranges = invert_map(&cp_map)
        .into_iter()
        .map(|(font, cps)| {
            let ranges = to_ranges(cps);
            (font, ranges)
        })
        .collect();
    println!("map inverted");
    let fjson = serde_json::to_string_pretty(&font_ranges_to_json(&font_ranges)).unwrap() + "\n";
    fs::write("fonts.json", &fjson).unwrap();
    let sjson = serde_json::to_string_pretty(&build_font_script_coverage(&scripts, &cp_map))
        .unwrap()
        + "\n";
    fs::write("font-scripts.json", &sjson).unwrap();
    let elapsed = start_time.elapsed();
    println!("finished in {elapsed:?}");
    println!(
        "blocks:  {:5.2} MiB pretty, {:5.2} MiB minified\nfonts:   {:5.2} MiB pretty\nscripts: \
         {:5.2} MiB pretty",
        to_mebi(bjson.len()),
        to_mebi(bjson_min.len()),
        to_mebi(fjson.len()),
        to_mebi(sjson.len()),
    );
}

fn to_mebi(bytes: usize) -> f64 { bytes as f64 / 1_048_576. }

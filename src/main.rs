#![allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "annoying"
)]

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs,
    process::Command,
    sync::LazyLock,
    thread,
    time::{Duration, Instant},
};

use rayon::prelude::*;
use reqwest::blocking::Client;
use serde_json::{Map, Value, json};

// data

#[derive(Debug, Clone)]
struct Block {
    name: String,
    start: u32,
    end: u32,
}
#[derive(Debug, Clone)]
struct Script {
    name: String,
    alias: String,
    start: u32,
    end: u32,
}
#[derive(Debug, Clone)]
struct ScriptExtension {
    start: u32,
    end: u32,
    scripts: Vec<String>,
}

fn try_fetch(url: &str) -> Option<String> {
    static CLIENT: LazyLock<Client> =
        LazyLock::new(|| Client::builder().timeout(Duration::from_mins(1)).build().unwrap());
    const RETRY_MAX: i32 = 3;
    for attempt in 0 .. RETRY_MAX {
        match CLIENT.get(url).send() {
            Ok(resp) => {
                let text = resp.text();
                match text {
                    Ok(text) => {
                        fs::write(url.split('/').next_back().unwrap(), &text).unwrap();
                        return Some(text);
                    }
                    Err(e) if attempt < RETRY_MAX - 1 => {
                        println!("retry {}/{RETRY_MAX} for {url}: {e}", attempt + 1);
                        thread::sleep(Duration::from_secs(1));
                    }
                    Err(_) => return None,
                }
            }
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
    let mut response = try_fetch("https://www.unicode.org/Public/draft/ucd/Blocks.txt");
    if response.clone().is_none_or(|r| r.starts_with('<')) {
        response = try_fetch("https://www.unicode.org/Public/latest/ucd/Blocks.txt");
    }
    let Some(response) = response else {
        panic!("ohno");
    };
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
    let mut response = try_fetch("https://www.unicode.org/Public/draft/ucd/Scripts.txt");
    if response.clone().is_none_or(|r| r.starts_with('<')) {
        response = try_fetch("https://www.unicode.org/Public/latest/ucd/Scripts.txt");
    }
    let Some(response) = response else {
        panic!("ohno");
    };
    let aliases = fetch_script_aliases();
    response
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| line.split('#').next().unwrap().trim())
        .filter(|line| !line.is_empty())
        .map(|line| {
            let parts = line.split(';').collect::<Vec<_>>();
            let range = parts[0].trim().split("..").collect::<Vec<_>>();
            let name = parts[1].trim().to_string();
            let alias = aliases.get(&name).cloned().unwrap_or_default();
            let start = u32::from_str_radix(range[0], 16).unwrap();
            let end =
                if range.len() == 2 { u32::from_str_radix(range[1], 16).unwrap() } else { start };
            Script { name, alias, start, end }
        })
        .collect()
}

fn fetch_script_aliases() -> HashMap<String, String> {
    let mut response =
        try_fetch("https://www.unicode.org/Public/draft/ucd/PropertyValueAliases.txt");
    if response.clone().is_none_or(|r| r.starts_with('<')) {
        response = try_fetch("https://www.unicode.org/Public/latest/ucd/PropertyValueAliases.txt");
    }
    let Some(response) = response else {
        panic!("ohno");
    };
    response
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            let parts = line.split('#').next()?.split(';').map(str::trim).collect::<Vec<_>>();
            if parts.len() < 3 || parts[0] != "sc" {
                return None;
            }
            Some((parts[2].to_string(), parts[1].to_string()))
        })
        .collect()
}

fn fetch_script_extensions() -> Vec<ScriptExtension> {
    let mut response = try_fetch("https://www.unicode.org/Public/draft/ucd/ScriptExtensions.txt");
    if response.clone().is_none_or(|r| r.starts_with('<')) {
        response = try_fetch("https://www.unicode.org/Public/latest/ucd/ScriptExtensions.txt");
    }
    let Some(response) = response else {
        panic!("ohno");
    };
    response
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            let line = line.split('#').next()?.trim();
            let parts = line.split(';').map(str::trim).collect::<Vec<_>>();
            let range = parts[0].split("..").collect::<Vec<_>>();
            let start = u32::from_str_radix(range[0], 16).unwrap();
            let end =
                if range.len() == 2 { u32::from_str_radix(range[1], 16).unwrap() } else { start };
            let scripts = parts[1].split_whitespace().map(str::to_string).collect();
            Some(ScriptExtension { start, end, scripts })
        })
        .collect()
}

fn build_codepoint_map() -> (HashMap<u32, Vec<String>>, HashSet<String>, HashSet<String>) {
    println!("building font map from fc-list...");
    let output = Command::new("fc-list")
        .args([":", "-f", "%{family[0]}\t%{charset}\t%{slant}\n"])
        .output()
        .unwrap();
    let mut map: HashMap<u32, HashSet<String>> = HashMap::new();
    let mut italic_families: HashSet<String> = HashSet::new();
    let mut families: HashSet<String> = HashSet::new();
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        let mut fields = line.splitn(3, '\t');
        let (Some(base_family), Some(charset_part), Some(slant_part)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let base_family = base_family.trim().to_string();
        if base_family.is_empty() {
            continue;
        }
        families.insert(base_family.clone());
        let slant: i32 = slant_part.trim().parse().unwrap_or(0);
        if slant >= 100 {
            italic_families.insert(base_family.clone());
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
    let map = map
        .into_iter()
        .map(|(cp, families)| {
            let mut families = families.into_iter().collect::<Vec<_>>();
            families.sort();
            (cp, families)
        })
        .collect();
    (map, italic_families, families)
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

fn font_ranges_to_json(
    map: &HashMap<String, Vec<(u32, u32)>>,
    italic_families: &HashSet<String>,
) -> Value {
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
                "italic": italic_families.contains(font),
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

// fallback

const BASE: &str = "iosevie";
const NERD_FONT: &str = "Symbols Nerd Font Mono";
const LAST_RESORT: &[&str] = &["Plangothic P1", "Plangothic P2", "Unifont", "Unifont Upper"];
const PLANGOTHIC: &[&str] = &["Plangothic P1", "Plangothic P2"];
const NEVER_SELECT: &[&str] = &["Unifont", "Unifont Upper", "Unifont-JP", "Unifont-T"];
const MONOSPACE: &[&str] = &[
    "Hack",
    "IBM Plex Mono",
    "iosevie",
    "Liberation Mono",
    "Noto Sans Mono",
    "Noto Sans Mono CJK SC",
    "Noto Sans Mono CJK TC",
    "Noto Sans Mono CJK JP",
    "Noto Sans Mono CJK HK",
    "Noto Sans Mono CJK KR",
    "Source Code Pro",
];

// hand-picked script fonts, in priority order. these go before (and win over)
// the fonts derived by `derive_script_fonts`, which gives every other script
// `Noto Sans <Script>` (or `Noto Serif`)
const SCRIPT_FONTS: &[(&str, &[&str])] = &[
    ("Arab", &["Noto Sans Arabic", "Amiri"]),
    ("Berf", &["Kedebideri"]),
    ("Egyp", &["NewGardiner"]),
    ("Ethi", &["Hibur Mono"]),
    ("Krai", &["Kanchenjunga"]),
];
// ideographic scripts that stay on the CJK/Plangothic fonts instead of getting
// a derived Noto font
const NO_DERIVE: &[&str] = &["Kits", "Nshu", "Tang"];

const EMOJI_FONTS: &[&str] = &["Noto Color Emoji"];
const EMOJI_BLOCKS: &[&str] = &[
    "Emoticons",
    "Enclosed Alphanumeric Supplement",
    "Enclosed Ideographic Supplement",
    "Geometric Shapes Extended",
    "Mahjong Tiles",
    "Miscellaneous Symbols and Pictographs",
    "Supplemental Symbols and Pictographs",
    "Symbols and Pictographs Extended-A",
    "Transport and Map Symbols",
];

type Predicate = fn(&str) -> bool;
const BLOCK_FONTS: &[(Predicate, &[&str])] = &[
    (|b| b.contains("Arabic"), &["Noto Sans Arabic"]),
    (|b| EMOJI_BLOCKS.contains(&b), EMOJI_FONTS),
    (|b| b == "Tamil Supplement", &["Noto Sans Tamil Supplement"]),
];

const CJK_SCRIPTS: &[&str] = &["Hani", "Hang", "Hira", "Kana", "Bopo"];
const CJK_FONTS: &[&str] = &[
    "Noto Sans Mono CJK SC",
    "Noto Sans Mono CJK TC",
    "Noto Sans Mono CJK JP",
    "Noto Sans Mono CJK HK",
    "Noto Sans Mono CJK KR",
    "Noto Sans CJK SC",
    "Noto Sans CJK TC",
    "Noto Sans CJK JP",
    "Noto Sans CJK HK",
    "Noto Sans CJK KR",
    "Plangothic P1",
    "Plangothic P2",
    "Jigmo",
    "Jigmo2",
    "Jigmo3",
];

/// Script alias -> (rank in priority order, fonts in priority order).
type ScriptFonts = HashMap<String, (usize, Vec<String>)>;

fn normalize(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// `Noto Sans <Script>` (or `Noto Serif <Script>` if there's no sans) for every
/// script that has one installed. bigger scripts come first, which settles
/// codepoints shared between several scripts but owned by none (danda, vedic
/// marks...) in favor of the biggest one.
#[allow(clippy::iter_over_hash_type, reason = "sorted right after")]
fn derive_script_fonts(
    families: &HashSet<String>,
    scripts: &[Script],
) -> Vec<(String, Vec<String>)> {
    let by_script = |prefix: &str| -> HashMap<String, &str> {
        families
            .iter()
            .filter_map(|family| Some((normalize(family.strip_prefix(prefix)?), family.as_str())))
            .collect()
    };
    let sans = by_script("Noto Sans ");
    let serif = by_script("Noto Serif ");
    let aliases = scripts
        .iter()
        .map(|script| (script.name.as_str(), script.alias.as_str()))
        .collect::<HashMap<_, _>>();
    let mut sizes = build_script_totals(scripts).into_iter().collect::<Vec<_>>();
    sizes.sort_by(|(a, a_size), (b, b_size)| b_size.cmp(a_size).then_with(|| a.cmp(b)));
    sizes
        .into_iter()
        .filter_map(|(name, _)| {
            let alias = aliases.get(name.as_str()).copied()?;
            if alias.is_empty() || NO_DERIVE.contains(&alias) {
                return None;
            }
            let key = normalize(&name);
            let font = *sans.get(&key).or_else(|| serif.get(&key))?;
            Some((alias.to_owned(), vec![font.to_owned()]))
        })
        .collect()
}

/// `SCRIPT_FONTS` first, then everything derived (behind the hand-picked fonts
/// of the same script).
fn build_script_fonts(families: &HashSet<String>, scripts: &[Script]) -> ScriptFonts {
    let mut ordered = SCRIPT_FONTS
        .iter()
        .map(|(alias, fonts)| {
            ((*alias).to_owned(), fonts.iter().map(|&font| font.to_owned()).collect::<Vec<_>>())
        })
        .collect::<Vec<_>>();
    let derived = derive_script_fonts(families, scripts);
    println!("derived fonts for {} scripts", derived.len());
    for (alias, fonts) in derived {
        if let Some((_, existing)) = ordered.iter_mut().find(|(name, _)| *name == alias) {
            for font in fonts {
                if !existing.contains(&font) {
                    existing.push(font);
                }
            }
        } else {
            ordered.push((alias, fonts));
        }
    }
    ordered.into_iter().enumerate().map(|(rank, (alias, fonts))| (alias, (rank, fonts))).collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b = b.chars().collect::<Vec<_>>();
    let mut row = (0 ..= b.len()).collect::<Vec<_>>();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != cb)).min(above + 1).min(row[j] + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

/// A font name that fc-list doesn't know about never matches anything and fails
/// silently (say, `Noto Sans Nko` for `Noto Sans NKo`), so shout about every
/// one of them.
#[allow(clippy::iter_over_hash_type, reason = "only looking for the closest family")]
fn validate_config(families: &HashSet<String>) {
    let mut named: Vec<(String, &str)> =
        vec![("BASE".to_owned(), BASE), ("NERD_FONT".to_owned(), NERD_FONT)];
    for (label, fonts) in [
        ("LAST_RESORT", LAST_RESORT),
        ("PLANGOTHIC", PLANGOTHIC),
        ("NEVER_SELECT", NEVER_SELECT),
        ("MONOSPACE", MONOSPACE),
        ("CJK_FONTS", CJK_FONTS),
        ("EMOJI_FONTS", EMOJI_FONTS),
    ] {
        named.extend(fonts.iter().map(|&font| (label.to_owned(), font)));
    }
    for (script, fonts) in SCRIPT_FONTS {
        named.extend(fonts.iter().map(|&font| (format!("SCRIPT_FONTS[{script}]"), font)));
    }
    for (index, (_, fonts)) in BLOCK_FONTS.iter().enumerate() {
        named.extend(fonts.iter().map(|&font| (format!("BLOCK_FONTS[{index}]"), font)));
    }
    let mut missing = 0;
    for (label, font) in named {
        if families.contains(font) {
            continue;
        }
        missing += 1;
        let lowercase = font.to_lowercase();
        let closest = families
            .iter()
            .map(|family| (edit_distance(&lowercase, &family.to_lowercase()), family))
            .filter(|&(distance, _)| distance <= 5)
            .min();
        match closest {
            Some((_, family)) => eprintln!(
                "warning: {label} names {font:?}, which isn't installed (did you mean {family:?}?)"
            ),
            None => eprintln!("warning: {label} names {font:?}, which isn't installed"),
        }
    }
    if missing == 0 {
        println!("all configured fonts are installed");
    }
}

#[derive(PartialEq, PartialOrd, Eq, Ord)]
struct Preference<'a> {
    cjk: usize,
    script: (usize, usize, usize),
    block: (usize, usize),
    plangothic: usize,
    proportional: bool,
    name: &'a str,
}

fn preference<'a>(
    font: &'a str,
    primary: &str,
    scripts: &[&str],
    block: &str,
    script_fonts: &ScriptFonts,
) -> Preference<'a> {
    let cjk = if scripts.iter().any(|script| CJK_SCRIPTS.contains(script)) {
        CJK_FONTS.iter().position(|&x| x == font).unwrap_or(usize::MAX)
    } else {
        usize::MAX
    };
    // a font listed for the codepoint's own script beats one listed for another
    // script that merely shares the codepoint (script extensions), whatever
    // the order of SCRIPT_FONTS. common and inherited codepoints have no
    // font listed for their primary script, so for them the order of
    // SCRIPT_FONTS still decides
    let script = scripts
        .iter()
        .filter_map(|&script| {
            let (script_rank, fonts) = script_fonts.get(script)?;
            let font_rank = fonts.iter().position(|candidate| candidate == font)?;
            Some((usize::from(script != primary), *script_rank, font_rank))
        })
        .min()
        .unwrap_or((usize::MAX, usize::MAX, usize::MAX));
    let block = BLOCK_FONTS
        .iter()
        .enumerate()
        .filter_map(|(block_rank, (matches, fonts))| {
            if !matches(block) {
                return None;
            }
            let font_rank = fonts.iter().position(|&candidate| candidate == font)?;
            Some((block_rank, font_rank))
        })
        .min()
        .unwrap_or((usize::MAX, usize::MAX));
    Preference {
        cjk,
        script,
        block,
        plangothic: PLANGOTHIC.iter().position(|&x| x == font).unwrap_or(usize::MAX),
        proportional: !MONOSPACE.contains(&font),
        name: font,
    }
}

fn script_for(cp: u32, scripts: &[Script]) -> Option<&Script> {
    scripts.iter().find(|script| script.start <= cp && cp <= script.end)
}
/// The codepoint's `Script` and its `Script_Extensions` (just the `Script` if
/// it has none).
fn script_extensions_for<'a>(
    cp: u32,
    scripts: &'a [Script],
    extensions: &'a [ScriptExtension],
) -> (&'a str, Vec<&'a str>) {
    let primary = script_for(cp, scripts).map_or("", |script| script.alias.as_str());
    if let Some(extension) =
        extensions.iter().find(|extension| extension.start <= cp && cp <= extension.end)
    {
        return (primary, extension.scripts.iter().map(String::as_str).collect());
    }
    (primary, if primary.is_empty() { vec![] } else { vec![primary] })
}

#[derive(Debug, Clone)]
struct Assignment {
    start: u32,
    end: u32,
    font: String,
}

fn choose_font(
    cp: u32,
    block: &Block,
    font_map: &HashMap<u32, Vec<String>>,
    scripts: &[Script],
    extensions: &[ScriptExtension],
    script_fonts: &ScriptFonts,
) -> Option<String> {
    let candidates = font_map.get(&cp)?;
    let (primary, scripts) = script_extensions_for(cp, scripts, extensions);
    candidates
        .iter()
        .filter(|font| !NEVER_SELECT.contains(&font.as_str()))
        .min_by_key(|font| preference(font, primary, &scripts, &block.name, script_fonts))
        .cloned()
}

fn build_assignments(
    block: &Block,
    font_map: &HashMap<u32, Vec<String>>,
    scripts: &[Script],
    extensions: &[ScriptExtension],
    script_fonts: &ScriptFonts,
) -> Vec<Assignment> {
    let mut assignments: Vec<Assignment> = vec![];
    for cp in block.start ..= block.end {
        if cp <= 0xff || (0xe000 .. 0xf900).contains(&cp) || cp >= 0xf0000 {
            continue;
        }
        if font_map.get(&cp).is_some_and(|fonts| fonts.iter().any(|font| font == BASE)) {
            continue;
        }
        let Some(font) = choose_font(cp, block, font_map, scripts, extensions, script_fonts) else {
            continue;
        };
        match assignments.last_mut() {
            Some(last) if last.font == font && last.end + 1 == cp => {
                last.end = cp;
            }
            _ => assignments.push(Assignment { start: cp, end: cp, font }),
        }
    }
    assignments
}

fn elisp_codepoint(cp: u32) -> String { format!("#x{cp:x}") }

fn write_assignment(out: &mut String, assignment: &Assignment) {
    if assignment.start == assignment.end {
        let _ = writeln!(
            out,
            "(set-fontset-font t {} {:?})",
            elisp_codepoint(assignment.start),
            assignment.font
        );
    } else {
        let _ = writeln!(
            out,
            "(set-fontset-font t '({} . {}) {:?})",
            elisp_codepoint(assignment.start),
            elisp_codepoint(assignment.end),
            assignment.font
        );
    }
}

fn generate_elisp(
    blocks: &[Block],
    font_map: &HashMap<u32, Vec<String>>,
    scripts: &[Script],
    extensions: &[ScriptExtension],
    script_fonts: &ScriptFonts,
) -> String {
    let mut out = String::new();
    out += ";;; unicode font fallbacks\n\n";
    for block in blocks {
        let _ = writeln!(out, ";; {}", block.name);
        for assignment in build_assignments(block, font_map, scripts, extensions, script_fonts) {
            write_assignment(&mut out, &assignment);
        }
        out += "\n";
    }
    let _ = writeln!(out, ";; private use areas");
    let _ = writeln!(out, "(set-fontset-font t '(#xe000 . #xf8ff) {NERD_FONT:?})");
    let _ = writeln!(out, "(set-fontset-font t '(#xf0000 . #xfffff) {NERD_FONT:?})\n");
    out += ";; last resort\n";
    for font in LAST_RESORT {
        let _ = writeln!(out, "(set-fontset-font t 'unicode {font:?} nil 'append)");
    }
    out += "\n(provide 'unicode)\n";
    out
}

// main

fn main() {
    let start_time = Instant::now();
    let (cp_map, italic_families, families) = build_codepoint_map();
    println!("codepoint map built");
    validate_config(&families);
    let blocks = fetch_blocks();
    let scripts = fetch_scripts();
    let extensions = fetch_script_extensions();
    println!("got all unicode files");
    let script_fonts = build_script_fonts(&families, &scripts);
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
    let fjson = serde_json::to_string_pretty(&font_ranges_to_json(&font_ranges, &italic_families))
        .unwrap()
        + "\n";
    fs::write("fonts.json", &fjson).unwrap();
    let sjson = serde_json::to_string_pretty(&build_font_script_coverage(&scripts, &cp_map))
        .unwrap()
        + "\n";
    fs::write("font-scripts.json", &sjson).unwrap();
    let elisp = generate_elisp(&blocks, &cp_map, &scripts, &extensions, &script_fonts);
    println!("elisp generated");
    fs::write("unicode-font-fallbacks.el", &elisp).unwrap();
    let elapsed = start_time.elapsed();
    println!("finished in {elapsed:?}");
    println!(
        "blocks:  {:5.2} MiB pretty, {:5.2} MiB minified\nfonts:   {:5.2} MiB pretty\nscripts: \
         {:5.2} MiB pretty\nelisp:   {:5.2} MiB",
        to_mebi(bjson.len()),
        to_mebi(bjson_min.len()),
        to_mebi(fjson.len()),
        to_mebi(sjson.len()),
        to_mebi(elisp.len()),
    );
}

fn to_mebi(bytes: usize) -> f64 { bytes as f64 / 1_048_576. }

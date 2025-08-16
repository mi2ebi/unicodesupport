#!/usr/bin/env nu
def tohex [] {$in | (format number -n).lowerhex | fill -a right -c 0 -w 4}
def fetch-assigned [] {
  let lines = http get https://www.unicode.org/Public/UNIDATA/UnicodeData.txt
  | lines
  | where ($it | str trim | is-not-empty)
  let parsed = $lines | each {|line|
    let parts = $line | split row ";"
    {codepoint:$parts.0 name:$parts.1}
  }
  let normal = $parsed
  | where not ($it.name | str ends-with ", First>")
  | where not ($it.name | str ends-with ", Last>")
  | get codepoint
  | str downcase
  let ranges = $parsed | enumerate
  | where ($it.item.name | str ends-with ", First>")
  | each {|f|
    let start = $f.item.codepoint | into int -r 16
    let end = ($parsed | get ($f.index + 1)).codepoint | into int -r 16
    $start..$end | par-each {|i| $i | tohex} | sort
  }
  | flatten
  [$normal $ranges] | flatten
}
def is-pua [block] {
  ($block | str contains -i "private") or ($block | str contains -i "surrogates")
}
def format-range [list:list<string> assigned:list<string>] {
  let assignedonly = $list | where {|hex| $hex in $assigned}
  if ($list | is-empty) or ($assignedonly | is-empty) {return ""}
  let sorted = $assignedonly | par-each {|hex| $hex | into int -r 16} | sort
  mut ranges = []
  mut start = -1
  mut prev = -1
  for codepoint in $sorted {
    if $start == -1 {
      $start = $codepoint
    } else if $codepoint != ($prev + 1) {
      if $start == $prev {
        $ranges = $ranges | append ($start | tohex)
      } else {
        $ranges = $ranges | append $"($start | tohex)-($prev | tohex)"
      }
      $start = $codepoint
    }
    $prev = $codepoint
  }
  if $start != -1 {
    if $start == $prev {
      $ranges = $ranges | append ($start | tohex)
    } else {
      $ranges = $ranges | append $"($start | tohex)-($prev | tohex)"
    }
  }
  $ranges | str join ", "
}
def main [
  --codepoint(-c):string # codepoint to search for
  --font     (-f):string # show coverage per block for this font
  --block    (-b):string # show font coverage for this block
] {
  let data = open data.json
  if ($codepoint | is-not-empty) {
    let codepoint = $codepoint
    | into int -r 16
    | (format number -n).lowerhex
    | fill -a right -c 0 -w 4
    $data
    | where {|block| $block.chars | any {|char| $char.codepoint == $codepoint}}
    | get chars
    | flatten
    | where codepoint == $codepoint
    | flatten
    | get families
  } else if ($font | is-not-empty) {
    print "fetching assigned codepoints..."
    let assigned = fetch-assigned
    $data
    | where {|block|
      $block.chars | where {|char| $char.families | any {|fam|
        ($fam | str downcase) == ($font | str downcase)
      }} | is-not-empty
    }
    | each {|block|
      let len = $block.chars | where {|char| $char.codepoint in $assigned} | length
      let supportedlist = $block.chars
      | where {|char| $char.families | any {|fam|
        ($fam | str downcase) == ($font | str downcase)
      }}
      | get codepoint
      let supportedlen = $supportedlist | length
      let pct = $supportedlen / $len * 100 | math round
      if ($pct == 0) {null} else {
        let missing = $block.chars
        | where {|char| $char.families | all {|fam|
          ($fam | str downcase) != ($font | str downcase)
        }}
        | get codepoint
        let has = format-range $supportedlist $assigned
        let hastext = if ($pct == 100) or ($has | is-empty) {""} else {
          $"\n(ansi green)yes(ansi reset) ($has)"
        }
        let gaps = format-range $missing $assigned
        let gaptext = if ($gaps | is-empty) or (is-pua $block.name) {""} else {
          $"\n(ansi red)no (ansi reset) ($gaps)"
        }
        $"(ansi yellow)($block.name)(ansi reset) ($pct)%($hastext)($gaptext)"
      }
    }
    | compact | each {print}
    return
  }
}

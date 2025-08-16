#!/usr/bin/env nu
def fetch-blocks [] {
  http get https://www.unicode.org/Public/UNIDATA/Blocks.txt
  | lines
  | where not ($it | str starts-with "#")
  | where not ($it | str trim | is-empty)
  | each {|line|
    let parts = $line | split row ";"
    let range = $parts.0 | str trim | split row ..
    let name = $parts.1 | str trim
    {
      name: $name
      start: ($range.0 | into int -r 16)
      end: ($range.1 | into int -r 16)
    }
  }
}
def chunk [block] {
  let len = $block.end - $block.start + 1
  if $len >= 4096 {
    let chunks = $len / 4096 | math ceil
    0..<$chunks | each {|i|
      let chunkstart = $block.start + $i * 4096
      let chunkend = [($chunkstart + 4095) $block.end] | math min
      {
        name: $"($block.name) \(chunk ($i))"
        start: $chunkstart
        end: $chunkend
      }
    }
  } else {
    [$block]
  }
}
def tohex [] {$in | (format number -n).lowerhex | fill -a right -c 0 -w 4}
def process-block [name:string start:int end:int] {
  let starthex = $start | tohex 
  let endhex = $end | tohex
  let len = $end - $start + 1
  print $"(ansi green)'($name)'(ansi reset) ($starthex)-($endhex)" --stderr
  let starttime = date now
  let res = $start..$end | enumerate | each {|item|
    let i = $item.item
    let index = $item.index + 1
    let hex = $i | tohex
    let now = date now
    let elapsed = $now - $starttime
    if $elapsed mod 20sec < 50ms and $elapsed > 1sec {
      let eta = $elapsed / $index * ($len - $index) / 1sec | math round | $in * 1sec
      print $"(ansi yellow)'($name)'(ansi reset) u+($hex) (ansi blue)($eta)(ansi reset)"
    } 
    let families = (
      fc-list $":charset=($hex)"
      | lines
      | each {|line|
        $line
        | split column , | get column1
        | split column : | get column2
        | str trim
        | str replace \ "" -a
      }
      | flatten | uniq | sort
    )
    {codepoint:$hex families:$families}
  }
  $res
}
def main [] {
  rm -f tmp.json
  timeit {
    let blocks = fetch-blocks
    let chunks = $blocks | each {|b| chunk $b} | flatten
    print $"there are ($blocks | length) blocks"
    let data = $chunks
    | par-each {|block| {
      name: $block.name
      start: ($block.start | tohex)
      startdec: $block.start
      end: ($block.end | tohex)
      chars: (process-block $block.name $block.start $block.end)
    }}
    | sort-by startdec
    $data | to json | save tmp.json
    mv tmp.json data.json
    print "done :3"
  }
}

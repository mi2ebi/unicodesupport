#!/usr/bin/env nu
def main [
  --codepoint (-c): string # codepoint to search for
] {
  let data = open ~/Projects/unicodesupport/data.json
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
  }
}

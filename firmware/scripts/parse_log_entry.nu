# Parse base64-encoded log entries into human-readable format.
#
# Usage:
#     parse-log-entry <base64_entry>
#     sdmmc open logs | each { |raw| parse-log-entry $raw }
#
# Wire format:
#     [id:4][level:1][task:2][idx:2][time:8][len:2][payload:len]
#
# Packed time (u64 LE):
#     bits  0..15  year
#     bits 16..23  month
#     bits 24..31  day
#     bits 32..39  weekday
#     bits 40..47  hour
#     bits 48..55  minute
#     bits 56..63  second

const LEVELS = [PANIC ERROR WARN INFO DEBUG TRACE]
const HEADER_SIZE = 19

const SCRIPT_DIR = (path self | path dirname)

def task-names [] {
    let kdl = ($SCRIPT_DIR | path join ".." "app.kdl")
    if ($kdl | path exists) {
        open $kdl | lines | where { |l| ($l | str trim | str starts-with 'task ') }
        | each { |l|
            let s = ($l | str trim)
            let start = ($s | str index-of '"') + 1
            let end = ($s | str substring $start.. | str index-of '"') + $start
            $s | str substring $start..($end - 1)
        }
    } else {
        []
    }
}

def unpack-time [raw: binary] {
    # Time is 8 bytes LE at offset 9..16
    let year = ($raw | bytes at 9..10 | into int --endian little)
    let month = ($raw | bytes at 11..11 | into int)
    let day = ($raw | bytes at 12..12 | into int)
    # byte 13 = weekday (skip)
    let hour = ($raw | bytes at 14..14 | into int)
    let minute = ($raw | bytes at 15..15 | into int)
    let second = ($raw | bytes at 16..16 | into int)

    if $year == 0 and $month == 0 and $day == 0 {
        null
    } else {
        let mo = ($month | fill -a right -w 2 -c '0')
        let d = ($day | fill -a right -w 2 -c '0')
        let h = ($hour | fill -a right -w 2 -c '0')
        let mi = ($minute | fill -a right -w 2 -c '0')
        let s = ($second | fill -a right -w 2 -c '0')
        $"($year)-($mo)-($d)T($h):($mi):($s)+00:00" | into datetime
    }
}

# Parse a single log entry from binary data.
def parse-log-binary [raw: binary] {
    if ($raw | bytes length) < $HEADER_SIZE {
        return null
    }

    let id = ($raw | bytes at 0..3 | into int --endian little)
    let level = ($raw | bytes at 4..4 | into int)
    let task_idx = ($raw | bytes at 5..6 | into int --endian little)
    let idx = ($raw | bytes at 7..8 | into int --endian little)
    let data_len = ($raw | bytes at 17..18 | into int --endian little)

    let time = (unpack-time $raw)
    let lvl = if $level < ($LEVELS | length) { $LEVELS | get $level } else { $"?($level)" }
    let names = (task-names)
    let task = if $task_idx < ($names | length) { $names | get $task_idx } else { $"task($task_idx)" }
    let payload = ($raw | bytes at $HEADER_SIZE..($HEADER_SIZE + $data_len - 1) | decode utf-8)

    {
        id: ($id | format number --no-prefix | get lowerhex | fill -a right -w 8 -c '0')
        idx: $idx
        timestamp: $time
        level: $lvl
        task: $task
        message: $payload
    }
}

# Parse a base64-encoded log entry, or binary data directly.
def parse-log-entry [entry?: any] {
    let input = if $entry != null { $entry } else { $in }
    let raw = if ($input | describe | str starts-with "binary") {
        $input
    } else {
        $input | decode base64
    }
    parse-log-binary $raw
}

def main [entry?: string] {
    def fmt [r] {
        let ts = if $r.timestamp != null {
            $r.timestamp | format date "%d/%m/%y %H:%M:%S"
        } else {
            "????/??/?? ??:??:??"
        }
        $"($ts) [($r.level) ($r.task)] ($r.message)"
    }

    if $entry != null {
        print (fmt (parse-log-entry $entry))
    } else {
        for line in ($in | lines) {
            let line = ($line | str trim)
            if ($line | is-empty) { continue }
            print (fmt (parse-log-entry $line))
        }
    }
}

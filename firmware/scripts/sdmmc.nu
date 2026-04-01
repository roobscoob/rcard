# SDMMC partition tooling
#
# Operates on the sdmmc.img disk image using the partition layout
# from .work/app.partitions.json.

const BLOCK_SIZE = 512
const IMG_PATH = "build/sdmmc.img"
const PARTITIONS_JSON = ".work/app.partitions.json"

def project-root [] {
    $env.PWD
}

def partitions-json [] {
    let root = (project-root)
    let path = ($root | path join $PARTITIONS_JSON)
    if not ($path | path exists) {
        error make { msg: $"Partition JSON not found at ($path). Run the KDL preprocessor first." }
    }
    open $path
}

def resolve-img [override: any] {
    if $override != null {
        $override
    } else {
        project-root | path join $IMG_PATH
    }
}

# Ensure the sdmmc image file exists, creating a zero-filled one if needed.
def ensure-img [override: any] {
    let path = (resolve-img $override)
    if not ($path | path exists) {
        let json = (partitions-json)
        let parts = ($json.devices | values | flatten)
        let end = ($parts | each { |p| $p.offset_bytes + $p.size_bytes } | math max)
        let dir = ($path | path dirname)
        if not ($dir | path exists) { mkdir $dir }
        python -c $"open\(r'($path)', 'wb'\).write\(b'\\x00' * ($end)\)"
        print $"Created ($path) \(($end) bytes\)"
    }
    $path
}

# Look up a partition by name, returning its config dict.
def find-partition [name: string] {
    let json = (partitions-json)
    let parts = ($json.devices | values | flatten)
    let part = ($parts | where name == $name)
    if ($part | is-empty) {
        let known = ($parts | get name | str join ", ")
        error make { msg: $"Unknown partition '($name)'. Known: ($known)" }
    }
    $part | first
}

# Return the partition layout as a structured record.
def "sdmmc layout" [] {
    let json = (partitions-json)

    let block_devices = ($json.devices | columns | each { |device|
        let parts = ($json.devices | get $device)
        let part_records = ($parts | each { |p|
            {
                name: $p.name
                offset: $p.offset_bytes
                size: ($p.size_bytes | into filesize)
                format: $p.format
            }
        })
        { name: $device, partitions: $part_records }
    } | reduce -f {} { |it, acc| $acc | insert $it.name $it.partitions })

    let filesystems = if "filesystems" in ($json | columns) and (not ($json.filesystems | is-empty)) {
        $json.filesystems | columns | each { |fs|
            let maps = ($json.filesystems | get $fs | each { |m|
                { name: $m.name, source: $"($m.source_device)::($m.source_partition)" }
            })
            { name: $fs, maps: $maps }
        } | reduce -f {} { |it, acc| $acc | insert $it.name $it.maps }
    } else {
        {}
    }

    { block_devices: $block_devices, filesystems: $filesystems }
}

# List files in a littlefs partition.
# Usage: sdmmc ls p1:/path
def "sdmmc ls" [
    target: string
    --img: string  # Path to sdmmc image (default: build/sdmmc.img)
] {
    let parts = ($target | split row ":")
    if ($parts | length) != 2 {
        error make { msg: "Usage: sdmmc ls <partition>:<path>" }
    }
    let part_name = ($parts | first)
    let path = ($parts | last)
    let part = (find-partition $part_name)

    if $part.format != "littlefs" {
        error make { msg: $"Partition '($part_name)' has format '($part.format)', expected 'littlefs'" }
    }

    python (project-root | path join scripts _lfs.py) ls (resolve-img $img) $part.offset_bytes $part.size_bytes $path
}

# Read a file from a littlefs partition, or dump a raw/boot partition.
#
# For littlefs:  sdmmc open p1:/path/to/file
# For raw/boot:  sdmmc open bootloader
def "sdmmc open" [
    target: string
    --img: string  # Path to sdmmc image (default: build/sdmmc.img)
] {
    let resolved = (resolve-img $img)
    if (":" in $target) {
        # littlefs file read
        let parts = ($target | split row ":")
        let part_name = ($parts | first)
        let path = ($parts | last)
        let part = (find-partition $part_name)

        if $part.format != "littlefs" {
            error make { msg: $"Partition '($part_name)' has format '($part.format)', expected 'littlefs'" }
        }

        python (project-root | path join scripts _lfs.py) read $resolved $part.offset_bytes $part.size_bytes $path
    } else {
        # whole partition
        let part = (find-partition $target)
        if $part.format == "ringbuffer" {
            # Extract raw partition slice, pipe through ring buffer reader
            let tmp = (mktemp -t sdmmc_ring_XXXXXX)
            open $resolved | bytes at $part.offset_bytes..<($part.offset_bytes + $part.size_bytes) | save -f $tmp
            let lines = (python (project-root | path join scripts read_ringbuf.py) $tmp | lines | each { |l| $l | decode base64 })
            rm -f $tmp
            $lines
        } else if $part.format in [raw boot ftab] {
            open $resolved | bytes at $part.offset_bytes..<($part.offset_bytes + $part.size_bytes)
        } else {
            error make { msg: $"Partition '($target)' has format '($part.format)', expected 'raw', 'boot', or 'ringbuffer'" }
        }
    }
}

# Format a littlefs partition, optionally populating it from a folder.
def "sdmmc format littlefs" [
    name: string       # Partition name
    --with: string     # Optional: path to folder to populate the filesystem with
    --img: string      # Path to sdmmc image (default: build/sdmmc.img)
    --silent           # If set, suppress output (for scripting)
] {
    let part = (find-partition $name)
    if $part.format != "littlefs" {
        error make { msg: $"Partition '($name)' has format '($part.format)', expected 'littlefs'" }
    }

    let resolved = (ensure-img $img)
    if $with != null {
        python (project-root | path join scripts _lfs.py) format $resolved $part.offset_bytes $part.size_bytes $with
    } else {
        python (project-root | path join scripts _lfs.py) format $resolved $part.offset_bytes $part.size_bytes
    }

    if not $silent {
        print $"Formatted partition '($name)' as littlefs"
    }
}

# Pack raw data into a partition, zero-padding to fill.
def "sdmmc pack" [
    name: string    # Partition name
    file: string    # Path to file to write
    --img: string   # Path to sdmmc image (default: build/sdmmc.img)
] {
    let part = (find-partition $name)
    let file_size = (ls $file | first | get size)

    if $file_size > ($part.size_bytes | into filesize) {
        error make {
            msg: $"File is ($file_size) bytes but partition '($name)' is only ($part.size_bytes) bytes"
        }
    }

    let resolved = (ensure-img $img)

    # Write file data into the image at the partition offset, zero-padding the rest
    python (project-root | path join scripts _pack.py) $file $resolved $"($part.offset_bytes)" $"($part.size_bytes)"
}

def sdmmc [] {
    print "Usage: sdmmc <command>"
    print ""
    print "Commands:"
    print "  layout                          Print the partition layout"
    print "  ls <name>:<path>                List files in a littlefs partition"
    print "  open <name>:<path>              Read a file from a littlefs partition"
    print "  open <name>                     Dump raw data from a raw/boot partition"
    print "  format littlefs <name> [--with] Format a littlefs partition"
    print "  pack <name> <file>              Write raw file data into a partition"
    print ""
    print "All commands accept --img <path> to override the default image path."
}

# Entry point stubs for running as a script (nu scripts/sdmmc.nu <command>)
def main [] { sdmmc }
def "main layout" [] { sdmmc layout }
def "main ls" [target: string, --img: string] { sdmmc ls $target --img $img }
def "main open" [target: string, --img: string] { sdmmc open $target --img $img }
def "main format littlefs" [name: string, --with: string, --img: string] { sdmmc format littlefs $name --with $with --img $img }
def "main pack" [name: string, file: string, --img: string] { sdmmc pack $name $file --img $img }

use std::io::{Cursor, Read};

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump-places <path.tfw>");
    let data = std::fs::read(&path).expect("failed to read file");
    let mut archive = zip::ZipArchive::new(Cursor::new(&data)).expect("invalid zip");
    let places = {
        let mut entry = archive.by_name("places.bin").expect("no places.bin");
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).unwrap();
        buf
    };
    let image = rcard_places::PlacesImage::parse(&places).expect("invalid places.bin");

    println!("Entry point: {:#010x}", image.entry_point());
    println!();

    println!("Partitions ({}):", image.partition_count());
    for p in image.partitions() {
        println!(
            "  name_hash={:#010x}  offset={:#010x}  size={:#010x}  flags={:#x}",
            p.name_hash, p.offset, p.size, p.flags
        );
    }
    println!();

    println!("Segments ({}):", image.segment_count());
    for seg in image.segments() {
        let region = if seg.dest() >= 0x2000_0000 && seg.dest() < 0x4000_0000 {
            "RAM"
        } else if seg.dest() >= 0x1000_0000 && seg.dest() < 0x2000_0000 {
            "FLASH"
        } else {
            "???"
        };
        println!(
            "  [{region}]  dest={:#010x}  file_size={:#010x} ({:>6} B)  mem_size={:#010x} ({:>6} B)  zero_fill={} B",
            seg.dest(),
            seg.file_size(),
            seg.file_size(),
            seg.mem_size(),
            seg.mem_size(),
            seg.zero_fill(),
        );
    }
}

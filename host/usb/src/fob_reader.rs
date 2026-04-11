use device::device::LogSink;
use device::logs::LogEntry;
use nusb::transfer::RequestBuffer;
use rcard_log::decoder::{Decoder, FeedResult};
use rcard_log::LogMetadata;
use rcard_usb_proto::FrameReader;
use zerocopy::TryFromBytes;

use crate::error::UsbError;

const METADATA_SIZE: usize = core::mem::size_of::<LogMetadata>();
const ENTRY_HEADER: usize = 10; // log_id(8) + data_len(1) + fragment_idx(1)

/// Read frames from the fob-driven bulk IN endpoint and dispatch events.
pub(crate) async fn run(fob_iface: nusb::Interface, in_endpoint: u8, sink: LogSink) {
    let mut reader = FrameReader::<4096>::new();

    loop {
        let data = match fob_iface
            .bulk_in(in_endpoint, RequestBuffer::new(4096))
            .await
            .into_result()
        {
            Ok(data) => data,
            Err(e) => {
                sink.error(UsbError::Transfer(e));
                return;
            }
        };

        reader.push(&data);

        loop {
            match reader.next_frame() {
                Ok(Some(frame)) => {
                    let size = frame.header.frame_size();
                    dispatch_frame(&frame, &sink);
                    reader.consume(size);
                }
                Ok(None) => break,
                Err(rcard_usb_proto::ReaderError::Oversized { declared_size }) => {
                    sink.error(UsbError::FrameOversize { declared_size });
                    reader.skip_frame(declared_size);
                }
                Err(e) => {
                    sink.error(UsbError::BadFrameHeader(e));
                    reader.reset();
                    break;
                }
            }
        }
    }
}

fn dispatch_frame(frame: &rcard_usb_proto::RawFrame<'_>, sink: &LogSink) {
    let Some(simple) = frame.as_simple() else {
        return;
    };

    if simple.opcode == rcard_usb_proto::messages::log_entry::OP_LOG_ENTRY {
        parse_log_entries(simple.payload, sink);
    }
    // Future event types dispatched here.
}

/// Parse batched log entries from a `consume_since` wire dump.
///
/// Each entry is `[log_id:8][data_len:1][fragment_idx:1][data:data_len]`.
/// Fragment 0 contains `LogMetadata` followed by Format-encoded argument
/// data. For single-shot logs (the common case), all data is in one
/// fragment — we decode it inline. Multi-fragment reassembly is deferred.
fn parse_log_entries(data: &[u8], sink: &LogSink) {
    let mut offset = 0;

    while offset + ENTRY_HEADER <= data.len() {
        let data_len = data[offset + 8] as usize;
        let idx = data[offset + 9];

        if offset + ENTRY_HEADER + data_len > data.len() {
            break;
        }

        let payload = &data[offset + ENTRY_HEADER..offset + ENTRY_HEADER + data_len];

        if idx == 0 && payload.len() >= METADATA_SIZE {
            match LogMetadata::try_read_from_bytes(&payload[..METADATA_SIZE]) {
                Ok(meta) => {
                    let arg_data = &payload[METADATA_SIZE..];
                    let values = decode_values(arg_data, sink);

                    sink.structured(LogEntry {
                        level: meta.level,
                        timestamp: meta.timestamp,
                        source: meta.source,
                        log_id: meta.log_id,
                        log_species: meta.log_species,
                        values,
                    });
                }
                Err(_) => {
                    sink.error(UsbError::LogMetadata);
                }
            }
        }
        // idx > 0: continuation fragment for streamed logs.
        // Multi-fragment reassembly deferred — most logs are single-shot.

        offset += ENTRY_HEADER + data_len;
    }
}

/// Decode Format-encoded argument values from a byte slice.
fn decode_values(data: &[u8], sink: &LogSink) -> Vec<rcard_log::OwnedValue> {
    let mut decoder = Decoder::new();
    let mut values = Vec::new();
    let mut remaining = data;

    loop {
        if remaining.is_empty() {
            break;
        }
        let (consumed, result) = decoder.feed(remaining);
        remaining = &remaining[consumed..];
        match result {
            FeedResult::Done(value) => {
                values.push(value);
            }
            FeedResult::EndOfStream => break,
            FeedResult::Incomplete => {
                if consumed == 0 {
                    break;
                }
            }
            FeedResult::Error(_) => {
                sink.error(UsbError::LogDecode);
                if consumed == 0 {
                    break;
                }
            }
        }
    }

    values
}

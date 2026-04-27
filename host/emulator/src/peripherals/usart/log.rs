use rcard_log::{LogMetadata, OwnedValue};

#[derive(Debug)]
pub struct UsartLog {
    pub channel: u8,
    pub kind: UsartLogKind,
}

pub enum UsartLogKind {
    Line(String),
    Stream(LogStream),
    Renode(String),
    Display(Vec<u8>),
}

pub struct LogStream {
    pub metadata: LogMetadata,
    pub values: Vec<OwnedValue>,
    /// True iff the host evicted this stream on a timeout instead of
    /// receiving a `TAG_END_OF_STREAM` terminator.
    pub truncated: bool,
}

impl core::fmt::Debug for UsartLogKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            UsartLogKind::Line(s) => f.debug_tuple("Line").field(s).finish(),
            UsartLogKind::Stream(s) => f
                .debug_struct("Stream")
                .field("metadata", &s.metadata)
                .finish_non_exhaustive(),
            UsartLogKind::Renode(s) => f.debug_tuple("Renode").field(s).finish(),
            UsartLogKind::Display(d) => f.debug_tuple("Display").field(&d.len()).finish(),
        }
    }
}

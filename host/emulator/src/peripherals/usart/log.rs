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
}

pub struct LogStream {
    pub metadata: LogMetadata,
    pub values: Vec<OwnedValue>,
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
        }
    }
}

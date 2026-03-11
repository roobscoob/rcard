use ipc::allocation;

allocation!(FRAME_BUFFERS = @frame_buffers: [[u8; 8192]; 64]);

pub enum FrameBufferContents {}

pub struct FrameBuffer {}

use crate::allocator::Allocator;
use ipc::allocation;
use rcard_log::OptionExt;

pub const CHUNK_SIZE: usize = 8192;
const CHUNK_COUNT: usize = 64;

allocation!(FRAME_BUFFERS = @frame_buffers: [u8; CHUNK_SIZE * CHUNK_COUNT]);

pub struct FrameBuffers {
    allocator: Allocator<'static, CHUNK_SIZE, CHUNK_COUNT>,
    next_id: u32,
}

impl FrameBuffers {
    pub fn take() -> Self {
        let buf = FRAME_BUFFERS
            .get()
            .log_expect("frame buffer allocation already taken");
        Self {
            allocator: Allocator::new(buf),
            next_id: 1,
        }
    }

    pub fn next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    pub fn allocator_mut(&mut self) -> &mut Allocator<'static, CHUNK_SIZE, CHUNK_COUNT> {
        &mut self.allocator
    }

    pub fn free(&mut self, id: u32) {
        self.allocator.free(id);
    }
}

use core::ops::Deref;

#[repr(C)]
#[derive(Debug)]
pub struct SuspensionBuffer {
    data: [u8; 256],
    length: u16,
}

impl SuspensionBuffer {
    pub fn ok(data: &[u8]) -> Result<Self, ()> {
        if data.len() > 256 {
            return Err(());
        }

        Ok(Self {
            data: {
                let mut buffer = [0; 256];
                buffer[..data.len()].copy_from_slice(data);
                buffer
            },
            length: data.len() as u16,
        })
    }
}

impl Deref for SuspensionBuffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.data[..self.length as usize]
    }
}

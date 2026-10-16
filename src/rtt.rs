use core::{
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::consts::BUF_SIZE;

#[repr(C)]
pub struct Header {
    id: [u8; 16],
    max_up_channels: usize,
    max_down_channels: usize,
    #[cfg(feature = "execution-trace")]
    up_channels: [Channel; 2],
    #[cfg(not(feature = "execution-trace"))]
    up_channel: Channel,
}

const MODE_MASK: usize = 0b11;
const MODE_BLOCK_IF_FULL: usize = 2;
const MODE_NON_BLOCKING_TRIM: usize = 1;

#[cfg(not(feature = "execution-trace"))]
#[used]
#[no_mangle]
pub static mut _SEGGER_RTT: Header = Header {
    id: *b"SEGGER RTT\0\0\0\0\0\0",
    max_up_channels: if cfg!(feature = "execution-trace") {
        2
    } else {
        1
    },
    max_down_channels: 0,
    #[cfg(feature = "execution-trace")]
    up_channels: [uninit_channel(), uninit_channel()],
    #[cfg(not(feature = "execution-trace"))]
    up_channel: Channel {
        name: &NAME as *const _ as *const u8,
        buffer: ptr::addr_of_mut!(BUFFER) as *mut u8,
        size: BUF_SIZE,
        write: AtomicUsize::new(0),
        read: AtomicUsize::new(0),
        flags: AtomicUsize::new(MODE_NON_BLOCKING_TRIM),
    },
};

#[cfg(feature = "execution-trace")]
#[cfg_attr(target_arch = "arm", link_section = ".execution_trace.rtt_cb")]
#[used]
#[no_mangle]
pub static mut _SEGGER_RTT: core::mem::MaybeUninit<Header> = core::mem::MaybeUninit::uninit();

#[cfg_attr(
    all(feature = "execution-trace", target_arch = "arm"),
    link_section = ".execution_trace.defmt_buffer"
)]
#[cfg_attr(
    all(not(feature = "execution-trace"), target_os = "macos"),
    link_section = ".uninit,defmt-rtt.BUFFER"
)]
#[cfg_attr(
    all(not(feature = "execution-trace"), not(target_os = "macos")),
    link_section = ".uninit.defmt-rtt.BUFFER"
)]
static mut BUFFER: [u8; BUF_SIZE] = [0; BUF_SIZE];

#[cfg_attr(not(feature = "execution-trace"), link_section = ".data")]
#[cfg_attr(
    all(feature = "execution-trace", target_arch = "arm"),
    link_section = ".execution_trace.defmt_name"
)]
#[cfg(feature = "execution-trace")]
static mut NAME: [u8; 6] = *b"defmt\0";
#[cfg(not(feature = "execution-trace"))]
#[link_section = ".data"]
static NAME: [u8; 6] = *b"defmt\0";

#[cfg(feature = "execution-trace")]
#[cfg_attr(target_arch = "arm", link_section = ".execution_trace.trace_name")]
static mut TRACE_NAME: [u8; 6] = *b"trace\0";

#[cfg(feature = "execution-trace")]
static TRACE_READY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Initialize the cache-incoherent RTT objects in their linker-reserved,
/// non-cacheable region, with `trace_buffer` as up-channel 1. Called once before
/// defmt is started.
///
/// # Safety
/// `trace_buffer` must be valid for writes of `trace_size` bytes for the life
/// of the program and must not be accessed by anything except the trace writer
/// and the debug host.
#[cfg(feature = "execution-trace")]
pub unsafe fn init_trace_channels(trace_buffer: *mut u8, trace_size: usize) {
    if TRACE_READY.load(Ordering::Acquire) {
        return;
    }
    unsafe {
        ptr::write_bytes(ptr::addr_of_mut!(BUFFER).cast::<u8>(), 0, BUF_SIZE);
        ptr::write_bytes(trace_buffer, 0, trace_size);
        ptr::copy_nonoverlapping(b"defmt\0".as_ptr(), ptr::addr_of_mut!(NAME).cast::<u8>(), 6);
        ptr::copy_nonoverlapping(
            b"trace\0".as_ptr(),
            ptr::addr_of_mut!(TRACE_NAME).cast::<u8>(),
            6,
        );
        let header = Header {
            id: *b"SEGGER RTT\0\0\0\0\0\0",
            max_up_channels: 2,
            max_down_channels: 0,
            up_channels: [
                Channel {
                    name: ptr::addr_of!(NAME).cast::<u8>(),
                    buffer: ptr::addr_of_mut!(BUFFER).cast::<u8>(),
                    size: BUF_SIZE,
                    write: AtomicUsize::new(0),
                    read: AtomicUsize::new(0),
                    flags: AtomicUsize::new(MODE_NON_BLOCKING_TRIM),
                },
                Channel {
                    name: ptr::addr_of!(TRACE_NAME).cast::<u8>(),
                    buffer: trace_buffer,
                    size: trace_size,
                    write: AtomicUsize::new(0),
                    read: AtomicUsize::new(0),
                    flags: AtomicUsize::new(MODE_NON_BLOCKING_TRIM),
                },
            ],
        };
        ptr::addr_of_mut!(_SEGGER_RTT).write(core::mem::MaybeUninit::new(header));
    }
    TRACE_READY.store(true, Ordering::Release);
}

// Safety: the defmt logger serializes channel 0 with its existing critical
// section. Callers of `try_write_trace` serialize channel 1 themselves.
pub(crate) unsafe fn handle() -> &'static Channel {
    #[cfg(feature = "execution-trace")]
    {
        &(*ptr::addr_of!(_SEGGER_RTT).cast::<Header>()).up_channels[0]
    }
    #[cfg(not(feature = "execution-trace"))]
    {
        &(*ptr::addr_of!(_SEGGER_RTT)).up_channel
    }
}

pub(crate) fn do_write(bytes: &[u8]) {
    unsafe { handle().write_all(bytes) }
}

/// Publish `bytes` to up-channel 1 in full, or return false without moving the
/// write index. Never blocks. Callers must serialize writes (e.g. under a
/// critical section).
#[cfg(feature = "execution-trace")]
pub fn try_write_trace(bytes: &[u8]) -> bool {
    if !TRACE_READY.load(Ordering::Acquire) {
        return false;
    }
    let channel = unsafe { &(*ptr::addr_of!(_SEGGER_RTT).cast::<Header>()).up_channels[1] };
    let read = channel.read.load(Ordering::Acquire);
    unsafe { try_write_full_record(channel.buffer, channel.size, read, &channel.write, bytes) }
}

#[cfg(feature = "execution-trace")]
fn trace_available(read: usize, write: usize, capacity: usize) -> usize {
    if read > write {
        read - write - 1
    } else {
        capacity - write + read - 1
    }
}

#[cfg(feature = "execution-trace")]
pub unsafe fn try_write_full_record(
    buffer: *mut u8,
    capacity: usize,
    read: usize,
    write_index: &AtomicUsize,
    record: &[u8],
) -> bool {
    let write = write_index.load(Ordering::Relaxed);
    if trace_available(read, write, capacity) < record.len() {
        return false;
    }
    let first = record.len().min(capacity - write);
    ptr::copy_nonoverlapping(record.as_ptr(), buffer.add(write), first);
    if first < record.len() {
        ptr::copy_nonoverlapping(record.as_ptr().add(first), buffer, record.len() - first);
    }
    // Single publish after all bytes are copied.
    write_index.store((write + record.len()) % capacity, Ordering::Release);
    true
}

#[repr(C)]
pub(crate) struct Channel {
    pub name: *const u8,
    pub buffer: *mut u8,
    pub size: usize,
    pub write: AtomicUsize,
    pub read: AtomicUsize,
    pub flags: AtomicUsize,
}

impl Channel {
    pub fn write_all(&self, mut bytes: &[u8]) {
        let write = match self.host_is_connected() {
            true => Self::blocking_write,
            false => Self::nonblocking_write,
        };
        while !bytes.is_empty() {
            let consumed = write(self, bytes);
            if consumed != 0 {
                bytes = &bytes[consumed..];
            }
        }
    }

    fn blocking_write(&self, bytes: &[u8]) -> usize {
        if bytes.is_empty() {
            return 0;
        }
        let read = self.read.load(Ordering::Relaxed);
        let write = self.write.load(Ordering::Acquire);
        let available = available_buffer_size(read, write, BUF_SIZE);
        if available == 0 {
            return 0;
        }
        self.write_impl(bytes, write, available)
    }

    fn nonblocking_write(&self, bytes: &[u8]) -> usize {
        let write = self.write.load(Ordering::Acquire);
        self.write_impl(bytes, write, BUF_SIZE)
    }

    fn write_impl(&self, bytes: &[u8], cursor: usize, available: usize) -> usize {
        let len = bytes.len().min(available);
        unsafe {
            if cursor + len > BUF_SIZE {
                let pivot = BUF_SIZE - cursor;
                ptr::copy_nonoverlapping(bytes.as_ptr(), self.buffer.add(cursor), pivot);
                ptr::copy_nonoverlapping(bytes.as_ptr().add(pivot), self.buffer, len - pivot);
            } else {
                ptr::copy_nonoverlapping(bytes.as_ptr(), self.buffer.add(cursor), len);
            }
        }
        self.write
            .store(cursor.wrapping_add(len) % BUF_SIZE, Ordering::Release);
        len
    }

    pub fn flush(&self) {
        if !self.host_is_connected() {
            return;
        }
        while self.read.load(Ordering::Relaxed) != self.write.load(Ordering::Relaxed) {}
    }

    fn host_is_connected(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & MODE_MASK == MODE_BLOCK_IF_FULL
    }
}

fn available_buffer_size(read_cursor: usize, write_cursor: usize, capacity: usize) -> usize {
    if read_cursor > write_cursor {
        read_cursor - write_cursor - 1
    } else if read_cursor == 0 {
        capacity - write_cursor - 1
    } else {
        capacity - write_cursor
    }
}

#[cfg(all(test, feature = "execution-trace"))]
mod tests {
    use super::*;

    const TRACE_BUF_SIZE: usize = 2 * 1024;

    #[test]
    fn trace_record_retries_atomically_then_wraps() {
        let mut ring = [0xAA; 64];
        let write = AtomicUsize::new(40);
        let record = [0x5A; 32];
        assert!(!unsafe { try_write_full_record(ring.as_mut_ptr(), 64, 0, &write, &record) });
        assert_eq!(write.load(Ordering::Relaxed), 40);
        assert!(ring.iter().all(|byte| *byte == 0xAA));

        assert!(unsafe { try_write_full_record(ring.as_mut_ptr(), 64, 9, &write, &record) });
        assert_eq!(write.load(Ordering::Acquire), 8);
        assert!(ring[40..].iter().all(|byte| *byte == 0x5A));
        assert!(ring[..8].iter().all(|byte| *byte == 0x5A));
    }

    #[test]
    fn multi_record_block_is_all_or_nothing() {
        let mut ring = [0u8; 128];
        let write = AtomicUsize::new(64);
        // 63 bytes free: a 64-byte block is refused, a 48-byte one fits.
        assert!(!unsafe { try_write_full_record(ring.as_mut_ptr(), 128, 0, &write, &[0x11; 64]) });
        assert_eq!(write.load(Ordering::Relaxed), 64);
        assert!(ring.iter().all(|byte| *byte == 0));
        assert!(unsafe { try_write_full_record(ring.as_mut_ptr(), 128, 0, &write, &[0x22; 48]) });
        assert_eq!(write.load(Ordering::Relaxed), 112);
    }

    #[test]
    fn ring_2048_holds_63_records_64th_refuses_without_mutation() {
        let mut ring = [0u8; TRACE_BUF_SIZE];
        let write = AtomicUsize::new(0);
        for seq in 0..63u32 {
            let mut record = [0u8; 32];
            record[4..8].copy_from_slice(&seq.to_le_bytes());
            assert!(unsafe {
                try_write_full_record(ring.as_mut_ptr(), TRACE_BUF_SIZE, 0, &write, &record)
            });
        }
        assert_eq!(write.load(Ordering::Relaxed), 63 * 32);

        let ring_before = ring;
        let write_before = write.load(Ordering::Relaxed);
        assert!(!unsafe {
            try_write_full_record(ring.as_mut_ptr(), TRACE_BUF_SIZE, 0, &write, &[0xEE; 32])
        });
        assert_eq!(write.load(Ordering::Relaxed), write_before);
        assert_eq!(ring, ring_before);
    }

    #[test]
    fn ack_retry_wrap_preserves_every_byte() {
        let mut ring = [0u8; TRACE_BUF_SIZE];
        let write = AtomicUsize::new(0);
        let mut read = 0usize;
        let mut written = 0u32;
        let mut acked = 0u32;
        for _ in 0..66u32 {
            let mut record = [0u8; 32];
            record[4..8].copy_from_slice(&written.to_le_bytes());
            loop {
                if unsafe {
                    try_write_full_record(ring.as_mut_ptr(), TRACE_BUF_SIZE, read, &write, &record)
                } {
                    break;
                }
                // The simulated host acknowledges the oldest buffered record,
                // verifying all 32 bytes survive buffering and the wrap.
                ack_oldest_record(&ring, &mut read, &mut acked);
            }
            written += 1;
        }
        while read != write.load(Ordering::Acquire) {
            ack_oldest_record(&ring, &mut read, &mut acked);
        }
        assert_eq!(written, 66);
        assert_eq!(acked, 66);
    }

    /// Verifies the whole oldest buffered record byte for byte, then advances
    /// the host read cursor past it.
    fn ack_oldest_record(ring: &[u8; TRACE_BUF_SIZE], read: &mut usize, acked: &mut u32) {
        let mut expected = [0u8; 32];
        expected[4..8].copy_from_slice(&acked.to_le_bytes());
        for offset in 0..32 {
            assert_eq!(
                ring[(*read + offset) % TRACE_BUF_SIZE],
                expected[offset],
                "record {} byte {} at cursor {}",
                acked,
                offset,
                read
            );
        }
        *read = (*read + 32) % TRACE_BUF_SIZE;
        *acked += 1;
    }
}

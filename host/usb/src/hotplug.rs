//! rcard-fob hotplug stream — wraps `nusb::watch_devices` and filters
//! its event stream down to events for our VID/PID.
//!
//! The stream also self-seeds: it calls `list_devices` synchronously
//! at construction and emits a `Connected` event for each matching
//! fob before returning. Any device that attaches between the
//! `watch_devices` subscription and the seed enumeration will show up
//! in both — callers must dedupe by `DeviceId` (or `serial`).

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::Stream;
use nusb::MaybeFuture;
use nusb::hotplug::{HotplugEvent, HotplugWatch};

use crate::{FobInfo, USB_PID, USB_VID};

/// Events surfaced by [`watch_fobs`].
#[derive(Debug, Clone)]
pub enum FobEvent {
    Connected(FobInfo),
    Disconnected(nusb::DeviceId),
}

/// Stream of rcard fob attach / detach events.
pub struct FobWatch {
    inner: HotplugWatch,
    /// Events produced by the initial `list_devices` seed that haven't
    /// been drained yet.
    seed: VecDeque<FobEvent>,
}

/// Subscribe to USB hotplug events, filtered to rcard fobs. The stream
/// is seeded with `Connected` events for every matching device already
/// attached at subscription time.
pub fn watch_fobs() -> Result<FobWatch, nusb::Error> {
    // Subscribe first, then enumerate. This ordering means any device
    // attached between the two calls appears in both — callers dedupe
    // by DeviceId. The reverse ordering could miss a device entirely.
    let inner = nusb::watch_devices()?;

    let mut seed = VecDeque::new();
    if let Ok(iter) = nusb::list_devices().wait() {
        for d in iter {
            if d.vendor_id() != USB_VID || d.product_id() != USB_PID {
                continue;
            }
            let Some(serial) = d.serial_number() else {
                continue;
            };
            seed.push_back(FobEvent::Connected(FobInfo {
                serial: serial.to_string(),
                id: d.id(),
            }));
        }
    }

    Ok(FobWatch { inner, seed })
}

impl Stream for FobWatch {
    type Item = FobEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(event) = self.seed.pop_front() {
            return Poll::Ready(Some(event));
        }

        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(HotplugEvent::Connected(info))) => {
                    if info.vendor_id() != USB_VID || info.product_id() != USB_PID {
                        continue;
                    }
                    let Some(serial) = info.serial_number() else {
                        continue;
                    };
                    return Poll::Ready(Some(FobEvent::Connected(FobInfo {
                        serial: serial.to_string(),
                        id: info.id(),
                    })));
                }
                Poll::Ready(Some(HotplugEvent::Disconnected(id))) => {
                    // We can't VID/PID-filter here — the descriptor is
                    // already gone. Pass every id through; the caller's
                    // map lookup is the filter.
                    return Poll::Ready(Some(FobEvent::Disconnected(id)));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

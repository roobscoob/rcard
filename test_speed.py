import time
import usb.core
import usb.util
import usb.backend.libusb1
import libusb_package

backend = usb.backend.libusb1.get_backend(find_library=libusb_package.find_library)
dev = usb.core.find(idVendor=0x16D0, idProduct=0x14EF, backend=backend)
if dev is None:
    print("Device not found")
    exit(1)

print(f"Found: VID={dev.idVendor:04x} PID={dev.idProduct:04x}")
dev.set_configuration()

EP_OUT = 0x02
EP_IN = 0x85
PKT_SIZE = 64

# Warm up
dev.write(EP_OUT, b"\x00" * PKT_SIZE)
dev.read(EP_IN, PKT_SIZE, timeout=2000)

# Round-trip latency test
ITERS = 1000
payload = bytes(range(PKT_SIZE))

t0 = time.perf_counter()
for _ in range(ITERS):
    dev.write(EP_OUT, payload)
    dev.read(EP_IN, PKT_SIZE, timeout=2000)
t1 = time.perf_counter()

elapsed = t1 - t0
rtt_us = elapsed / ITERS * 1_000_000
total_bytes = ITERS * PKT_SIZE * 2  # out + in
throughput_kbps = total_bytes / elapsed / 1024

print(f"\n--- Round-trip (64B echo x {ITERS}) ---")
print(f"Total: {elapsed:.2f}s")
print(f"RTT:   {rtt_us:.0f} us/round-trip")
print(f"Throughput: {throughput_kbps:.0f} KB/s (bidirectional)")

# Bulk write throughput test
BULK_SIZE = 64
BULK_ITERS = 5000

t0 = time.perf_counter()
for _ in range(BULK_ITERS):
    dev.write(EP_OUT, payload)
    dev.read(EP_IN, PKT_SIZE, timeout=2000)
t1 = time.perf_counter()

elapsed = t1 - t0
total_bytes = BULK_ITERS * PKT_SIZE
throughput_kbps = total_bytes / elapsed / 1024

print(f"\n--- Sustained echo (64B x {BULK_ITERS}) ---")
print(f"Total: {elapsed:.2f}s")
print(f"OUT throughput: {throughput_kbps:.0f} KB/s")

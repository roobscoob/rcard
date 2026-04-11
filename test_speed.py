import argparse
import time
import usb.core
import usb.util
import usb.backend.libusb1
import libusb_package

parser = argparse.ArgumentParser()
parser.add_argument(
    "--pattern",
    choices=["counter", "55", "aa", "alternating", "all"],
    default="counter",
    help=(
        "counter=0x00..0x3f; 55=all 0x55; aa=all 0xaa; alternating=0x55/0xaa; "
        "all=run each pattern in sequence"
    ),
)
parser.add_argument("--iters", type=int, default=1000)
parser.add_argument("--bulk-iters", type=int, default=5000)
parser.add_argument(
    "--size-sweep",
    action="store_true",
    help=(
        "Run a packet-size sweep with the counter pattern to test the "
        "FIFO-size hypothesis. Sizes tried: 1, 4, 8, 16, 17, 24, 32, 48, 64."
    ),
)
parser.add_argument(
    "--size-sweep-small",
    action="store_true",
    help=(
        "Fine-grained sweep at small sizes (1..8) with many more iterations "
        "to resolve very low error rates."
    ),
)
parser.add_argument(
    "--size-sweep-8aligned",
    action="store_true",
    help=(
        "8-aligned sizes (8, 16, ..., 64) with 50k iters to test clean "
        "8-byte stride hypothesis."
    ),
)
parser.add_argument(
    "--delay-sweep",
    action="store_true",
    help=(
        "Delay sweep: run a 64-byte counter echo at several inter-iteration "
        "delays (0, 0.1, 1, 10 ms) to test the CPU/peripheral race theory. "
        "If errors trend toward zero with delay, it's a timing race."
    ),
)
parser.add_argument(
    "--size-sweep-4align",
    action="store_true",
    help=(
        "Sizes 8..16 to test the 4-byte-alignment-of-size hypothesis. "
        "If sizes that are multiples of 4 (8, 12, 16) have markedly fewer "
        "errors than neighbors, the bug is in partial-word end-of-packet handling."
    ),
)
parser.add_argument(
    "--soak",
    action="store_true",
    help=(
        "Continuous 64-byte echo loop. Reports errors per 10k-packet window "
        "until Ctrl-C. Use to characterize the accumulated-state bug."
    ),
)
parser.add_argument(
    "--soak-size",
    type=int,
    default=64,
    help="Packet size for --soak (default 64).",
)
parser.add_argument(
    "--soak-window",
    type=int,
    default=10000,
    help="Window size (packets) for --soak reporting (default 10000).",
)
parser.add_argument(
    "--rx-only",
    action="store_true",
    help=(
        "RX-only test: send 64-byte counter packets to OUT indefinitely. "
        "Do not read IN. Stub (with MODE=RxVerify) verifies each packet "
        "internally and logs mismatches. Watch device USART for `stub rx BAD` lines."
    ),
)
parser.add_argument(
    "--tx-only",
    action="store_true",
    help=(
        "TX-only test: read 64-byte packets from IN indefinitely and verify "
        "against the counter pattern. Do not write OUT. Stub must be running "
        "with MODE=TxGenerate."
    ),
)
parser.add_argument(
    "--iters-direction",
    type=int,
    default=200000,
    help="Iteration count for --rx-only / --tx-only (default 200k).",
)
args = parser.parse_args()

backend = usb.backend.libusb1.get_backend(find_library=libusb_package.find_library)
dev = usb.core.find(idVendor=0x16D0, idProduct=0x14EF, backend=backend)
if dev is None:
    print("Device not found")
    exit(1)

print(f"Found: VID={dev.idVendor:04x} PID={dev.idProduct:04x}")
dev.set_configuration()

# Stub opens EP2 OUT (0x02) and EP1 IN (0x81).
EP_OUT = 0x02
EP_IN = 0x81
PKT_SIZE = 64


def make_payload(name: str, iteration: int = 0) -> bytes:
    if name == "counter":
        return bytes(range(PKT_SIZE))
    if name == "55":
        return b"\x55" * PKT_SIZE
    if name == "aa":
        return b"\xaa" * PKT_SIZE
    if name == "alternating":
        return b"\x55\xaa" * (PKT_SIZE // 2)
    if name == "varying":
        return bytes((iteration + j) & 0xFF for j in range(PKT_SIZE))
    raise ValueError(f"unknown pattern {name}")


def check_echo(sent: bytes, got: bytes, label: str) -> bool:
    if len(got) != len(sent):
        print(f"  [{label}] length mismatch: sent {len(sent)}, got {len(got)}")
        return False
    if got != sent:
        for i, (a, b) in enumerate(zip(sent, got)):
            if a != b:
                window = 8
                lo = max(0, i - window)
                hi = min(len(sent), i + window)
                print(
                    f"  [{label}] mismatch at byte {i}: "
                    f"sent {sent[lo:hi].hex()} got {got[lo:hi].hex()}"
                )
                break
        return False
    return True


def run_pattern(pattern: str) -> tuple[int, int]:
    """Return (errors, total)."""
    print(f"\n=== Pattern: {pattern} ===")

    # Warm up.
    warm = make_payload(pattern, 0)
    dev.write(EP_OUT, warm)
    dev.read(EP_IN, PKT_SIZE, timeout=2000)

    # Round-trip.
    errors = 0
    t0 = time.perf_counter()
    for i in range(args.iters):
        payload = make_payload(pattern, i)
        dev.write(EP_OUT, payload)
        got = bytes(dev.read(EP_IN, PKT_SIZE, timeout=2000))
        if not check_echo(payload, got, f"{pattern} rtt {i}"):
            errors += 1
            if errors >= 20:
                print("  (further rtt errors suppressed)")
                break
    t1 = time.perf_counter()
    elapsed = t1 - t0
    rtt_us = elapsed / max(args.iters, 1) * 1_000_000
    print(
        f"rtt: {elapsed:.2f}s, {rtt_us:.0f} us/iter, errors {errors}/{args.iters}"
    )
    total_errors = errors

    # Bulk.
    errors = 0
    t0 = time.perf_counter()
    for i in range(args.bulk_iters):
        payload = make_payload(pattern, i)
        dev.write(EP_OUT, payload)
        got = bytes(dev.read(EP_IN, PKT_SIZE, timeout=2000))
        if not check_echo(payload, got, f"{pattern} bulk {i}"):
            errors += 1
            if errors >= 20:
                print("  (further bulk errors suppressed)")
                break
    t1 = time.perf_counter()
    elapsed = t1 - t0
    total_bytes = args.bulk_iters * PKT_SIZE
    throughput_kbps = total_bytes / elapsed / 1024
    print(
        f"bulk: {elapsed:.2f}s, {throughput_kbps:.0f} KB/s, errors {errors}/{args.bulk_iters}"
    )
    total_errors += errors

    return total_errors, args.iters + args.bulk_iters


def sweep(sizes, iters, label):
    print(f"\n=== {label} ({iters} iters each) ===")
    print(f"{'size':>5}  {'errors':>10}  {'error%':>8}")
    for sz in sizes:
        payload = bytes(range(sz))
        # Warm up at this size.
        dev.write(EP_OUT, payload)
        dev.read(EP_IN, 64, timeout=2000)
        errors = 0
        for _ in range(iters):
            dev.write(EP_OUT, payload)
            got = bytes(dev.read(EP_IN, 64, timeout=2000))
            if got != payload:
                errors += 1
        pct = 100.0 * errors / iters
        print(f"{sz:>5}  {errors:>10}  {pct:>7.3f}%")


def run_size_sweep():
    """Probe the FIFO-size hypothesis. For each packet size, send a counter
    pattern trimmed to that size and count echo mismatches. If the TX or
    RX FIFO for our endpoints is N bytes wide, we expect 0 errors at sizes
    ≤ N and non-zero errors at sizes > N.
    """
    sweep([1, 4, 8, 16, 17, 24, 32, 48, 64], 20000, "Size sweep")


def run_size_sweep_small():
    """Fine-grained sweep at small sizes. Many iterations so we can
    distinguish true zero-error from a very low rate."""
    sweep([1, 2, 3, 4, 5, 6, 7, 8], 100000, "Small size sweep")


def run_size_sweep_4align():
    """Test the 4-byte-alignment hypothesis: if size-%-4-aligned packets
    are cleaner, sizes 12 and 16 should show markedly fewer errors than
    9, 10, 11, 13, 14, 15."""
    sweep([8, 9, 10, 11, 12, 13, 14, 15, 16], 100000, "4-alignment size sweep")


def run_rx_only(iters: int):
    """Send OUT packets continuously; stub verifies internally. The host
    does not read IN (stub in RxVerify mode never writes to it). Mismatches
    show up in the stub's USART log as `stub rx BAD ...` lines."""
    payload = bytes(range(64))
    print(f"\n=== RX-only test ({iters} writes) ===")
    print("(stub must be running with MODE=RxVerify)")
    print("(check device USART log for `stub rx BAD` lines)")
    t0 = time.perf_counter()
    for i in range(iters):
        try:
            dev.write(EP_OUT, payload)
        except usb.core.USBError as e:
            print(f"[iter {i}] USBError: {e}")
            return
        if i > 0 and i % 10000 == 0:
            print(f"  sent {i}")
    t1 = time.perf_counter()
    print(f"done: {iters} writes in {t1 - t0:.1f}s")


def run_tx_only(iters: int):
    """Read IN packets continuously; verify against counter pattern. Stub
    (in TxGenerate mode) pushes the counter pattern into the TX FIFO as
    fast as the peripheral accepts it."""
    expected = bytes(range(64))
    print(f"\n=== TX-only test ({iters} reads) ===")
    print("(stub must be running with MODE=TxGenerate)")
    errors = 0
    t0 = time.perf_counter()
    for i in range(iters):
        try:
            got = bytes(dev.read(EP_IN, 64, timeout=2000))
        except usb.core.USBError as e:
            print(f"[iter {i}] USBError: {e}")
            break
        if got != expected:
            errors += 1
            if errors <= 10:
                for j, (a, b) in enumerate(zip(expected, got)):
                    if a != b:
                        print(
                            f"  [iter {i}] first diff at byte {j}: "
                            f"expected {a:02x} got {b:02x}"
                        )
                        break
            elif errors == 11:
                print("  (further diffs suppressed)")
    t1 = time.perf_counter()
    print(f"done: {iters} reads in {t1 - t0:.1f}s, errors {errors}")


def run_soak(sz: int, window: int):
    """Soak test: run continuously at a fixed packet size, report error
    rate per `window` packets, until Ctrl-C or a pipe error. The goal is
    to see whether errors stay flat, drift upward, or cliff over time.
    """
    payload = bytes((i & 0xFF) for i in range(sz))
    # Warm up once.
    dev.write(EP_OUT, payload)
    dev.read(EP_IN, 64, timeout=2000)

    print(f"\n=== Soak test (size={sz}, window={window}, Ctrl-C to stop) ===")
    print(f"{'window':>8}  {'packets':>12}  {'errors':>10}  {'error%':>8}  {'total_err':>10}  {'total_err%':>11}")

    total_packets = 0
    total_errors = 0
    win_idx = 0
    t_start = time.perf_counter()
    try:
        while True:
            win_errors = 0
            for _ in range(window):
                try:
                    dev.write(EP_OUT, payload)
                    got = bytes(dev.read(EP_IN, 64, timeout=2000))
                except usb.core.USBError as e:
                    print(f"\n[window {win_idx}, packet {total_packets}] USBError: {e}")
                    raise KeyboardInterrupt
                if got != payload:
                    win_errors += 1
                total_packets += 1
            total_errors += win_errors
            win_pct = 100.0 * win_errors / window
            tot_pct = 100.0 * total_errors / total_packets
            print(
                f"{win_idx:>8}  {total_packets:>12}  {win_errors:>10}  "
                f"{win_pct:>7.3f}%  {total_errors:>10}  {tot_pct:>10.3f}%"
            )
            win_idx += 1
    except KeyboardInterrupt:
        elapsed = time.perf_counter() - t_start
        rate = total_packets / elapsed if elapsed > 0 else 0
        print(
            f"\nSoak stopped: {total_packets} packets in {elapsed:.1f}s "
            f"({rate:.0f}/s), {total_errors} errors total "
            f"({100.0 * total_errors / max(total_packets, 1):.3f}%)"
        )


def run_size_sweep_8aligned():
    """8-aligned sweep to test whether corruption has a clean 8-byte
    granularity. Each step adds exactly one 8-byte block."""
    sweep([8, 16, 24, 32, 40, 48, 56, 64], 50000, "8-aligned size sweep")


def run_delay_sweep():
    """Hold packet size fixed at 64 bytes, vary the inter-iteration delay,
    measure error rate. If errors drop with delay, CPU/peripheral is racing."""
    iters = 20000
    delays_ms = [0, 0.1, 0.5, 1, 5, 10]
    payload = bytes(range(64))
    print(f"\n=== Delay sweep (64-byte counter, {iters} iters each) ===")
    print(f"{'delay (ms)':>10}  {'errors':>10}  {'error%':>8}")
    for delay_ms in delays_ms:
        # Warm up.
        dev.write(EP_OUT, payload)
        dev.read(EP_IN, 64, timeout=2000)
        errors = 0
        sleep_s = delay_ms / 1000.0
        for _ in range(iters):
            dev.write(EP_OUT, payload)
            got = bytes(dev.read(EP_IN, 64, timeout=2000))
            if got != payload:
                errors += 1
            if sleep_s > 0:
                time.sleep(sleep_s)
        pct = 100.0 * errors / iters
        print(f"{delay_ms:>10.1f}  {errors:>10}  {pct:>7.2f}%")


# ── Run ──────────────────────────────────────────────────────────────
if args.size_sweep:
    run_size_sweep()
    exit(0)

if args.size_sweep_small:
    run_size_sweep_small()
    exit(0)

if args.size_sweep_8aligned:
    run_size_sweep_8aligned()
    exit(0)

if args.delay_sweep:
    run_delay_sweep()
    exit(0)

if args.size_sweep_4align:
    run_size_sweep_4align()
    exit(0)

if args.soak:
    run_soak(args.soak_size, args.soak_window)
    exit(0)

if args.rx_only:
    run_rx_only(args.iters_direction)
    exit(0)

if args.tx_only:
    run_tx_only(args.iters_direction)
    exit(0)

if args.pattern == "all":
    patterns = ["counter", "55", "aa", "alternating"]
else:
    patterns = [args.pattern]

grand_errors = 0
grand_total = 0
for p in patterns:
    e, t = run_pattern(p)
    grand_errors += e
    grand_total += t

print(f"\n=== Summary: {grand_errors} errors / {grand_total} iterations ===")
if grand_errors == 0:
    print("PASS")
else:
    print("FAIL")
    exit(1)

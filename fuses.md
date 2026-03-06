# SF32LB52x eFuse Layout

Base address: `0x5000_C000` (EFUSEC)

4 banks × 256 bits = 1024 bits total. Default value: all zeros.
Bits can only be burned 0→1 (irreversible). Banks can be read/write masked via Bank 0.

## Bank 0 — Config & Identity (bits 0–255)

| Bits | Field | Size | Notes |
|------|-------|------|-------|
| 0–127 | `uid[127:0]` | 16 bytes | Chip ID. Used as AES-CBC IV/nonce. Hardware output signal. |
| 128–191 | `sig_hash` | 8 bytes | RSA public key hash (secure boot). |
| 192–223 | `secure_enabled` | 4 bytes | Byte 0 = `0xA5` enables secure boot. |
| 224–243 | *unknown* | ~20 bits | `swddis` (1b), `pkgid[1:0]` (2b), `idsel` (1b) are likely somewhere here. Exact positions undocumented. |
| 244–245 | mask bank 1 read | 2 bits | `bank0[245:244] = 2'b11` → bank 1 reads masked permanently. |
| 246–247 | mask bank 1 write | 2 bits | `bank0[247:246] = 2'b11` → bank 1 writes masked permanently. |
| 248–249 | mask bank 2 read | 2 bits | `bank0[249:248] = 2'b11` → bank 2 reads masked permanently. |
| 250–251 | mask bank 2 write | 2 bits | `bank0[251:250] = 2'b11` → bank 2 writes masked permanently. |
| 252–253 | mask bank 3 read | 2 bits | `bank0[253:252] = 2'b11` → bank 3 reads masked permanently. |
| 254–255 | mask bank 3 write | 2 bits | `bank0[255:254] = 2'b11` → bank 3 writes masked permanently. |

## Bank 1 — Factory Calibration (bits 256–511)

| Bits | Field | Size | Notes |
|------|-------|------|-------|
| 256–511 | TLV-packed calibration | 32 bytes | Written at factory. Entries terminated by `0xFF`. |

Each TLV entry: `[1B tag][1B length][data...]`

| Tag | ID | Contents |
|-----|----|----------|
| 1 | MAC | BLE MAC address |
| 2 | SN | Serial number |
| 3 | CRYSTAL | Crystal tuning (`cbank_sel`) |
| 4 | ADC | ADC calibration (vol10, vol25, low_mv, high_mv, vbat) |
| 5 | SDMADC | SDMADC calibration (vol_mv, value) |
| 6 | VBUCK | LDO/buck trim values |
| 7 | SECCODE | Security code |
| 8 | LOCALNAME | BLE local name |
| 9 | BATTERY | Battery calibration (ax+b formula) |
| 10 | FWVERIFY | FW verify code |
| 19 | BLECALI | BLE RF calibration per channel |
| 20 | SIPMODE | MPI1/MPI2 mode |
| 21 | CHARGER | Charger calibration (prog_v1p2, cv_vctrl, cc_mn, cc_mp, chg_step) |

Not all entries are present on every chip — only those relevant to the specific variant.

## Bank 2 — Unknown (bits 512–767)

| Bits | Field | Size | Notes |
|------|-------|------|-------|
| 512–767 | *no references in SDK* | 32 bytes | Possibly free. No known usage. |

## Bank 3 — Root Key (bits 768–1023)

| Bits | Field | Size | Notes |
|------|-------|------|-------|
| 768–1023 | `rootkey[255:0]` | 32 bytes | AES-256 root key. Hardware output signal. |

In dedicated mode (`HAL_AES_init(NULL, ...)` + `__HAL_SYSCFG_SET_SECURITY()`),
the AES engine sources this key directly from hardware — software never reads it.

## Hardware Interface Signals

These eFuse values are hardwired out as always-on signals to other SoC blocks
(user manual Table 13-3). They are not accessed via software.

| Signal | Description |
|--------|-------------|
| `uid[127:0]` | Chip ID → AES engine (as IV/nonce) |
| `rootkey[255:0]` | Root key → AES engine (dedicated mode) |
| `swddis` | Disables SWD debug port permanently |
| `pkgid[1:0]` | Package variant identifier |
| `idsel` | ID source select |

## Masking

Bank 0 cannot be masked (it contains the mask control bits).
Masking is irreversible — once the mask bits are burned, the target bank is locked forever.

## Sources

- SF32LB52x User Manual (UM5201, V0.8.4), Section 13.3
- [SiFli SDK `middleware/dfu/efuse.c`](https://github.com/OpenSiFli/SiFli-SDK/blob/718fd434/middleware/dfu/efuse.c)
- [SiFli SDK `drivers/hal/bf0_sys_cfg.c`](https://github.com/OpenSiFli/SiFli-SDK/blob/718fd434/drivers/hal/bf0_sys_cfg.c)
- [SiFli SDK `drivers/Include/bf0_sys_cfg.h`](https://github.com/OpenSiFli/SiFli-SDK/blob/718fd434/drivers/Include/bf0_sys_cfg.h)
- [SiFli SDK `middleware/dfu/dfu.h`](https://github.com/OpenSiFli/SiFli-SDK/blob/718fd434/middleware/dfu/dfu.h)

import re, os, sys, json

def visible_len(s):
    return len(re.sub(r'\x1b\[[^m]*m', '', s))

ESC = "\x1b"
def fg(r,g,b,s):         return f"{ESC}[38;2;{r};{g};{b}m{s}{ESC}[0m"
def fgbg(fr,fg_,fb,br,bg_,bb,s): return f"{ESC}[38;2;{fr};{fg_};{fb}m{ESC}[48;2;{br};{bg_};{bb}m{s}{ESC}[0m"
def dim(s):              return f"{ESC}[2m{s}{ESC}[0m"
def bold(s):             return f"{ESC}[1m{s}{ESC}[0m"

DARK = (18, 24, 32)
UNUSED = (50, 58, 68)
LOST = (220, 50, 60)

# parse JSON from last argv
ignore_empty = "--ignore-empty" in sys.argv
raw = json.loads(sys.argv[-1])

# build DATA: { memory_lower -> [(task, start, end, lost_bytes), ...] }
# auto-assign colors per unique task name
_PALETTE = [
    (232,201,122),(122,184,232),(168,232,122),(232,122,154),(184,122,232),
    (122,232,208),(232,160,122),(122,154,232),(232,224,122),(232,122,232),
    (180,232,140),(232,180,122),(140,180,232),(232,140,180),
]
_task_colors = {}
def _color(task):
    if task not in _task_colors:
        if task == '(unused)':
            _task_colors[task] = UNUSED
        else:
            _task_colors[task] = _PALETTE[len(_task_colors) % len(_PALETTE)]
    return _task_colors[task]

def _parse_size(s):
    s = s.strip().lower()
    if not s or s == '0': return 0
    try:
        if 'mib' in s: return int(float(s.replace('mib','').strip()) * 1024 * 1024)
        if 'kib' in s: return int(float(s.replace('kib','').strip()) * 1024)
        if 'bytes' in s: return int(float(s.replace('bytes','').strip()))
        if 'b' in s: return int(float(s.replace('b','').strip()))
        return int(float(s))
    except: return 0

DATA = {}
for entry in raw:
    mem   = entry["memory"].lower()
    task  = entry["task"]
    start = int(entry["start"], 16)
    end   = int(entry["end"],   16)
    lost  = _parse_size(entry.get("lost", "0"))
    if ignore_empty and end - start + 1 == 0:
        continue
    _color(task)  # register color
    DATA.setdefault(mem, []).append((task, start, end, lost))

# sort each region by start address; drop empty regions when flag set
for mem in DATA:
    DATA[mem].sort(key=lambda x: x[1])
if ignore_empty:
    DATA = {mem: segs for mem, segs in DATA.items()
            if sum(e - s + 1 for t, s, e, _ in segs if t != '(unused)') > 0}

COLORS = _task_colors

def short(t): return t.replace("sysmodule_","")

BAR_H = 3
HALF  = "\u258c"  # LEFT HALF BLOCK: fg=left, bg=right

def _terminal_width():
    for fd in (1, 2, 0):
        try: return os.get_terminal_size(fd).columns
        except OSError: pass
    return 80

def fmt_size_raw(n):
    if n >= 1024: return f"{n/1024:.1f} KiB"
    return f"{n} B"

def fmt_size(segs):
    return fmt_size_raw(sum(e - s + 1 for _,s,e,_ in segs))

def fmt_bpc(n):
    if n >= 1024: return f"~{n/1024:.0f} KiB/char"
    return f"~{n} B/char"

def compute_layout(segs, width):
    MIN_CH = 1
    n = len(segs)
    remainder = max(0, width - MIN_CH * n)
    actual_sizes = [end - start + 1 for (_, start, end, _) in segs]
    total_actual = sum(actual_sizes)
    if total_actual == 0:
        extra = [remainder // n] * n
        extra[-1] += remainder - sum(extra)
    else:
        extra = [round(sz / total_actual * remainder) for sz in actual_sizes]
    drift = sum(extra) + MIN_CH * n - width
    if drift != 0:
        extra[extra.index(max(extra))] -= drift
    result = []
    cursor = 0
    for i, (task, start, end, lost) in enumerate(segs):
        c = COLORS.get(task, (140,140,140))
        w = MIN_CH + extra[i]
        result.append((task, c, cursor, cursor + w, lost, end - start + 1, start))
        cursor += w
    return result

def render_bar(segs, width):
    layout = compute_layout(segs, width)
    pixels = [(DARK, False)] * (width * 2)
    for (task, c, left, right, lost, size, byte_start) in layout:
        for px in range(left * 2, right * 2):
            pixels[px] = (c, False)
        if lost > 0 and right > 0:
            pixels[right * 2 - 1] = (c, True)

    bar_row = ""
    for ch in range(width):
        (lr,lg,lb), l_lost = pixels[ch * 2]
        (rr,rg,rb), r_lost = pixels[ch * 2 + 1]
        if l_lost:
            bar_row += fgbg(*LOST, rr,rg,rb, HALF)
        elif r_lost:
            bar_row += fgbg(lr,lg,lb, *LOST, HALF)
        else:
            bar_row += fgbg(lr,lg,lb, rr,rg,rb, HALF)

    def build_label_rows(layout, width):
        placements = []  # (name_row, col, name_text, size_text, color)
        occupied   = {}  # row -> [(start, end)]

        def overlaps(row, start, end):
            for (s, e) in occupied.get(row, []):
                if start < e and end > s:
                    return True
            return False

        def reserve(row, start, end):
            occupied.setdefault(row, []).append((start, end))

        by_position = sorted(layout, key=lambda x: x[6], reverse=True)

        for (task, c, left, right, lost, size, byte_start) in by_position:
            _name  = f" {short(task)}"
            _sz    = f" {fmt_size_raw(size)}"
            text_w = max(len(_name), len(_sz))
            name   = _name.ljust(text_w)
            sz     = _sz.ljust(text_w)
            col = max(0, left - 1)
            for row in range(40):
                if not overlaps(row,     col, col + text_w) and \
                   not overlaps(row + 1, col, col + text_w):
                    placements.append((row, col, name, sz, c))
                    reserve(row,     col, col + text_w)
                    reserve(row + 1, col, col + text_w)
                    break

        if not placements:
            return []

        max_row = max(r + 1 for (r, *_) in placements)
        W2 = width + 60
        rows   = [[' '] * W2 for _ in range(max_row + 1)]
        colors = [[None]  * W2 for _ in range(max_row + 1)]

        for (name_row, col, name, sz, c) in placements:
            for r in range(name_row):
                rows[r][col] = '│'
                colors[r][col] = c

        for (name_row, col, name, sz, c) in placements:
            for i, ch in enumerate(name):
                if col + i < W2:
                    rows[name_row][col + i] = ch
                    colors[name_row][col + i] = c
            for i, ch in enumerate(sz):
                if col + i < W2:
                    rows[name_row + 1][col + i] = ch
                    colors[name_row + 1][col + i] = c

        result = []
        for row_idx in range(max_row + 1):
            line = ""
            for i in range(W2):
                c  = colors[row_idx][i]
                ch = rows[row_idx][i]
                if c:
                    line += fg(*c, ch)
                elif i < width:
                    line += ch
                # beyond width: only render if colored (i.e. actual label content)
            result.append(line.rstrip())
        return result

    return bar_row, build_label_rows(layout, width)

def addr_line(start, end, width):
    s = hex(start)
    e = hex(end)
    inner = width - len(s) - len(e) - 2
    return f"{dim(s)} {dim('·' * max(0, inner))} {dim(e)}"

def render():
    term_w = _terminal_width()
    margin = 2

    def rightmost_overflow(segs, w):
        from math import ceil
        layout = compute_layout(segs, w)
        last = layout[-1]
        task, c, left, right, lost, size, byte_start = last
        name = f" {short(task)}"
        sz   = f" {fmt_size_raw(size)}"
        text_w = max(len(name), len(sz))
        # how far past the bar end does this label reach?
        col = max(0, left - 1)
        return max(0, col + text_w - w)

    # iterative solve: W + overflow(W) <= term_w - margin
    W = term_w - margin - 10  # initial guess
    for _ in range(20):
        overflow = max(rightmost_overflow(segs, W) for segs in DATA.values())
        new_W = term_w - margin - overflow
        if new_W == W:
            break
        W = new_W
    print()
    for region in DATA:
        segs = DATA[region]
        size = segs[-1][2] - segs[0][1] + 1
        print(f"  {bold(region.upper())}  {dim(fmt_size(segs))}  {dim(fmt_bpc(size // W))}")
        print(f"  {addr_line(segs[0][1], segs[-1][2], W)}")
        bar_row, label_rows = render_bar(segs, W)
        for _ in range(BAR_H):
            print(f"  {bar_row}")
        for row in label_rows:
            print(f"  {row}")
        print()

    print(f"  {dim('─' * min(50, W))}")
    lost_parts = "  ".join(
        f"{region.upper()} {sum(l for _,_,_,l in segs)} B"
        for region, segs in DATA.items()
    )
    print(f"  {fg(*LOST,'█')} {dim(f'lost bytes  {lost_parts}')}")
    print()

render()

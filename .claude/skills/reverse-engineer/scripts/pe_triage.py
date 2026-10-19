"""Static PE triage — stdlib only. Never executes the target.

Usage:
  python -I pe_triage.py <file> [--out DIR] [--min N]
  python -I pe_triage.py <file> --hexdump 0xOFFSET [--len N]
"""
import argparse
import datetime
import hashlib
import math
import os
import re
import struct
import sys
from collections import Counter

MACHINES = {0x14C: "x86 (i386)", 0x8664: "x64 (AMD64)", 0x1C0: "ARM", 0xAA64: "ARM64"}
SUBSYSTEMS = {1: "native", 2: "Windows GUI", 3: "Windows console", 9: "WinCE GUI", 10: "EFI app"}

FINGERPRINTS = [
    (b".qtmetad", "Qt plugin metadata section (Qt app)"),
    (b"Qt5Core.dll", "Qt 5"),
    (b"Qt6Core.dll", "Qt 6"),
    (b"qrc:/", "Qt resource paths (qrc:/)"),
    (b"QSerialPort", "Qt serial port usage"),
    (b"QUdpSocket", "Qt UDP networking"),
    (b"QTcpSocket", "Qt TCP networking"),
    (b"libgcc_s", "MinGW/GCC runtime"),
    (b"libstdc++", "GCC C++ runtime"),
    (b"GCC: (", "GCC compiler banner"),
    (b"Microsoft Visual C++", "MSVC runtime"),
    (b"VCRUNTIME", "MSVC runtime"),
    (b"mscoree.dll", ".NET (mscoree)"),
    (b"_CorExeMain", ".NET entry stub"),
    (b"UPX!", "UPX packed"),
    (b"_MEIPASS", "PyInstaller"),
    (b"PYZ-00.pyz", "PyInstaller archive"),
    (b"python3", "Embedded Python"),
    (b"app.asar", "Electron"),
    (b"Inno Setup", "Inno Setup installer"),
    (b"Nullsoft", "NSIS installer"),
    (b"Embarcadero", "Delphi/C++Builder"),
    (b"Borland", "Borland/Delphi"),
    (b"Go build ID", "Go binary"),
    (b"rust_panic", "Rust binary"),
    (b"SQLite format 3", "Embedded SQLite DB"),
    (b"midiOut", "MIDI output (winmm)"),
    (b"hid.dll", "USB HID"),
    (b"SetupDiGetClassDevs", "Device enumeration (setupapi)"),
    (b"libusb", "libusb"),
]

SUSPICIOUS_IMPORTS = {
    "VirtualAllocEx", "WriteProcessMemory", "CreateRemoteThread", "SetWindowsHookExA",
    "SetWindowsHookExW", "URLDownloadToFileA", "URLDownloadToFileW", "NtUnmapViewOfSection",
    "IsDebuggerPresent", "CheckRemoteDebuggerPresent", "AdjustTokenPrivileges",
    "CryptEncrypt", "CryptDecrypt", "ShellExecuteA", "ShellExecuteW", "WinExec",
    "RegSetValueExA", "RegSetValueExW", "GetAsyncKeyState", "InternetOpenUrlA", "InternetOpenUrlW",
}

CATEGORIES = [
    ("URL", re.compile(r"(?i)\b(?:https?|ftp|wss?|tcp|udp|file)://[^\s\"'<>]{3,}")),
    ("IPv4", re.compile(r"\b(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)(?::\d{2,5})?\b")),
    ("Email", re.compile(r"\b[\w.+-]+@[\w-]+\.[\w.-]{2,}\b")),
    ("Registry", re.compile(r"(?i)\b(?:HKEY_[A-Z_]+|HKLM|HKCU|SOFTWARE\\)[\\\w .-]*")),
    ("Path", re.compile(r"(?i)(?:\b[A-Z]:\\|%\w+%\\|\\\\[\w.-]+\\)[^\s\"'<>|]*")),
    ("File", re.compile(r"(?i)\b[\w-]+\.(?:ini|json|xml|cfg|conf|db|sqlite|dat|log|qml|ui|dll|exe|bin|fw|hex|txt|csv)\b")),
    ("SQL", re.compile(r"(?i)\b(?:SELECT\s.+\sFROM|INSERT\s+INTO|CREATE\s+TABLE|UPDATE\s+\w+\s+SET)\b")),
    ("Auth/Crypto", re.compile(r"(?i)pass(?:word|wd)?|secret|token|api[_-]?key|private key|BEGIN [A-Z ]+KEY|license|serial ?(?:no|number|key)|activation|aes|rsa|sha-?\d|md5|hmac|bearer")),
    ("Network/Proto", re.compile(r"(?i)\b(?:baud|COM\d+|socket|port|udp|tcp|http|osc|midi|rtp|modbus|serial|bluetooth|usb|vid_[0-9a-f]{4}|pid_[0-9a-f]{4})\b")),
    ("Qt", re.compile(r"qrc:/|\bQ[A-Z][A-Za-z]+::|\b(?:on_\w+_clicked|SIGNAL\(|SLOT\()")),
    ("Version", re.compile(r"(?i)\b(?:version|ver|v)\s*[:=]?\s*\d+\.\d+(?:\.\d+)*\b|\b\d+\.\d+\.\d+(?:\.\d+)?\b")),
]


def entropy(data):
    if not data:
        return 0.0
    n = len(data)
    return abs(-sum(c / n * math.log2(c / n) for c in Counter(data).values()))


class PE:
    def __init__(self, data):
        self.data = data
        self.ok = data[:2] == b"MZ"
        if not self.ok:
            return
        self.e_lfanew = struct.unpack_from("<I", data, 0x3C)[0]
        if data[self.e_lfanew:self.e_lfanew + 4] != b"PE\0\0":
            self.ok = False
            return
        coff = self.e_lfanew + 4
        (self.machine, self.nsec, self.timestamp, self.symptr, self.nsyms,
         self.opt_size, self.characteristics) = struct.unpack_from("<HHIIIHH", data, coff)
        opt = coff + 20
        self.magic = struct.unpack_from("<H", data, opt)[0]
        self.pe32plus = self.magic == 0x20B
        self.entry = struct.unpack_from("<I", data, opt + 16)[0]
        self.image_base = (struct.unpack_from("<Q", data, opt + 24)[0] if self.pe32plus
                           else struct.unpack_from("<I", data, opt + 28)[0])
        self.subsystem = struct.unpack_from("<H", data, opt + 68)[0]
        dd_off = opt + (112 if self.pe32plus else 96)
        ndirs = struct.unpack_from("<I", data, dd_off - 4)[0]
        self.dirs = [struct.unpack_from("<II", data, dd_off + i * 8) for i in range(min(ndirs, 16))]
        self.sections = []
        sec_off = opt + self.opt_size
        strtab = self.symptr + self.nsyms * 18 if self.symptr else None
        for i in range(self.nsec):
            o = sec_off + i * 40
            raw_name = data[o:o + 8].rstrip(b"\0")
            name = raw_name.decode("latin-1")
            if name.startswith("/") and strtab and name[1:].isdigit():  # MinGW long section names
                s = strtab + int(name[1:])
                name = data[s:data.index(b"\0", s)].decode("latin-1")
            vsize, va, rsize, rptr = struct.unpack_from("<IIII", data, o + 8)
            chars = struct.unpack_from("<I", data, o + 36)[0]
            self.sections.append(dict(name=name, va=va, vsize=vsize, rptr=rptr, rsize=rsize, chars=chars,
                                      entropy=entropy(data[rptr:rptr + rsize])))

    def rva2off(self, rva):
        for s in self.sections:
            if s["va"] <= rva < s["va"] + max(s["vsize"], s["rsize"]):
                return rva - s["va"] + s["rptr"]
        return None

    def cstr(self, off, limit=512):
        end = self.data.find(b"\0", off, off + limit)
        return self.data[off:end if end != -1 else off + limit].decode("latin-1")

    def imports(self):
        result = {}
        if len(self.dirs) < 2 or not self.dirs[1][0]:
            return result
        off = self.rva2off(self.dirs[1][0])
        tsize, tfmt, ordflag = (8, "<Q", 1 << 63) if self.pe32plus else (4, "<I", 1 << 31)
        while off is not None:
            oft, _, _, name_rva, ft = struct.unpack_from("<IIIII", self.data, off)
            if not (oft or name_rva or ft):
                break
            dll = self.cstr(self.rva2off(name_rva) or 0)
            funcs = []
            t = self.rva2off(oft or ft)
            while t is not None:
                v = struct.unpack_from(tfmt, self.data, t)[0]
                if not v:
                    break
                if v & ordflag:
                    funcs.append(f"ord#{v & 0xFFFF}")
                else:
                    h = self.rva2off(v & 0x7FFFFFFF)
                    funcs.append(self.cstr(h + 2) if h else f"rva#{v:x}")
                t += tsize
            result.setdefault(dll, []).extend(funcs)
            off += 20
        return result

    def exports(self):
        if not self.dirs or not self.dirs[0][0]:
            return []
        off = self.rva2off(self.dirs[0][0])
        if off is None:
            return []
        n_names, names_rva = struct.unpack_from("<I", self.data, off + 24)[0], struct.unpack_from("<I", self.data, off + 32)[0]
        names_off = self.rva2off(names_rva)
        out = []
        for i in range(min(n_names, 5000)):
            r = struct.unpack_from("<I", self.data, names_off + i * 4)[0]
            o = self.rva2off(r)
            if o:
                out.append(self.cstr(o))
        return out

    def overlay_start(self):
        return max((s["rptr"] + s["rsize"] for s in self.sections), default=0)


def extract_strings(data, min_len):
    ascii_re = re.compile(rb"[\x20-\x7e]{%d,}" % min_len)
    utf16_re = re.compile(rb"(?:[\x20-\x7e]\x00){%d,}" % min_len)
    a = [(m.start(), m.group().decode("ascii")) for m in ascii_re.finditer(data)]
    u = [(m.start(), m.group().decode("utf-16le")) for m in utf16_re.finditer(data)]
    return a, u


def hexdump(data, start, length):
    for o in range(start, min(start + length, len(data)), 16):
        chunk = data[o:o + 16]
        hx = " ".join(f"{b:02x}" for b in chunk)
        txt = "".join(chr(b) if 32 <= b < 127 else "." for b in chunk)
        print(f"{o:08x}  {hx:<48}  {txt}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("file")
    ap.add_argument("--out", default=None)
    ap.add_argument("--min", type=int, default=6)
    ap.add_argument("--hexdump", default=None)
    ap.add_argument("--len", type=int, default=256)
    args = ap.parse_args()

    with open(args.file, "rb") as f:
        data = f.read()

    if args.hexdump:
        hexdump(data, int(args.hexdump, 0), args.len)
        return

    out = args.out or os.path.splitext(os.path.basename(args.file))[0] + "_re"
    os.makedirs(out, exist_ok=True)
    md = []
    w = md.append

    w(f"# Triage: {os.path.basename(args.file)}\n")
    w(f"- Size: {len(data):,} bytes")
    for algo in ("md5", "sha1", "sha256"):
        w(f"- {algo.upper()}: `{hashlib.new(algo, data).hexdigest()}`")
    w(f"- Whole-file entropy: {entropy(data):.3f}")

    pe = PE(data)
    imports = {}
    if not pe.ok:
        w("\n**Not a PE file** (no MZ/PE signature). Magic bytes: `" + data[:16].hex(" ") + "`")
    else:
        ts = datetime.datetime.fromtimestamp(pe.timestamp, datetime.timezone.utc)
        w(f"- Arch: {MACHINES.get(pe.machine, hex(pe.machine))}, {'PE32+' if pe.pe32plus else 'PE32'}")
        w(f"- Type: {'DLL' if pe.characteristics & 0x2000 else 'EXE'}, subsystem {SUBSYSTEMS.get(pe.subsystem, pe.subsystem)}")
        w(f"- Compile timestamp: {ts:%Y-%m-%d %H:%M:%S} UTC (can be forged/zeroed)")
        w(f"- ImageBase 0x{pe.image_base:x}, EntryPoint RVA 0x{pe.entry:x} (file off {hex(pe.rva2off(pe.entry) or 0)})")
        sec_dir = pe.dirs[4] if len(pe.dirs) > 4 else (0, 0)
        w(f"- Authenticode signature: {'present (%d bytes at 0x%x)' % (sec_dir[1], sec_dir[0]) if sec_dir[1] else 'none'}")
        clr = pe.dirs[14] if len(pe.dirs) > 14 else (0, 0)
        w(f"- .NET CLR header: {'YES — use ILSpy/dnSpyEx' if clr[0] else 'no'}")
        ov = pe.overlay_start()
        ov_len = len(data) - ov - (sec_dir[1] if sec_dir[0] >= ov else 0)
        if ov_len > 0:
            w(f"- Overlay: {ov_len:,} bytes at 0x{ov:x}, magic `{data[ov:ov + 8].hex(' ')}`, entropy {entropy(data[ov:ov + ov_len]):.2f}")

        w("\n## Sections\n\n| Name | VA | VSize | RawOff | RawSize | Entropy | Flags |\n|---|---|---|---|---|---|---|")
        for s in pe.sections:
            fl = "".join(c for c, bit in (("R", 0x40000000), ("W", 0x80000000), ("X", 0x20000000)) if s["chars"] & bit)
            warn = " ⚠ packed/encrypted?" if s["entropy"] > 7.2 and s["name"] != ".qtmimed" else ""
            w(f"| {s['name']} | 0x{s['va']:x} | {s['vsize']:,} | 0x{s['rptr']:x} | {s['rsize']:,} | {s['entropy']:.2f}{warn} | {fl} |")

        try:
            imports = pe.imports()
        except (struct.error, ValueError) as e:
            w(f"\n_Import parsing failed: {e}_")
        w(f"\n## Imports ({len(imports)} DLLs, {sum(map(len, imports.values()))} functions)\n")
        for dll, funcs in sorted(imports.items(), key=lambda kv: kv[0].lower()):
            shown = ", ".join(funcs[:40]) + (f", … (+{len(funcs) - 40})" if len(funcs) > 40 else "")
            w(f"- **{dll}** ({len(funcs)}): {shown}")
        sus = sorted({f for fs in imports.values() for f in fs if f in SUSPICIOUS_IMPORTS})
        if sus:
            w("\n**Noteworthy APIs:** " + ", ".join(sus))

        try:
            exps = pe.exports()
        except (struct.error, ValueError, TypeError):
            exps = []
        if exps:
            w(f"\n## Exports ({len(exps)})\n\n" + ", ".join(exps[:200]))

    w("\n## Fingerprints\n")
    hits = [(desc, data.find(sig)) for sig, desc in FINGERPRINTS if sig in data]
    w("\n".join(f"- {d} (first at 0x{o:x})" for d, o in hits) or "- none matched")

    a, u = extract_strings(data, args.min)
    with open(os.path.join(out, "strings_ascii.txt"), "w", encoding="utf-8") as f:
        f.writelines(f"0x{o:08x}  {s}\n" for o, s in a)
    with open(os.path.join(out, "strings_utf16.txt"), "w", encoding="utf-8") as f:
        f.writelines(f"0x{o:08x}  {s}\n" for o, s in u)

    buckets = {name: [] for name, _ in CATEGORIES}
    for o, s in a + u:
        if len(s) > 400:
            continue
        for name, rx in CATEGORIES:
            if rx.search(s):
                buckets[name].append((o, s))
                break
    with open(os.path.join(out, "interesting.txt"), "w", encoding="utf-8") as f:
        for name, items in buckets.items():
            seen, uniq = set(), []
            for o, s in sorted(items):
                if s not in seen:
                    seen.add(s)
                    uniq.append((o, s))
            f.write(f"===== {name} ({len(uniq)}) =====\n")
            f.writelines(f"0x{o:08x}  {s}\n" for o, s in uniq)
            f.write("\n")

    w(f"\n## Strings\n\n- ASCII: {len(a):,} → strings_ascii.txt\n- UTF-16LE: {len(u):,} → strings_utf16.txt")
    w("- Categorized: " + ", ".join(f"{k} {len(set(s for _, s in v))}" for k, v in buckets.items()) + " → interesting.txt")

    with open(os.path.join(out, "triage.md"), "w", encoding="utf-8") as f:
        f.write("\n".join(md) + "\n")
    print(f"Wrote {out}/triage.md, interesting.txt, strings_ascii.txt, strings_utf16.txt")


if __name__ == "__main__":
    sys.exit(main())

"""Extract VBA module source code from an .xlsm/.xlsb/.xlam (vbaProject.bin) without Excel.

Implements just enough of [MS-CFB] (compound file) and [MS-OVBA] (dir stream, compression).
usage: python -I extract_vba.py WORKBOOK OUT_DIR
"""
import struct, sys, zipfile, pathlib


def cfb_streams(data: bytes) -> dict:
    sec = 1 << struct.unpack_from('<H', data, 0x1E)[0]
    mini = 1 << struct.unpack_from('<H', data, 0x20)[0]
    first_dir, = struct.unpack_from('<I', data, 0x30)
    cutoff, first_minifat, n_minifat, first_difat, n_difat = struct.unpack_from('<IIIII', data, 0x38)
    def sector(i):
        return data[(i + 1) * sec:(i + 2) * sec]
    difat = list(struct.unpack_from('<109I', data, 0x4C))
    d = first_difat
    for _ in range(n_difat):
        s = sector(d)
        difat += list(struct.unpack_from('<%dI' % (sec // 4 - 1), s))
        d, = struct.unpack_from('<I', s, sec - 4)
    fat = []
    for f in difat:
        if f >= 0xFFFFFFFA:
            continue
        fat += list(struct.unpack_from('<%dI' % (sec // 4), sector(f)))
    def chain(start, table):
        out, s, seen = [], start, set()
        while s < 0xFFFFFFFA and s not in seen:
            seen.add(s); out.append(s); s = table[s]
        return out
    def read(start, size):
        return b''.join(sector(s) for s in chain(start, fat))[:size]
    dirs = b''.join(sector(s) for s in chain(first_dir, fat))
    entries = []
    for k in range(len(dirs) // 128):
        e = dirs[k * 128:(k + 1) * 128]
        nlen, = struct.unpack_from('<H', e, 64)
        name = e[:max(0, nlen - 2)].decode('utf-16-le')
        typ = e[66]
        start, size = struct.unpack_from('<IQ', e, 116)
        entries.append((name, typ, start, size))
    root = entries[0]
    ministream = read(root[2], root[3])
    minifat = []
    for s in chain(first_minifat, fat):
        minifat += list(struct.unpack_from('<%dI' % (sec // 4), sector(s)))
    out = {}
    for name, typ, start, size in entries[1:]:
        if typ != 2:
            continue
        if size < cutoff:
            raw = b''.join(ministream[m * mini:(m + 1) * mini] for m in chain(start, minifat))[:size]
        else:
            raw = read(start, size)
        out.setdefault(name, raw)
    return out


def decompress(buf: bytes) -> bytes:
    if not buf or buf[0] != 1:
        raise ValueError('not an MS-OVBA compressed container')
    out = bytearray()
    i = 1
    while i < len(buf):
        hdr, = struct.unpack_from('<H', buf, i); i += 2
        size = (hdr & 0x0FFF) + 3
        end = min(i + size - 2, len(buf))
        if not (hdr >> 15) & 1:
            out += buf[i:i + 4096]; i += 4096; continue
        start = len(out)
        while i < end:
            flags = buf[i]; i += 1
            for b in range(8):
                if i >= end:
                    break
                if not (flags >> b) & 1:
                    out.append(buf[i]); i += 1
                else:
                    tok, = struct.unpack_from('<H', buf, i); i += 2
                    pos = len(out) - start
                    bits = max((pos - 1).bit_length(), 4)
                    length = (tok & (0xFFFF >> bits)) + 3
                    offset = (tok >> (16 - bits)) + 1
                    for _ in range(length):
                        out.append(out[-offset])
        i = end
    return bytes(out)


def modules(streams: dict):
    d = decompress(streams['dir'])
    i, cur, mods, codepage = 0, {}, [], 1252
    while i + 6 <= len(d):
        rid, size = struct.unpack_from('<HI', d, i); i += 6
        if rid == 0x0009:
            size = 6
        val = d[i:i + size]; i += size
        if rid == 0x0003:
            codepage, = struct.unpack_from('<H', val)
        elif rid == 0x0019:
            cur = {'name': val.decode('cp%d' % codepage, 'replace')}
        elif rid == 0x0032:
            cur['stream'] = val.decode('utf-16-le')
        elif rid == 0x001A and 'stream' not in cur:
            cur['stream'] = val.decode('cp%d' % codepage, 'replace')
        elif rid == 0x0031:
            cur['offset'], = struct.unpack_from('<I', val)
        elif rid == 0x002B:
            mods.append(cur)
    for m in mods:
        raw = streams.get(m.get('stream', m['name']))
        if raw is None:
            continue
        yield m['name'], decompress(raw[m['offset']:]).decode('cp%d' % codepage, 'replace')


def main():
    book, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(book) as z:
        name = next(n for n in z.namelist() if n.lower().endswith('vbaproject.bin'))
        streams = cfb_streams(z.read(name))
    n = 0
    for mod, src in modules(streams):
        (out / f'{mod}.bas').write_text(src.replace('\r\n', '\n'), encoding='utf-8')
        n += 1
        print(f'{mod}: {src.count(chr(10))} lines')
    print(f'{n} modules -> {out}')


if __name__ == '__main__':
    main()

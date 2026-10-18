"""Re-derive the EUR profile in crates/gba-core/src/ffta_rom.rs from a user-owned ROM.

Usage: python3 -I scripts/ffta_eur_verify.py "Final Fantasy Tactics Advance (Europe) (En,Fr,De,Es,It).gba"

Only the USA digests already published in ffta_rom.rs are used; no game bytes are embedded.
"""
import hashlib
import struct
import sys

USA_DIMS, EUR_DIMS = 0x08149510, 0x0814CED8
CODE_SHIFT, IWRAM_SHIFT = 0x38, 0x35DE0C
EUR_STATS, STATS_TILES = 0x08395102, [0x1F0, 0x200, 0x208, 0x208]
OBJ_DIMS = b''.join(struct.pack('<HH', w, h) for w, h in [
    (8, 8), (16, 16), (32, 32), (64, 64), (16, 8), (32, 8), (32, 16), (64, 32), (8, 16), (8, 32), (16, 32), (32, 64)])
USA_FUNCTIONS = [
    (0x14C8, 0x16D4, '6be04b5192e1a3e6776819aa834b155911206bc53444ba3bde476d7b2b14063f', CODE_SHIFT),
    (0x16D4, 0x18A0, 'd883a53ce5bc4c3b4dd08bf37fddef575da1c3cae2c6d30886fbbbc9976eaac6', CODE_SHIFT),
    (0xA3915C, 0xA39458, 'ce5416f21676ea64415b11aa832634fddd97e304b2d6ae29bb0a5e42828a1afc', IWRAM_SHIFT),
    (0xA38EA0, 0xA38FC8, '1e988998a21067596046e2b303da1ad9b6924c18146f396ce0bf5f6e038433c7', IWRAM_SHIFT),
]
USA_CHECKS = [
    (0x14C8, 'f0b5'), (0x159C, '83e0'), (0x15AA, '7cdc'), (0x15FE, '52db'), (0x160C, '4bdc'), (0x169A, '0835'),
    (0x16D4, 'f0b5'), (0x1778, '70db'), (0x1788, '68dc'), (0x17C2, '4bdb'), (0x17D2, '43dc'), (0x1856, '0833'),
]


def main(path):
    rom = open(path, 'rb').read()
    sha256 = hashlib.sha256(rom).hexdigest()
    ok = sha256 == '8fab1e390cb9d5e695bfdb86e81246892e1aa8c0cb4014db8235ff76538ea632'
    print(f'image sha256 {sha256} EUR={ok}')
    for start, end, usa_hash, shift in USA_FUNCTIONS:
        body = bytearray(rom[start + shift:end + shift])
        # Undo the only measured difference: the OBJ dims literal of both ROM producers.
        moved = body.count(struct.pack('<I', EUR_DIMS))
        body = body.replace(struct.pack('<I', EUR_DIMS), struct.pack('<I', USA_DIMS))
        same = hashlib.sha256(body).hexdigest() == usa_hash
        ok &= same
        print(f'USA {start:#08x} -> EUR {start + shift:#08x}: re-pointed literals={moved} usa_digest_match={same}')
    for off, expected in USA_CHECKS + [(0xA3915C, 'f0b5'), (0xA3930C, '1880')]:
        shift = IWRAM_SHIFT if off > 0x100000 else CODE_SHIFT
        same = rom[off + shift:off + shift + 2].hex() == expected
        ok &= same
        print(f'check {off + shift:#08x} {expected} {"OK" if same else "FAIL"}')
    dims = rom[EUR_DIMS - 0x08000000:EUR_DIMS - 0x08000000 + 48] == OBJ_DIMS
    copy = rom[0xA398DC + IWRAM_SHIFT:0xA3990C + IWRAM_SHIFT] == OBJ_DIMS
    ok &= dims and copy
    print(f'OBJ dims table at {EUR_DIMS:#010x}={dims}, IWRAM copy source={copy}')
    # Stats panel list: the only 4-piece ROM list with the panel tile layout, loaded once by code.
    panels = [off for off in range(0, len(rom) - 26, 2) if rom[off:off + 2] == b'\x04\x00'
              and [struct.unpack_from('<H', rom, off + 6 + i * 6)[0] & 1023 for i in range(4)] == STATS_TILES]
    refs = rom.count(struct.pack('<I', EUR_STATS))
    stats = panels == [EUR_STATS - 0x08000000] and refs == 1
    ok &= stats
    print(f'stats panel list {EUR_STATS:#010x}: layout_matches={[hex(0x08000000 + p) for p in panels]} literal_refs={refs} {"OK" if stats else "FAIL"}')
    print('EUR PROFILE VERIFIED' if ok else 'EUR PROFILE MISMATCH')
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv[1]))

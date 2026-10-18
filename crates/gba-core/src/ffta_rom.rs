//! Patch eligibility data, shared with the installer. No game code is embedded.
//! One profile per verified cartridge image. The ROM addresses differ between
//! regions; the IWRAM copies are byte-identical and load at the same addresses.
use sha2::{Digest, Sha256};

pub const HOOK_TAG: u64 = 0x4646544153505231;
pub const RAM_SPRITE_START: u32 = 0x030065a8;
pub const RAM_SPRITE_END: u32 = 0x03006888;
pub const VIS_START:u32=0x030062ec;
pub const VIS_RETURN:u32=0x0300640e;
pub const VIS_END:u32=0x03006414;
const VIS_SHA256: &str = "1e988998a21067596046e2b303da1ad9b6924c18146f396ce0bf5f6e038433c7";

pub struct Profile {
    pub region: &'static str,
    pub sha256: &'static str,
    pub sha1: &'static str,
    /// Thumb OBJ producers: first and second descriptor ABI (docs/ffta_sprite_routines.md).
    pub spr_first: u32,
    pub spr_second: u32,
    /// Instruction ranges of both producers, literal pools excluded.
    pub spr_code: [(u32, u32); 2],
    /// ROM OBJ width/height table read by both producers.
    pub dims: u32,
    /// ROM sources of the IWRAM copies at RAM_SPRITE_START, VIS_START and 03006D28.
    pub ram_source: (usize, usize),
    pub vis_source: (usize, usize),
    pub dims_source: (usize, usize),
    /// Unit-hover stats panel descriptor list; None disables that gap completion.
    pub stats_list: Option<u32>,
    /// Terrain writer range checked by the movement/stage patches, if measured.
    pub terrain_code: Option<(usize, usize, &'static str)>,
    pub checks: &'static [(usize, &'static [u8])],
    pub functions: &'static [(usize, usize, &'static str)],
}

pub const USA: Profile = Profile {
    region: "USA",
    sha256: "43fc8204c6dceee58828aebc7af0c72eb807e99f35ad641c8bb0a4fa8b6edc19",
    sha1: "4ac05441f4de70a4ec3dd932116346c61b8783d9",
    spr_first: 0x080014c8, spr_second: 0x080016d4,
    spr_code: [(0x080014c8, 0x080016c8), (0x080016d4, 0x08001882)],
    dims: 0x08149510,
    ram_source: (0xa3915c, 0xa39458),
    vis_source: (0xa38ea0, 0xa38fc8),
    dims_source: (0xa398dc, 0xa3990c),
    stats_list: Some(0x083916fa),
    terrain_code: Some((0x1af08, 0x1b5d0, "ce26cfe5654123ddf633d32314054c816d6a70315473dcc86928d5cef7237f2c")),
    checks: &[
        (0x14c8, &[0xf0, 0xb5]), (0x159c, &[0x83, 0xe0]),
        (0x15aa, &[0x7c, 0xdc]), (0x15fe, &[0x52, 0xdb]),
        (0x160c, &[0x4b, 0xdc]), (0x169a, &[0x08, 0x35]),
        (0x16d4, &[0xf0, 0xb5]), (0x1778, &[0x70, 0xdb]),
        (0x1788, &[0x68, 0xdc]), (0x17c2, &[0x4b, 0xdb]),
        (0x17d2, &[0x43, 0xdc]), (0x1856, &[0x08, 0x33]),
        (0xa3915c, &[0xf0, 0xb5]), (0xa3930c, &[0x18, 0x80]),
    ],
    functions: &[
        (0x14c8, 0x16d4, "6be04b5192e1a3e6776819aa834b155911206bc53444ba3bde476d7b2b14063f"),
        (0x16d4, 0x18a0, "d883a53ce5bc4c3b4dd08bf37fddef575da1c3cae2c6d30886fbbbc9976eaac6"),
        (0xa3915c, 0xa39458, "ce5416f21676ea64415b11aa832634fddd97e304b2d6ae29bb0a5e42828a1afc"),
    ],
};

/// Europe (En,Fr,De,Es,It), AFXP. Measured against the USA digests: both producers
/// are the USA bytes moved by +0x38 with only the dims literal re-pointed
/// (0x08149510 -> 0x0814ced8); the IWRAM sources are identical, moved by +0x35de0c.
/// See docs/ffta_eur.md.
pub const EUR: Profile = Profile {
    region: "EUR",
    sha256: "8fab1e390cb9d5e695bfdb86e81246892e1aa8c0cb4014db8235ff76538ea632",
    sha1: "9efaf328cbbcbc830be14940e42e4b92d90dfb58",
    spr_first: 0x08001500, spr_second: 0x0800170c,
    spr_code: [(0x08001500, 0x08001700), (0x0800170c, 0x080018ba)],
    dims: 0x0814ced8,
    ram_source: (0xd96f68, 0xd97264),
    vis_source: (0xd96cac, 0xd96dd4),
    dims_source: (0xd976e8, 0xd97718),
    // Only ROM list with the panel layout (tiles 1F0/200/208); runtime: passed to
    // 08001500 from 08001DBD as semi-transparent tiles 1F0/200/208/208, palette 11.
    stats_list: Some(0x08395102),
    // The whole-image SHA256 already pins this code; the USA range has no
    // byte-identical EUR counterpart. The 02007F40 stage fields were checked
    // statically (same offsets and widths), the planes stay VRAM-verified.
    terrain_code: None,
    checks: &[
        (0x1500, &[0xf0, 0xb5]), (0x15d4, &[0x83, 0xe0]),
        (0x15e2, &[0x7c, 0xdc]), (0x1636, &[0x52, 0xdb]),
        (0x1644, &[0x4b, 0xdc]), (0x16d2, &[0x08, 0x35]),
        (0x170c, &[0xf0, 0xb5]), (0x17b0, &[0x70, 0xdb]),
        (0x17c0, &[0x68, 0xdc]), (0x17fa, &[0x4b, 0xdb]),
        (0x180a, &[0x43, 0xdc]), (0x188e, &[0x08, 0x33]),
        (0xd96f68, &[0xf0, 0xb5]), (0xd97118, &[0x18, 0x80]),
    ],
    functions: &[
        (0x1500, 0x170c, "991e33b72f908114b349f73626f506c67f3e055981c5e85131333c1ff5f1914e"),
        (0x170c, 0x18d8, "b56470154f1dcb540bb03e60427d09569722de720c30ba50c5cc52af2e0e7702"),
        (0xd96f68, 0xd97264, "ce5416f21676ea64415b11aa832634fddd97e304b2d6ae29bb0a5e42828a1afc"),
    ],
};

pub const PROFILES: &[&Profile] = &[&USA, &EUR];

impl Profile {
    pub fn verify_hooks(&self, rom: &[u8]) -> bool {
        self.checks.iter().all(|(off, expected)| rom.get(*off..off + expected.len()) == Some(*expected))
            && self.functions.iter().all(|(start, end, hash)| rom.get(*start..*end).is_some_and(|bytes| digest(bytes) == *hash))
    }
    pub fn verify_visibility(&self, rom:&[u8])->bool {
        rom.get(self.vis_source.0..self.vis_source.1).is_some_and(|bytes|
            bytes.starts_with(&[0xf0,0xb5]) && digest(bytes)==VIS_SHA256)
    }
    pub fn verify_terrain(&self, rom:&[u8])->bool {
        self.terrain_code.map_or(true,|(start,end,hash)|rom.get(start..end).is_some_and(|b|digest(b)==hash))
    }
    pub fn producer(&self, pc: u32) -> bool {
        pc == self.spr_first || pc == self.spr_second || pc == RAM_SPRITE_START
    }
    pub fn sprite_pc(&self, pc: u32) -> bool {
        self.spr_code.iter().any(|(start, end)| (*start..*end).contains(&pc))
            || (RAM_SPRITE_START..RAM_SPRITE_END).contains(&pc)
            || (VIS_START..VIS_END).contains(&pc)
    }
}

pub fn digest(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}
/// The profile whose whole-image SHA256 and hook bytes both match.
pub fn profile(rom: &[u8]) -> Option<&'static Profile> {
    let hash = digest(rom);
    PROFILES.iter().copied().find(|p| p.sha256 == hash && p.verify_hooks(rom))
}
pub fn eligible(rom: &[u8]) -> bool { profile(rom).is_some() }
pub fn verify_hooks(rom: &[u8]) -> bool { PROFILES.iter().any(|p| p.verify_hooks(rom)) }
pub fn verify_visibility(rom:&[u8])->bool { PROFILES.iter().any(|p| p.verify_visibility(rom)) }
/// Translation-time guard: every profile's ranges. At run time the guard only
/// interprets the ranges of the loaded image's own profile.
pub fn sprite_pc(pc: u32) -> bool { PROFILES.iter().any(|p| p.sprite_pc(pc)) }

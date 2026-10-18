//! Read-only, all-or-nothing eligibility before any installation side effects.
use sha1::{Digest, Sha1};

/// Change to true to refuse any ROM that cannot activate all patches.
pub const REFUSE_UNVERIFIED_ROM: bool = false;

#[derive(Debug, PartialEq, Eq)]
pub enum InstallPlan { Verified(&'static str), Unpatched(String) }

pub fn plan(data: &[u8]) -> Result<InstallPlan, String> {
    // Always hash the newly read bytes first, even for a malformed file.
    let hash: String = Sha1::digest(data).iter().map(|b| format!("{b:02x}")).collect();
    if data.len() < 0xc0 || data.len() > 32 * 1024 * 1024 {
        return Err("Not a GBA ROM: invalid cartridge size; nothing installed.".into());
    }
    if crate::KNOWN_SHA1.contains(&hash.as_str()) {
        if let Some(profile) = crate::ffta_rom::profile(data).filter(|p| p.sha1 == hash) {
            return Ok(InstallPlan::Verified(profile.region));
        }
    }
    let warning = format!("WARNING: ROM is not an exact verified USA or EUR image (SHA1 {hash}). Installing WITHOUT FFTA patches.");
    if REFUSE_UNVERIFIED_ROM { Err(warning) } else { Ok(InstallPlan::Unpatched(warning)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_non_rom_before_installation() {
        let result = plan(b"This is not a ROM");
        assert!(result.is_err());
        println!("INSTALLER non-ROM: REFUSED, patches=0, filesystem_actions=0: {}", result.unwrap_err());
    }
    #[test]
    #[ignore = "requires user-owned ROM via absolute FFTA_TEST_ROM"]
    fn correct_and_one_byte_changed_rom() {
        let path = std::path::PathBuf::from(std::env::var("FFTA_TEST_ROM").unwrap());
        let mut rom = std::fs::read(&path).unwrap();
        let artifacts = path.parent().unwrap().join("target/ffta-patch-validation");
        std::fs::create_dir_all(&artifacts).unwrap();
        let profile = crate::ffta_rom::profile(&rom).expect("verified USA or EUR image");
        let hook = profile.checks[0].0;
        assert_eq!(plan(&rom).unwrap(), InstallPlan::Verified(profile.region));
        println!("INSTALLER exact {} ROM: VERIFIED, all_hook_checks=PASS, patches=BG+SPR", profile.region);
        // Keep the same game code; accepting that code must not bypass the hash.
        rom[hook] ^= 1;
        let changed = artifacts.join("one-byte-changed.gba");
        std::fs::write(&changed, &rom).unwrap();
        let InstallPlan::Unpatched(message) = plan(&std::fs::read(changed).unwrap()).unwrap() else { panic!("wrong ROM accepted as verified"); };
        assert!(!profile.verify_hooks(&rom));
        println!("INSTALLER one-byte copy (same game code): WARNING, patches=0: {message}");
        let non_rom = artifacts.join("not-a-rom.txt");
        std::fs::write(&non_rom, b"This is not a ROM").unwrap();
        assert!(plan(&std::fs::read(non_rom).unwrap()).is_err());
        rom[hook] ^= 1;
        let last = rom.len() - 1;
        rom[last] ^= 1;
        assert!(profile.verify_hooks(&rom));
        assert!(matches!(plan(&rom), Ok(InstallPlan::Unpatched(_))));
        println!("INSTALLER other hash with intact functions and same game code: WARNING, patches=0");
    }
}

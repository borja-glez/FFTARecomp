//! Scanline PPU renderer (hardware reference, "LCD Video Controller").
//!
//! Renders one visible scanline at HBlank into the RGB555 framebuffer:
//! - modes 0-2: text backgrounds (4/8bpp tiles, flips, scrolling) and
//!   affine backgrounds (8bpp, optional wrap, per-line reference stepping);
//! - modes 3-5: bitmap backgrounds (16bpp, 8bpp paged, small paged);
//! - sprites: 1D/2D mapping, 4/8bpp, flips, affine (incl. double-size),
//!   per-pixel priority against backgrounds, semi-transparent mode,
//!   OBJ-window mode;
//! - windows 0/1, OBJ window, outside region;
//! - color effects: alpha blend, brightness up/down, semi-transparent
//!   sprites forcing alpha against second targets;
//! - backdrop (palette entry 0).
//!
//! Mosaic is not yet implemented. Mid-scanline raster effects are out of
//! scope by design (scanline granularity; see docs/architecture.md §6).
use crate::mem::{extra, render_width, MemMap, MAX_RENDER_WIDTH, VISIBLE_WIDTH};

const TRANSPARENT: u16 = 0xFFFF;

/// One line of layer output over the *render* width (the 240 visible
/// columns plus EXTRA columns per side): RGB555 color or TRANSPARENT.
/// Index `i` is screen column `i - EXTRA`.
type Line = [u16; MAX_RENDER_WIDTH];

#[derive(Clone, Copy)]
struct ObjPixel {
    owner:u32,
    color: u16,
    priority: u8,
    semi: bool,
}

const OBJ_EMPTY: ObjPixel = ObjPixel {
    owner:0,
    color: TRANSPARENT,
    priority: 0xFF,
    semi: false,
};

fn io16(mem: &MemMap, off: usize) -> u16 {
    u16::from_le_bytes([mem.io[off], mem.io[off + 1]])
}

fn pal16(mem: &MemMap, index: usize) -> u16 {
    u16::from_le_bytes([mem.palette[index * 2], mem.palette[index * 2 + 1]]) & 0x7FFF
}

// The battle sky is drawn by changing palette entry 0 on each hardware line.
// Extend that measured gradient into host-only rows instead of freezing its
// first/last color. The 240x160 hardware frame still uses its live palette.
fn extend_sky(top: u16, bottom: u16, line: i32) -> u16 {
    let mut result = 0u16;
    for shift in [0, 5, 10] {
        let a = ((top >> shift) & 31) as i32;
        let b = ((bottom >> shift) & 31) as i32;
        let v = (a + (b - a) * line / 159).clamp(0, 31) as u16;
        result |= v << shift;
    }
    result
}

#[cfg(test)]
mod sky_extension_tests {
    use super::extend_sky;
    #[test]
    fn continues_the_raster_colors_beyond_both_hardware_edges() {
        let top = 0x7d21;
        let bottom = 0x7fcc;
        assert_eq!(extend_sky(top, bottom, 0), top);
        assert_eq!(extend_sky(top, bottom, 159), bottom);
        assert_ne!(extend_sky(top, bottom, -80), top);
        assert_ne!(extend_sky(top, bottom, 239), bottom);
    }
}

/// Screen column (0..VISIBLE_WIDTH) of render-buffer index `i`, or None
/// when `i` falls in the widescreen margins.
fn visible_col(i: usize) -> Option<usize> {
    i.checked_sub(extra()).filter(|&x| x < VISIBLE_WIDTH)
}

/// Window tests use hardware screen coordinates; margin columns inherit
/// the nearest visible edge column.
fn win_x(i: usize) -> u8 {
    (i as isize - extra() as isize).clamp(0, VISIBLE_WIDTH as isize - 1) as u8
}

/// Store one composed pixel: always into the wide buffer, and into the
/// legacy 240-wide framebuffer when the column is on the visible screen.
fn put_pixel(mem: &mut MemMap, line: usize, i: usize, color: u16) {
    let rw = render_width();
    let row=if mem.ffta_movement.zoom_output {
        (line as isize+crate::mem::extra_y() as isize) as usize
    } else {line};
    mem.wide_framebuffer[row * rw + i] = color;
    if !mem.ffta_movement.zoom_output {if let Some(x) = visible_col(i) {
        mem.framebuffer[line * VISIBLE_WIDTH + x] = color;
    }}
}

/// Complete the host view after the 160 hardware scanlines. No guest writes,
/// no extra video events, and no changes to the existing sprite/terrain hooks.
pub fn render_ffta_zoom_rows(mem:&mut MemMap) {
    let ey=crate::mem::extra_y();
    if ey==0 || !(mem.ffta_movement.enabled || mem.ffta_stage.enabled)
        || !(0..4).any(|bg|ffta_terrain(mem,bg) || crate::ffta_stage::layer(mem,bg).is_some()) {return;}
    let rw=render_width();
    mem.wide_framebuffer.copy_within(0..160*rw,ey*rw);
    mem.ffta_movement.zoom_output=true;
    let dispcnt=io16(mem,0);
    if dispcnt&0x80!=0 {
        mem.wide_framebuffer[..rw*crate::mem::render_height()].fill(0x7fff);
        mem.ffta_movement.zoom_output=false;return;
    }
    for sy in (-(ey as isize)..0).chain(160..160+ey as isize) {
        let line=sy as usize;
        let mut backgrounds=[[TRANSPARENT;MAX_RENDER_WIDTH];4];
        let mut used=[false;4];
        for bg in 0..4 {
            // UI text planes belong to this scanline too: omitting them
            // leaves extended OBJ text without its panel background.
            if dispcnt&(0x100<<bg)!=0 {
                render_text_bg(mem,bg,line,&mut backgrounds[bg]);used[bg]=true;
            }
        }
        let mut objects=[OBJ_EMPTY;MAX_RENDER_WIDTH];let mut window=[false;MAX_RENDER_WIDTH];
        let semi=if dispcnt&0x1000!=0 {render_sprites(mem,dispcnt,line,&mut objects,&mut window)}else{false};
        compose(mem,dispcnt,line,&backgrounds,&used,&objects,&window,semi);
    }
    mem.ffta_movement.zoom_output=false;
}

pub fn render_scanline(mem: &mut MemMap, line: usize) {
    if line==0 {mem.ffta_movement.sky_edges[0]=pal16(mem,0);}
    if line==159 {mem.ffta_movement.sky_edges[1]=pal16(mem,0);}
    if crate::ffta_log::enabled() && mem.frames==13100 && matches!(line,0|40|80|120|159) {
        eprintln!("FFTA_SKY frame={} line={} palette0={:04x} bldcnt={:04x} bldy={:04x} dispcnt={:04x}",mem.frames,line,pal16(mem,0),io16(mem,0x50),io16(mem,0x54),io16(mem,0));
    }
    if line==0 && crate::ffta_log::enabled() {mem.ffta_sprites.pixel_counts.clear();}
    let dispcnt = io16(mem, 0x00);

    if dispcnt & 0x80 != 0 {
        // Forced blank: white.
        let row = &mut mem.framebuffer[line * VISIBLE_WIDTH..(line + 1) * VISIBLE_WIDTH];
        row.fill(0x7FFF);
        let rw = render_width();
        let wide = &mut mem.wide_framebuffer[line * rw..(line + 1) * rw];
        wide.fill(0x7FFF);
        return;
    }

    // Latch / step the affine reference accumulators.
    if line == 0 || mem.aff_dirty[0] {
        mem.aff_ref[0] = sign_extend28(read_io32(mem, 0x28));
        mem.aff_ref[1] = sign_extend28(read_io32(mem, 0x2C));
        mem.aff_dirty[0] = false;
    }
    if line == 0 || mem.aff_dirty[1] {
        mem.aff_ref[2] = sign_extend28(read_io32(mem, 0x38));
        mem.aff_ref[3] = sign_extend28(read_io32(mem, 0x3C));
        mem.aff_dirty[1] = false;
    }

    let mode = dispcnt & 7;
    let mut bg_lines = [[TRANSPARENT; MAX_RENDER_WIDTH]; 4];
    let mut bg_used = [false; 4];

    match mode {
        0 => {
            for bg in 0..4 {
                if dispcnt & (0x100 << bg) != 0 {
                    render_text_bg(mem, bg, line, &mut bg_lines[bg]);
                    bg_used[bg] = true;
                }
            }
        }
        1 => {
            for bg in 0..2 {
                if dispcnt & (0x100 << bg) != 0 {
                    render_text_bg(mem, bg, line, &mut bg_lines[bg]);
                    bg_used[bg] = true;
                }
            }
            if dispcnt & 0x400 != 0 {
                render_affine_bg(mem, 2, &mut bg_lines[2]);
                bg_used[2] = true;
            }
        }
        2 => {
            for bg in 2..4 {
                if dispcnt & (0x100 << bg) != 0 {
                    render_affine_bg(mem, bg, &mut bg_lines[bg]);
                    bg_used[bg] = true;
                }
            }
        }
        3 | 4 | 5 => {
            if dispcnt & 0x400 != 0 {
                render_bitmap_bg(mem, mode, dispcnt, line, &mut bg_lines[2]);
                bg_used[2] = true;
            }
        }
        _ => {}
    }

    // Sprites.
    let mut obj_line = [OBJ_EMPTY; MAX_RENDER_WIDTH];
    let mut obj_window = [false; MAX_RENDER_WIDTH];
    let mut line_has_semi = false;
    if dispcnt & 0x1000 != 0 {
        line_has_semi = render_sprites(mem, dispcnt, line, &mut obj_line, &mut obj_window);
    }

    // Step affine accumulators for the next line (PB/PD).
    mem.aff_ref[0] = mem.aff_ref[0].wrapping_add(io16(mem, 0x22) as i16 as i32);
    mem.aff_ref[1] = mem.aff_ref[1].wrapping_add(io16(mem, 0x26) as i16 as i32);
    mem.aff_ref[2] = mem.aff_ref[2].wrapping_add(io16(mem, 0x32) as i16 as i32);
    mem.aff_ref[3] = mem.aff_ref[3].wrapping_add(io16(mem, 0x36) as i16 as i32);

    compose(
        mem,
        dispcnt,
        line,
        &bg_lines,
        &bg_used,
        &obj_line,
        &obj_window,
        line_has_semi,
    );
}


/// Debug aid for widescreen margins. Set FFTA_WIDE_DEBUG before launching:
///   nospr -> sprites are hidden in the margins
///   nobg  -> backgrounds are hidden in the margins
///   none  -> both hidden (margins show only the backdrop)
fn margin_debug() -> (bool, bool) {
    use std::sync::OnceLock;
    static V: OnceLock<(bool, bool)> = OnceLock::new();
    *V.get_or_init(|| match std::env::var("FFTA_WIDE_DEBUG").as_deref() {
        Ok("nospr") => (true, false),
        Ok("nobg") => (false, true),
        Ok("none") => (true, true),
        _ => (false, false),
    })
}

fn read_io32(mem: &MemMap, off: usize) -> u32 {
    u32::from_le_bytes([
        mem.io[off],
        mem.io[off + 1],
        mem.io[off + 2],
        mem.io[off + 3],
    ])
}

fn sign_extend28(v: u32) -> i32 {
    ((v << 4) as i32) >> 4
}

// ---- text backgrounds ----

/// Read-only layer isolation for the requested movement-grid measurements.
pub fn dump_ffta_text_layers(mem: &MemMap) -> Result<(), String> {
    if !crate::ffta_log::enabled() { return Ok(()); }
    let Ok(prefix) = std::env::var("FFTA_BG_DUMP_PREFIX") else { return Ok(()); };
    if io16(mem, 0) & 7 != 0 { return Err("FFTA_BG_DUMP requires text mode 0".into()); }
    let width = render_width();
    for bg in 0..4 {
        if io16(mem, 0) & (0x100 << bg) == 0 { continue; }
        let mut ppm = format!("P6\n{width} 160\n255\n").into_bytes();
        for y in 0..160 {
            let mut pixels = [TRANSPARENT; MAX_RENDER_WIDTH];
            render_text_bg(mem, bg, y, &mut pixels);
            for &pixel in &pixels[..width] {
                let pixel = if pixel == TRANSPARENT { 0 } else { pixel };
                let r=(pixel & 31) as u8; let g=((pixel >> 5)&31) as u8; let b=((pixel >> 10)&31) as u8;
                ppm.extend_from_slice(&[r<<3 | r>>2,g<<3 | g>>2,b<<3 | b>>2]);
            }
        }
        let path=format!("{prefix}-bg{bg}.ppm");
        let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| format!("{path}: {e}"))?;
        std::io::Write::write_all(&mut file,&ppm).map_err(|e| e.to_string())?;
        let cnt=io16(mem,8+bg*2);
        let (w,h)=match cnt>>14 {0=>(256,256),1=>(512,256),2=>(256,512),_=>(512,512)};
        println!("FFTA_BG_LAYER frame={} bg={bg} cnt=0x{cnt:04X} map=0x{:08X} char=0x{:08X} size={w}x{h} priority={} hofs={} vofs={} path={path}",
            mem.frames,0x06000000 + ((cnt as u32 >> 8)&31)*0x800,0x06000000 + ((cnt as u32 >> 2)&3)*0x4000,
            cnt&3,io16(mem,0x10+bg*4)&511,io16(mem,0x12+bg*4)&511);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FftaBgEdge { Wrap, Transparent, Clamp }

impl FftaBgEdge {
    pub(crate) fn for_rom(rom: &[u8]) -> Self {
        if std::env::var("FFTA_PATCH_BG").as_deref() == Ok("0") {
            return Self::Wrap;
        }
        if crate::ffta_rom::profile(rom).is_none() {
            return Self::Wrap;
        }
        match std::env::var("FFTA_BG_EDGE").as_deref() {
            Ok("wrap") => Self::Wrap,
            Ok("clamp") => Self::Clamp,
            _ => Self::Transparent,
        }
    }
}

// Require the paired terrain layers, not a single register signature.
// Semantic identification remains a hypothesis until battle visual validation.
pub fn ffta_terrain(mem: &MemMap, bg: usize) -> bool {
    if io16(mem, 0) & 7 != 0 { return false; }
    for first in [0usize, 2] {
        if bg != first && bg != first + 1 { continue; }
        if io16(mem, 0) & (3 << (8 + first)) != 3 << (8 + first) { continue; }
        if io16(mem, 8 + first * 2) != 0x4c03
            || io16(mem, 10 + first * 2) != 0x4e02 { continue; }
        if io16(mem, 0x10 + first * 4) == io16(mem, 0x14 + first * 4)
            && io16(mem, 0x12 + first * 4) == io16(mem, 0x16 + first * 4) {
            return true;
        }
    }
    false
}

// Measured movement overlay: BG0 contains the blue cells and BG1 their
// foreground fragments. Require their paired battle layout and local scroll;
// static UI (HOFS=0), title/cloud layouts and other background assignments fail.
fn ffta_movement_grid(mem: &MemMap, bg: usize) -> bool {
    if bg > 1 || io16(mem, 0) & 0x0f07 != 0x0f00 || !ffta_terrain(mem, 2) {
        return false;
    }
    if io16(mem, 8) != 0x0a0b || io16(mem, 10) != 0x0b0a { return false; }
    let h=io16(mem,0x10)&511;
    let v=io16(mem,0x12)&511;
    (8..=15).contains(&h) && h==io16(mem,0x14)&511 && v==io16(mem,0x16)&511
}

#[cfg(test)]
mod ffta_bg_tests {
    use super::*;
    #[test]
    fn stats_body_extends_toward_edge_and_keeps_cap_and_alpha() {
        let mut m=MemMap::new(vec![0;192]);
        for o in m.oam.chunks_exact_mut(8) {o[..2].copy_from_slice(&0x200u16.to_le_bytes());}
        m.vram[0x10000+0x208*32..0x10000+0x228*32].fill(0x66);
        m.palette[0x36c..0x36e].copy_from_slice(&0x7c00u16.to_le_bytes());
        let original=m.oam.clone();
        let mut out=[OBJ_EMPTY;MAX_RENDER_WIDTH];let mut window=[false;MAX_RENDER_WIDTH];
        for flip in [0,0x1000] {
            let cap_x=if flip==0 {90}else{170};
            m.ffta_sprites.active=Some(vec![
                crate::ffta_sprites::HostSprite {x:100,y:10,attrs:[0x440a,0xc064|flip,0xb208],slot:0,replaces:true,stats_background:true,..Default::default()},
                crate::ffta_sprites::HostSprite {x:cap_x,y:10,attrs:[0x040a,cap_x as u16,0xb1f0],slot:1,replaces:true,stats_background:true,..Default::default()}
            ]);
            out.fill(OBJ_EMPTY);
            render_sprites_extent(&m,0x1040,10,&mut out,&mut window,164,568);
            let edge=if flip==0 {567}else{0};
            assert_eq!(out[edge].color,0x7c00);assert!(out[edge].semi);
            assert_eq!(out[cap_x as usize+164].priority,0xff);
            m.ffta_sprites.active.as_mut().unwrap()[0].stats_background=false;
            out.fill(OBJ_EMPTY);
            render_sprites_extent(&m,0x1040,10,&mut out,&mut window,164,568);
            assert_eq!(out[edge].priority,0xff);
        }
        out.fill(OBJ_EMPTY);
        render_sprites_extent(&m,0x1040,10,&mut out,&mut window,0,240);
        assert_eq!(out[239].priority,0xff);
        assert_eq!(m.oam,original);
    }
    #[test]
    fn stats_backing_keeps_original_transparency_and_preserves_oam() {
        let mut m=MemMap::new(vec![0;192]);
        for o in m.oam.chunks_exact_mut(8) {o[..2].copy_from_slice(&0x200u16.to_le_bytes());}
        let attrs=[0x040au16,10,0xb001];
        for (i,a) in attrs.iter().enumerate() {m.oam[2*i..2*i+2].copy_from_slice(&a.to_le_bytes());}
        m.vram[0x10020..0x10040].fill(0x11);
        m.palette[0x362..0x364].copy_from_slice(&0x7c00u16.to_le_bytes());
        m.ffta_sprites.active=Some(vec![crate::ffta_sprites::HostSprite {
            x:10,y:10,attrs,slot:0,replaces:true,stats_background:true,..Default::default()
        }]);
        let original=m.oam.clone();
        let mut out=[OBJ_EMPTY;MAX_RENDER_WIDTH];let mut window=[false;MAX_RENDER_WIDTH];
        render_sprites_extent(&m,0x1040,10,&mut out,&mut window,164,568);
        assert_eq!(out[174].color,0x7c00);assert!(out[174].semi);
        out.fill(OBJ_EMPTY);
        render_sprites_extent(&m,0x1040,10,&mut out,&mut window,0,240);
        assert_eq!(out[10].color,0x7c00);assert!(out[10].semi);
        assert_eq!(m.oam,original);
    }
    #[test]
    fn zoom_ui_rows_keep_panel_and_do_not_repeat_outside_map() {
        let mut m=MemMap::new(vec![0;192]);
        m.io.fill(0);
        m.io[8..10].copy_from_slice(&0x0800u16.to_le_bytes());
        m.vram[..32].fill(0x11);
        m.palette[2..4].copy_from_slice(&0x1234u16.to_le_bytes());
        let mut row=[TRANSPARENT;MAX_RENDER_WIDTH];
        m.ffta_movement.zoom_output=true;
        render_text_bg_extent(&m,0,180,&mut row,164,568);
        assert_eq!(row[164],0x1234);
        for line in [(-1isize) as usize,256] {
            render_text_bg_extent(&m,0,line,&mut row,164,568);
            assert!(row[..568].iter().all(|p|*p==TRANSPARENT));
        }
        m.ffta_movement.zoom_output=false;
        render_text_bg_extent(&m,0,256,&mut row,0,240);
        assert_eq!(row[0],0x1234);
    }
    #[test]
    fn verified_stage_survives_hidden_partner_and_bg_reassignment() {
        let mut m=MemMap::new(vec![0;192]);
        m.ffta_stage.enabled=true;
        m.ffta_stage.bases=[Some(0x020091a0),Some(0x0200b1a0)];
        m.ffta_stage.origins=[Some((0,0)),Some((0,0))];
        m.ffta_stage.valid=[true,true];
        m.vram[0..32].fill(0x11);
        m.palette[2..4].copy_from_slice(&0x1234u16.to_le_bytes());
        // Tile 0 is colored; tile 1 is empty. The host plane extends below
        // the hardware's 32 tile rows, independently of the partner layer.
        m.vram[0x6000..0x8000].chunks_exact_mut(2).for_each(|b|b.copy_from_slice(&1u16.to_le_bytes()));
        for bg in [0,2] {
            m.io.fill(0);
            m.io[0..2].copy_from_slice(&(0x100u16<<bg).to_le_bytes());
            m.io[8+bg*2..10+bg*2].copy_from_slice(&0x4c03u16.to_le_bytes());
            assert!(!ffta_terrain(&m,bg));
            let mut wide=[TRANSPARENT;MAX_RENDER_WIDTH];
            render_text_bg_extent(&m,bg,300,&mut wide,164,568);
            assert_eq!(wide[164],0x1234);
            m.ffta_stage.enabled=false;
            render_text_bg_extent(&m,bg,300,&mut wide,164,568);
            assert_eq!(wide[164],TRANSPARENT);
            // 600 synthetic frames: the original-width path is identical.
            for frame in 0..600 {
                let mut off=[TRANSPARENT;MAX_RENDER_WIDTH];
                let mut on=off;
                render_text_bg_extent(&m,bg,frame%160,&mut off,0,240);
                m.ffta_stage.enabled=true;
                render_text_bg_extent(&m,bg,frame%160,&mut on,0,240);
                assert_eq!(off,on);
                m.ffta_stage.enabled=false;
            }
            m.ffta_stage.enabled=true;
        }
        m.io[8..10].copy_from_slice(&0x0a0bu16.to_le_bytes());
        assert_eq!(crate::ffta_stage::layer(&m,0),None);
        m.ffta_stage.valid=[false,false];
        assert_eq!(crate::ffta_stage::layer(&m,2),None);
    }
    #[test]
    fn ffta_movement_edges_preserve_hardware_and_exclude_ui() {
        let mut m=MemMap::new(vec![0;192]);
        for (off,value) in [(0,0x0f00u16),(8,0x0a0b),(10,0x0b0a),(12,0x4c03),(14,0x4e02),
            (0x10,8),(0x14,8),(0x12,88),(0x16,88),(0x18,200),(0x1c,200),(0x1a,56),(0x1e,56)] {
            m.io[off..off+2].copy_from_slice(&value.to_le_bytes());
        }
        m.vram[0x8000..0x8020].fill(0x11);
        m.palette[2..4].copy_from_slice(&0x7c00u16.to_le_bytes());
        assert!(ffta_movement_grid(&m,0));
        assert!(ffta_movement_grid(&m,1));
        for bg in 0..2 {
            let mut baseline=[TRANSPARENT;MAX_RENDER_WIDTH];
            m.ffta_bg_edge=FftaBgEdge::Wrap;
            render_text_bg_extent(&m,bg,0,&mut baseline,0,240);
            for edge in [FftaBgEdge::Wrap,FftaBgEdge::Transparent,FftaBgEdge::Clamp] {
                m.ffta_bg_edge=edge;
                let mut hardware=[TRANSPARENT;MAX_RENDER_WIDTH];
                render_text_bg_extent(&m,bg,0,&mut hardware,0,240);
                assert_eq!(hardware,baseline);
                let mut wide=[TRANSPARENT;MAX_RENDER_WIDTH];
                render_text_bg_extent(&m,bg,0,&mut wide,164,568);
                let expected=if edge==FftaBgEdge::Transparent {TRANSPARENT} else {0x7c00};
                assert_eq!(wide[0],expected);
                assert_eq!(wide[567],expected);
                assert_eq!(wide[164],0x7c00);
            }
        }
        // In-battle static menu scroll must keep its previous rendering policy.
        m.io[0x10]=0; m.io[0x14]=0;
        assert!(!ffta_movement_grid(&m,0));
        // A repeated cloud/title map with the same overlay registers is insufficient.
        m.io[0x10]=8; m.io[0x14]=8; m.io[12]=0;
        assert!(!ffta_movement_grid(&m,0));
        m.io[12]=3; m.io[14]=0;
        assert!(!ffta_movement_grid(&m,1));
    }
    #[test]
    fn ffta_host_coordinates_unlimited_order_affine_and_semi() {
        use crate::ffta_sprites::HostSprite;
        let mut m = MemMap::new(vec![0; 192]);
        for entry in m.oam.chunks_exact_mut(8) { entry[..2].copy_from_slice(&0x200u16.to_le_bytes()); }
        m.vram[0x10020..0x10040].fill(0x11);
        m.vram[0x10040..0x10060].fill(0x22);
        m.palette[0x202..0x204].copy_from_slice(&0x03e0u16.to_le_bytes());
        m.palette[0x204..0x206].copy_from_slice(&0x001fu16.to_le_bytes());
        // Original affine matrix #0: identity. Host objects use the same matrix words.
        m.oam[6..8].copy_from_slice(&0x100u16.to_le_bytes());
        m.oam[30..32].copy_from_slice(&0x100u16.to_le_bytes());
        let mut objects = vec![HostSprite { x: -10000, y: 20, attrs: [20, 0, 1], slot: 0, replaces: false,..HostSprite::default() }; 200];
        objects.push(HostSprite { x: 700, y: 20, attrs: [20 | 0x500, 0, 1], slot: 0, replaces: false,..HostSprite::default() });
        // Same priority, later object: first must win and keep semitransparency.
        objects.push(HostSprite { x: 700, y: 20, attrs: [20, 0, 2], slot: 0, replaces: false,..HostSprite::default() });
        objects.push(HostSprite { x: -600, y: 20, attrs: [20, 0, 1], slot: 0, replaces: false,..HostSprite::default() });
        // Higher priority wins, regardless of insertion order.
        objects.push(HostSprite { x: 600, y: 20, attrs: [20, 0, 0x401], slot: 0, replaces: false,..HostSprite::default() });
        objects.push(HostSprite { x: 600, y: 20, attrs: [20, 0, 2], slot: 0, replaces: false,..HostSprite::default() });
        // Partial submissions can place unlimited host entries past slot 128.
        objects.push(HostSprite { x: 800, y: 20, attrs: [20, 0, 1], slot: 200, replaces: false,..HostSprite::default() });
        m.ffta_sprites.active = Some(objects);
        let mut out = [OBJ_EMPTY; MAX_RENDER_WIDTH];
        let mut window = [false; MAX_RENDER_WIDTH];
        assert!(render_sprites_extent(&m, 0x1040, 22, &mut out, &mut window, 648, 1536));
        assert_eq!(out[1348].color, 0x03e0);
        assert!(out[1348].semi);
        assert_eq!(out[48].color, 0x03e0);
        assert_eq!(out[1248].color, 0x001f);
        assert_eq!(out[1448].color, 0x03e0);
        let mut hardware = [OBJ_EMPTY; MAX_RENDER_WIDTH];
        assert!(!render_sprites_extent(&m, 0x1040, 22, &mut hardware, &mut window, 0, 240));
        assert!(hardware[..240].iter().all(|p| p.priority == 0xff));
    }

    fn scene() -> MemMap {
        let mut m = MemMap::new(vec![0; 192]);
        for (off, value) in [(0, 0x0300u16), (8, 0x4c03), (10, 0x4e02), (0x10, 17), (0x14, 17)] {
            m.io[off..off+2].copy_from_slice(&value.to_le_bytes());
        }
        m.vram[..32].fill(0x11);
        m.palette[2..4].copy_from_slice(&0x1234u16.to_le_bytes());
        m
    }
    #[test]
    fn ffta_bg_wrap_and_zero_extra_preserve_pixels() {
        let mut m = scene();
        let mut baseline = [TRANSPARENT; MAX_RENDER_WIDTH];
        render_text_bg_extent(&m, 0, 0, &mut baseline, 0, 240);
        for policy in [FftaBgEdge::Wrap, FftaBgEdge::Transparent, FftaBgEdge::Clamp] {
            m.ffta_bg_edge = policy;
            let mut actual = [TRANSPARENT; MAX_RENDER_WIDTH];
            render_text_bg_extent(&m, 0, 0, &mut actual, 0, 240);
            assert_eq!(actual, baseline);
        }
    }
    #[test]
    fn ffta_bg_edges_and_unrelated_layers() {
        let mut m = scene();
        assert!(ffta_terrain(&m, 0));
        assert!(!ffta_terrain(&m, 2));
        for (policy, expected) in [(FftaBgEdge::Wrap, 0x1234), (FftaBgEdge::Clamp, 0x1234), (FftaBgEdge::Transparent, TRANSPARENT)] {
            m.ffta_bg_edge = policy;
            let mut pixels = [TRANSPARENT; MAX_RENDER_WIDTH];
            render_text_bg_extent(&m, 0, 0, &mut pixels, 648, 1536);
            assert_eq!(pixels[0], expected);
            assert_eq!(pixels[1535], expected);
            assert_eq!(pixels[648], 0x1234);
        }
        m.io[10] = 0;
        assert!(!ffta_terrain(&m, 0));
        m.ffta_bg_edge = FftaBgEdge::Transparent;
        let mut pixels = [TRANSPARENT; MAX_RENDER_WIDTH];
        render_text_bg_extent(&m, 0, 0, &mut pixels, 164, 568);
        assert_eq!(pixels[0], 0x1234);
        assert_eq!(FftaBgEdge::for_rom(&[0; 192]), FftaBgEdge::Wrap);
    }
}

fn edge_coordinate(value: isize, size: isize, policy: FftaBgEdge) -> Option<usize> {
    match policy {
        FftaBgEdge::Wrap => Some(value.rem_euclid(size) as usize),
        FftaBgEdge::Transparent => (0..size).contains(&value).then_some(value as usize),
        FftaBgEdge::Clamp => Some(value.clamp(0, size - 1) as usize),
    }
}

fn render_text_bg(mem: &MemMap, bg: usize, line: usize, out: &mut Line) {
    render_text_bg_extent(mem, bg, line, out, extra(), render_width());
}

fn render_text_bg_extent(mem: &MemMap, bg: usize, line: usize, out: &mut Line, ex: usize, rw: usize) {
    let cnt = io16(mem, 0x08 + bg * 2);
    let char_base = ((cnt >> 2) & 3) as usize * 0x4000;
    let eight_bpp = cnt & 0x80 != 0;
    let screen_base = ((cnt >> 8) & 0x1F) as usize * 0x800;
    let size = (cnt >> 14) & 3;

    let hofs = (io16(mem, 0x10 + bg * 4) & 0x1FF) as usize;
    let vofs = (io16(mem, 0x12 + bg * 4) & 0x1FF) as usize;

    if ex>0 && crate::ffta_stage::layer(mem,bg).is_some() {
        for sx in 0..rw {
            let x=sx as i32-ex as i32+hofs as i32;let y=(line as isize+vofs as isize) as i32;
            let Some(entry)=crate::ffta_stage::entry(mem,bg,x,y) else{out[sx]=TRANSPARENT;continue;};
            let mut px=x.rem_euclid(8) as usize;let mut py=y.rem_euclid(8) as usize;
            if entry&0x0400!=0{px=7-px;}if entry&0x0800!=0{py=7-py;}
            let byte=mem.vram[char_base+(entry as usize&1023)*32+py*4+px/2];let index=(byte>>(4*(px%2)))&15;
            out[sx]=if index==0{TRANSPARENT}else{pal16(mem,(entry as usize>>12)*16+index as usize)};
        }
        return;
    }

    if ex>0 && ffta_movement_grid(mem,bg) && mem.ffta_movement.enabled && mem.ffta_movement.valid[bg] {
        for sx in 0..rw {
            let x=sx as i32-ex as i32+hofs as i32;
            let y=(line as isize+vofs as isize) as i32;
            let entry=crate::ffta_movement::entry(mem,bg,x,y).unwrap_or(0);
            let mut px=x.rem_euclid(8) as usize;let mut py=y.rem_euclid(8) as usize;
            if entry&0x0400!=0 {px=7-px;}if entry&0x0800!=0 {py=7-py;}
            let tile=(entry&1023) as usize;
            let byte=mem.vram[char_base+tile*32+py*4+px/2];
            let index=(byte>>(4*(px%2)))&15;
            out[sx]=if index==0 {TRANSPARENT}else{pal16(mem,((entry>>12) as usize)*16+index as usize)};
        }
        return;
    }

    let (w_tiles, h_tiles) = match size {
        0 => (32, 32),
        1 => (64, 32),
        2 => (32, 64),
        _ => (64, 64),
    };

    let policy = if ex > 0 && (ffta_terrain(mem, bg) || ffta_movement_grid(mem,bg)) {
        mem.ffta_bg_edge
    } else { FftaBgEdge::Wrap };
    // Extend menu panels only within their real tile plane. Do not repeat
    // a static UI into the rows above/below it when zooming out.
    let vertical_policy = if mem.ffta_movement.zoom_output
        && !ffta_terrain(mem,bg) && !ffta_movement_grid(mem,bg) {
        FftaBgEdge::Transparent
    } else { policy };
    let Some(y) = edge_coordinate(line as isize + vofs as isize, (h_tiles * 8) as isize, vertical_policy) else {
        out[..rw].fill(TRANSPARENT);
        return;
    };
    let ty = y / 8;
    let py = y % 8;
    let map_w = (w_tiles * 8) as isize;

    // Walk the line in tile spans: all pixels within one span share the
    // same map entry, so fetch and decode it once. The walk covers the
    // wide buffer; margin columns read the map at negative / past-240
    // screen positions (wrapping at the map width, like the hardware).
    let mut sx = 0usize;
    while sx < rw {
        let screen_x = sx as isize - ex as isize;
        let Some(x) = edge_coordinate(screen_x + hofs as isize, map_w, policy) else {
            out[sx] = TRANSPARENT;
            sx += 1;
            continue;
        };
        let tx = x / 8;
        let px = x % 8;
        let span = if policy == FftaBgEdge::Wrap { (8 - px).min(rw - sx) } else { 1 };

        // Static (unscrolled) layers such as dialog boxes and HUD were built
        // for exactly 240 px. Their map holds leftovers past column 240 and
        // wrapping would paste the right end of the map into the left
        // margin, so leave the margins empty for them. Scrolling layers
        // (hofs != 0) keep real map content and still read through.
        let in_margin = screen_x < 0 || screen_x >= VISIBLE_WIDTH as isize;
        if in_margin && (margin_debug().1 || (hofs == 0 && policy == FftaBgEdge::Wrap)) {
            for i in 0..span {
                out[sx + i] = TRANSPARENT;
            }
            sx += span;
            continue;
        }

        // Screenblock layout: 32x32-tile blocks, left-to-right then down.
        let block = (ty / 32) * (w_tiles / 32) + tx / 32;
        let entry_idx = block * 1024 + (ty % 32) * 32 + (tx % 32);
        let entry_off = screen_base + entry_idx * 2;
        let entry = u16::from_le_bytes([mem.vram[entry_off], mem.vram[entry_off + 1]]);

        let tile = (entry & 0x3FF) as usize;
        let hflip = entry & 0x400 != 0;
        let vflip = entry & 0x800 != 0;
        let fy = if vflip { 7 - py } else { py };

        if eight_bpp {
            let base = char_base + tile * 64 + fy * 8;
            for i in 0..span {
                let fx = if hflip { 7 - (px + i) } else { px + i };
                let addr = base + fx;
                let color_idx = if addr >= 0x1_0000 {
                    0
                } else {
                    mem.vram[addr] as usize
                };
                out[sx + i] = if color_idx == 0 {
                    TRANSPARENT
                } else {
                    pal16(mem, color_idx)
                };
            }
        } else {
            let base = char_base + tile * 32 + fy * 4;
            let pal_base = ((entry >> 12) as usize & 0xF) * 16;
            for i in 0..span {
                let fx = if hflip { 7 - (px + i) } else { px + i };
                let addr = base + fx / 2;
                let color_idx = if addr >= 0x1_0000 {
                    0
                } else {
                    let b = mem.vram[addr];
                    let nib = if fx & 1 == 0 { b & 0xF } else { b >> 4 } as usize;
                    if nib == 0 {
                        0
                    } else {
                        pal_base + nib
                    }
                };
                out[sx + i] = if color_idx == 0 {
                    TRANSPARENT
                } else {
                    pal16(mem, color_idx)
                };
            }
        }
        sx += span;
    }

    // Static UI layers: stretch the edge columns into the margins so bands,
    // panels and menu boxes reach the edges of the wide frame instead of
    // ending 22 px short. Content stays where the game put it; only the
    // empty margin is filled, and only where the edge pixel is opaque.
    if hofs == 0 && ex > 0 && !margin_debug().1 && policy == FftaBgEdge::Wrap {
        let l = ex;
        let r = ex + VISIBLE_WIDTH - 1;
        let (cl, cr) = (out[l], out[r]);
        if cl != TRANSPARENT {
            for i in 0..l {
                out[i] = cl;
            }
        }
        if cr != TRANSPARENT {
            for i in (r + 1)..rw {
                out[i] = cr;
            }
        }
    }
}

// ---- affine backgrounds ----

fn render_affine_bg(mem: &MemMap, bg: usize, out: &mut Line) {
    let cnt = io16(mem, 0x08 + bg * 2);
    let char_base = ((cnt >> 2) & 3) as usize * 0x4000;
    let screen_base = ((cnt >> 8) & 0x1F) as usize * 0x800;
    let wrap = cnt & 0x2000 != 0;
    let size_tiles = 16usize << ((cnt >> 14) & 3); // 16,32,64,128
    let size_px = (size_tiles * 8) as i32;

    let (pa, pc, ref_base) = if bg == 2 {
        (
            io16(mem, 0x20) as i16 as i32,
            io16(mem, 0x24) as i16 as i32,
            0,
        )
    } else {
        (
            io16(mem, 0x30) as i16 as i32,
            io16(mem, 0x34) as i16 as i32,
            2,
        )
    };
    // The buffer starts EXTRA columns left of screen column 0, so back the
    // reference point up by EXTRA steps of (PA, PC).
    let mut fx = mem.aff_ref[ref_base].wrapping_sub(pa.wrapping_mul(extra() as i32));
    let mut fy = mem.aff_ref[ref_base + 1].wrapping_sub(pc.wrapping_mul(extra() as i32));

    for out_px in out[..render_width()].iter_mut() {
        let mut tx = fx >> 8;
        let mut ty = fy >> 8;
        fx = fx.wrapping_add(pa);
        fy = fy.wrapping_add(pc);

        if wrap {
            tx = tx.rem_euclid(size_px);
            ty = ty.rem_euclid(size_px);
        } else if tx < 0 || ty < 0 || tx >= size_px || ty >= size_px {
            *out_px = TRANSPARENT;
            continue;
        }
        let (tx, ty) = (tx as usize, ty as usize);

        let entry_off = screen_base + (ty / 8) * size_tiles + tx / 8;
        let tile = mem.vram[entry_off] as usize;
        let addr = char_base + tile * 64 + (ty % 8) * 8 + tx % 8;
        let color_idx = if addr < 0x1_0000 {
            mem.vram[addr] as usize
        } else {
            0
        };
        *out_px = if color_idx == 0 {
            TRANSPARENT
        } else {
            pal16(mem, color_idx)
        };
    }
}

// ---- bitmap backgrounds ----

fn render_bitmap_bg(mem: &MemMap, mode: u16, dispcnt: u16, line: usize, out: &mut Line) {
    let page = if dispcnt & 0x10 != 0 { 0xA000usize } else { 0 };
    // Bitmap modes only exist on the 240-px screen; margins stay transparent.
    match mode {
        3 => {
            for (i, out_px) in out[..render_width()].iter_mut().enumerate() {
                let Some(x) = visible_col(i) else {
                    *out_px = TRANSPARENT;
                    continue;
                };
                let off = (line * VISIBLE_WIDTH + x) * 2;
                let c = u16::from_le_bytes([mem.vram[off], mem.vram[off + 1]]) & 0x7FFF;
                *out_px = c;
            }
        }
        4 => {
            for (i, out_px) in out[..render_width()].iter_mut().enumerate() {
                let Some(x) = visible_col(i) else {
                    *out_px = TRANSPARENT;
                    continue;
                };
                let idx = mem.vram[page + line * VISIBLE_WIDTH + x] as usize;
                *out_px = if idx == 0 {
                    TRANSPARENT
                } else {
                    pal16(mem, idx)
                };
            }
        }
        _ => {
            // Mode 5: 160x128 16bpp, paged.
            for (i, out_px) in out[..render_width()].iter_mut().enumerate() {
                match visible_col(i) {
                    Some(x) if x < 160 && line < 128 => {
                        let off = page + (line * 160 + x) * 2;
                        let c = u16::from_le_bytes([mem.vram[off], mem.vram[off + 1]]) & 0x7FFF;
                        *out_px = c;
                    }
                    _ => *out_px = TRANSPARENT,
                }
            }
        }
    }
}

// ---- sprites ----

const OBJ_SIZES: [[(i32, i32); 4]; 3] = [
    [(8, 8), (16, 16), (32, 32), (64, 64)], // square
    [(16, 8), (32, 8), (32, 16), (64, 32)], // horizontal
    [(8, 16), (8, 32), (16, 32), (32, 64)], // vertical
];

/// Returns true if any semi-transparent sprite pixel was written
/// (conservative: a later overwrite by an opaque sprite still counts).
fn render_sprites(
    mem: &MemMap,
    dispcnt: u16,
    line: usize,
    out: &mut [ObjPixel; MAX_RENDER_WIDTH],
    window: &mut [bool; MAX_RENDER_WIDTH],
) -> bool {
    render_sprites_extent(mem, dispcnt, line, out, window, extra(), render_width())
}

fn render_sprites_extent(
    mem: &MemMap, dispcnt: u16, line: usize,
    out: &mut [ObjPixel; MAX_RENDER_WIDTH], window: &mut [bool; MAX_RENDER_WIDTH],
    ex: usize, rw: usize,
) -> bool {
    let mut any_semi = false;
    let one_dim = dispcnt & 0x40 != 0;
    let bitmap_mode = (dispcnt & 7) >= 3;
    let line = line as i32;

    let host = if ex > 0 { mem.ffta_sprites.active.as_deref().unwrap_or(&[]) } else { &[] };
    // Published host entries are stably sorted. Merge in linear time without
    // scanline allocations; guest OAM and affine words remain live reads.
    let (mut slot,mut cursor,mut replaced)=(0usize,0usize,false);
    let expanded=std::iter::from_fn(|| {
        loop {
            if cursor < host.len() && (host[cursor].slot <= slot || slot >= 128) {
                let sprite=&host[cursor]; cursor+=1;
                replaced |= sprite.replaces;
                return Some((sprite.attrs,Some((sprite.x,sprite.y)),sprite.owner,sprite.stats_background));
            }
            if slot >= 128 { return None; }
            let i=slot; slot+=1;
            let emit=!replaced; replaced=false;
            if emit {
                let p=&mem.oam[i*8..i*8+6];
                return Some(([u16::from_le_bytes([p[0],p[1]]),u16::from_le_bytes([p[2],p[3]]),
                    u16::from_le_bytes([p[4],p[5]])],None,mem.ffta_sprites.oam_owners.get(i).copied().unwrap_or(0),false));
            }
        }
    });
    for ([a0, a1, a2], coordinates,owner,stats_background) in expanded {

        let affine = a0 & 0x100 != 0;
        if !affine && a0 & 0x200 != 0 {
            continue; // disabled
        }
        let shape = ((a0 >> 14) & 3) as usize;
        if shape == 3 {
            continue; // prohibited
        }
        let size_idx = ((a1 >> 14) & 3) as usize;
        let (w, h) = OBJ_SIZES[shape][size_idx];
        let double = affine && a0 & 0x200 != 0;
        let (bw, bh) = if double { (w * 2, h * 2) } else { (w, h) };

        let mut y = (a0 & 0xFF) as i32;
        if y >= 160 {
            y -= 256;
        }
        let mut x = (a1 & 0x1FF) as i32;
        // 9-bit X wraps. Values past the right margin read as negative
        // (hardware: >= 240). With EXTRA > 0, a sprite parked in the
        // 240..240+EXTRA range will show in the right margin.
        if x >= (VISIBLE_WIDTH + ex) as i32 {
            x -= 512;
        }
        if let Some((host_x, host_y)) = coordinates { x = host_x; y = host_y; }
        if line < y || line >= y + bh {
            continue;
        }

        let mode = (a0 >> 10) & 3;
        let eight_bpp = a0 & 0x2000 != 0;
        let tile_base = (a2 & 0x3FF) as usize;
        let priority = ((a2 >> 10) & 3) as u8;
        let palette = ((a2 >> 12) & 0xF) as usize;
        // The game uploads temporary snowball pieces before the matching OBJ
        // palette bank. A host sprite may reach the widened area at that point.
        // Check this piece's own bank; a nonempty palette elsewhere is no proof.
        if ex > 0 && !eight_bpp {
            let colors = &mem.palette[0x200 + palette * 32 + 2..0x200 + palette * 32 + 32];
            if colors.iter().all(|&byte| byte == 0) { continue; }
        }

        // Affine parameters.
        let (pa, pb, pc, pd) = if affine {
            let g = ((a1 >> 9) & 0x1F) as usize * 32;
            let p = |off: usize| {
                u16::from_le_bytes([mem.oam[g + off], mem.oam[g + off + 1]]) as i16 as i32
            };
            (p(6), p(14), p(22), p(30))
        } else {
            (0x100, 0, 0, 0x100)
        };

        let row_tiles = if one_dim {
            (w / 8) * if eight_bpp { 2 } else { 1 }
        } else {
            32
        };

        let ly = line - y;
        // Stretch only the rectangular half toward its screen edge. The
        // curved cap and the text keep their original geometry and alpha.
        let stats_body = stats_background && tile_base == 0x208 && !affine && !eight_bpp;
        let (start,end) = if stats_body && a1 & 0x1000 != 0 {
            (-((ex as i32)+x).max(0),bw)
        } else if stats_body {
            (0,(rw as i32-ex as i32-x).max(bw))
        } else {(0,bw)};
        for bx in start..end {
            let sx = x + bx;
            // Buffer index = screen column + EXTRA (margins included).
            let idx = sx + ex as i32;
            if !(0..rw as i32).contains(&idx) {
                continue;
            }
            if margin_debug().0 && (sx < 0 || sx >= VISIBLE_WIDTH as i32) {
                continue;
            }
            let sx_usize = idx as usize;

            // Texture coordinates.
            let (u, v) = if affine {
                let dx = bx - bw / 2;
                let dy = ly - bh / 2;
                let u = ((pa * dx + pb * dy) >> 8) + w / 2;
                let v = ((pc * dx + pd * dy) >> 8) + h / 2;
                if u < 0 || v < 0 || u >= w || v >= h {
                    continue;
                }
                (u, v)
            } else {
                let mut u = bx.clamp(0,w-1);
                let mut v = ly;
                // OBJ attr1 flips: bit 12 horizontal, bit 13 vertical
                // (unlike BG map entries, where flips are bits 10/11).
                if a1 & 0x1000 != 0 {
                    u = w - 1 - u;
                }
                if a1 & 0x2000 != 0 {
                    v = h - 1 - v;
                }
                (u, v)
            };

            let (tu, pu) = (u / 8, u % 8);
            let (tv, pv) = (v / 8, v % 8);
            let tile_step = if eight_bpp { 2 } else { 1 };
            let tile = tile_base + tv as usize * row_tiles as usize + tu as usize * tile_step;
            // In bitmap modes the first 512 tiles overlap the bitmap.
            if bitmap_mode && tile < 512 {
                continue;
            }
            let addr = 0x1_0000
                + if eight_bpp {
                    (tile / 2) * 64 + pv as usize * 8 + pu as usize
                } else {
                    tile * 32 + pv as usize * 4 + pu as usize / 2
                };
            if addr >= 0x1_8000 {
                continue;
            }
            let color_idx = if eight_bpp {
                mem.vram[addr] as usize
            } else {
                let b = mem.vram[addr];
                let nib = if pu & 1 == 0 { b & 0xF } else { b >> 4 } as usize;
                if nib == 0 {
                    0
                } else {
                    palette * 16 + nib
                }
            };
            if color_idx == 0 {
                continue;
            }
            let color = pal16(mem, 256 + color_idx);

            if mode == 2 {
                window[sx_usize] = true;
            } else if out[sx_usize].priority == 0xFF || priority < out[sx_usize].priority {
                out[sx_usize] = ObjPixel {
                    owner,
                    color,
                    priority,
                    semi: mode == 1,
                };
                any_semi |= mode == 1;
            }
        }
    }
    any_semi
}

// ---- composition ----

/// Layer ids for blend targeting: 0-3 BGs, 4 OBJ, 5 backdrop.
#[allow(clippy::too_many_arguments)]
fn compose(
    mem: &mut MemMap,
    dispcnt: u16,
    line: usize,
    bg_lines: &[Line; 4],
    bg_used: &[bool; 4],
    obj_line: &[ObjPixel; MAX_RENDER_WIDTH],
    obj_window: &[bool; MAX_RENDER_WIDTH],
    line_has_semi: bool,
) {
    let backdrop = if mem.ffta_movement.zoom_output {
        let [top,bottom] = mem.ffta_movement.sky_edges;
        extend_sky(top,bottom,line as i32)
    } else {pal16(mem, 0)};

    // Background priorities (compose ties: lower BG number wins; sort
    // keys are unique, so unstable sort matches the old stable sort).
    let mut bg_prio_buf = [(0u8, 0usize); 4];
    let mut nbg = 0;
    for bg in 0..4 {
        if bg_used[bg] {
            bg_prio_buf[nbg] = ((io16(mem, 0x08 + bg * 2) & 3) as u8, bg);
            nbg += 1;
        }
    }
    bg_prio_buf[..nbg].sort_unstable();
    let bg_prio = &bg_prio_buf[..nbg];

    let win0_on = dispcnt & 0x2000 != 0;
    let win1_on = dispcnt & 0x4000 != 0;
    let objwin_on = dispcnt & 0x8000 != 0;
    let any_window = win0_on || win1_on || objwin_on;

    let winin = io16(mem, 0x48);
    let winout = io16(mem, 0x4A);

    let win0h = io16(mem, 0x40);
    let win1h = io16(mem, 0x42);
    let win0v = io16(mem, 0x44);
    let win1v = io16(mem, 0x46);

    let window_y=if mem.ffta_movement.zoom_output {(line as isize).clamp(0,159) as u8}else{line as u8};
    let in_win0_y = win0_on && in_window_range((win0v >> 8) as u8, win0v as u8, window_y);
    let in_win1_y = win1_on && in_window_range((win1v >> 8) as u8, win1v as u8, window_y);

    let bldcnt = io16(mem, 0x50);
    let blend_mode = (bldcnt >> 6) & 3;
    let bldalpha = io16(mem, 0x52);
    let eva = (bldalpha & 0x1F).min(16) as u32;
    let evb = ((bldalpha >> 8) & 0x1F).min(16) as u32;
    let evy = (io16(mem, 0x54) & 0x1F).min(16) as u32;

    // Fast path: no color effects and no semi-transparent sprite pixels
    // on this line, so the second-layer search is dead code - the top
    // layer alone decides each pixel: the first opaque BG in priority
    // order whose window control bit allows it, beaten by the sprite
    // when its priority is <= that BG's (sprites rank above BGs of
    // equal priority).
    if blend_mode == 0 && !line_has_semi {
        for x in 0..render_width() {
            let wx = win_x(x);
            let control: u16 = if !any_window {
                0x3F
            } else if in_win0_y && in_window_range((win0h >> 8) as u8, win0h as u8, wx) {
                winin & 0x3F
            } else if in_win1_y && in_window_range((win1h >> 8) as u8, win1h as u8, wx) {
                (winin >> 8) & 0x3F
            } else if objwin_on && obj_window[x] {
                (winout >> 8) & 0x3F
            } else {
                winout & 0x3F
            };

            let mut color = backdrop;
            let mut top_prio = 0xFFu8;
            for &(bp, bg) in bg_prio {
                if control & (1 << bg) != 0 {
                    let c = bg_lines[bg][x];
                    if c != TRANSPARENT {
                        color = c;
                        top_prio = bp;
                        break;
                    }
                }
            }
            let obj = &obj_line[x];
            if obj.priority != 0xFF && control & 0x10 != 0 && obj.priority <= top_prio {
                color = obj.color;
                if obj.owner!=0 && crate::ffta_sprites::trace_enabled(mem.frames) {*mem.ffta_sprites.pixel_counts.entry(obj.owner).or_default()+=1;}
            }
            put_pixel(mem, line, x, color);
        }
        return;
    }

    for x in 0..render_width() {
        let wx = win_x(x);
        // Window control for this pixel: bits 0-3 BGs, 4 OBJ, 5 effects.
        let control: u16 = if !any_window {
            0x3F
        } else if in_win0_y && in_window_range((win0h >> 8) as u8, win0h as u8, wx) {
            winin & 0x3F
        } else if in_win1_y && in_window_range((win1h >> 8) as u8, win1h as u8, wx) {
            (winin >> 8) & 0x3F
        } else if objwin_on && obj_window[x] {
            (winout >> 8) & 0x3F
        } else {
            winout & 0x3F
        };

        // Find the top two visible layers (OBJ ranks above BGs of equal
        // priority; BGs tie-break by index via the pre-sorted list).
        let obj = &obj_line[x];
        let obj_visible = obj.priority != 0xFF && control & 0x10 != 0;

        let mut first: Option<(u16, usize)> = None;
        let mut second: Option<(u16, usize)> = None;
        'outer: for p in 0..4u8 {
            if obj_visible && obj.priority == p {
                if first.is_none() {
                    first = Some((obj.color, 4));
                } else {
                    second = Some((obj.color, 4));
                    break 'outer;
                }
            }
            for &(bp, bg) in bg_prio {
                if bp == p && control & (1 << bg) != 0 {
                    let c = bg_lines[bg][x];
                    if c != TRANSPARENT {
                        if first.is_none() {
                            first = Some((c, bg));
                        } else {
                            second = Some((c, bg));
                            break 'outer;
                        }
                    }
                }
            }
        }
        let (fc, fl) = first.unwrap_or((backdrop, 5));
        if fl==4 && obj.owner!=0 && crate::ffta_sprites::trace_enabled(mem.frames) {*mem.ffta_sprites.pixel_counts.entry(obj.owner).or_default()+=1;}
        let (sc, sl) = second.unwrap_or((backdrop, 5));

        let effects_allowed = control & 0x20 != 0;
        let semi = obj_visible && fl == 4 && obj.semi;

        let color = if semi && bldcnt & (0x100 << sl) != 0 {
            // Semi-transparent sprite forces alpha against 2nd targets.
            alpha_blend(fc, sc, eva, evb)
        } else if effects_allowed && blend_mode != 0 && bldcnt & (1 << fl) != 0 {
            match blend_mode {
                1 if bldcnt & (0x100 << sl) != 0 => alpha_blend(fc, sc, eva, evb),
                2 => brighten(fc, evy),
                3 => darken(fc, evy),
                _ => fc,
            }
        } else {
            fc
        };

        put_pixel(mem, line, x, color);
    }
}

/// Window coordinate test: x1 = left/top (inclusive), x2 = right/bottom
/// (exclusive); x1 > x2 wraps.
fn in_window_range(x1: u8, x2: u8, v: u8) -> bool {
    if x1 <= x2 {
        (x1..x2).contains(&v)
    } else {
        v >= x1 || v < x2
    }
}

// Color-effect arithmetic models the hardware blend unit, not GBATEK's
// fractional notation: per-channel multiply-accumulate rounds half-up
// (+8 >> 4; the darken subtrahend rounds half-down, +7, so the result
// still rounds half-up), and green passes through a 6-bit lane (the
// real hardware LSB of that lane is PRAM bit 15, which we mask at
// palette fetch — see pal16).

fn alpha_blend(a: u16, b: u16, eva: u32, evb: u32) -> u16 {
    let (ar, ag, ab) = split6(a);
    let (br, bg, bb) = split6(b);
    let r = ((ar * eva + br * evb + 8) >> 4).min(31);
    let g = ((ag * eva + bg * evb + 8) >> 4).min(63);
    let b = ((ab * eva + bb * evb + 8) >> 4).min(31);
    join6(r, g, b)
}

fn brighten(c: u16, evy: u32) -> u16 {
    let (r, g, b) = split6(c);
    join6(
        r + (((31 - r) * evy + 8) >> 4),
        g + (((63 - g) * evy + 8) >> 4),
        b + (((31 - b) * evy + 8) >> 4),
    )
}

fn darken(c: u16, evy: u32) -> u16 {
    let (r, g, b) = split6(c);
    join6(
        r - ((r * evy + 7) >> 4),
        g - ((g * evy + 7) >> 4),
        b - ((b * evy + 7) >> 4),
    )
}

/// Channels as the blend unit sees them: 5-bit red/blue, 6-bit green
/// lane (5-bit value doubled; hardware ORs in PRAM bit 15 as the LSB,
/// which we strip at fetch).
fn split6(c: u16) -> (u32, u32, u32) {
    (
        (c & 31) as u32,
        (((c >> 5) & 31) as u32) << 1,
        ((c >> 10) & 31) as u32,
    )
}

fn join6(r: u32, g: u32, b: u32) -> u16 {
    (r | ((g >> 1) << 5) | (b << 10)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;

    #[test]
    fn mode3_pixel() {
        let mut mem = MemMap::new(vec![]);
        mem.write16(0x0400_0000, 0x0403); // mode 3, BG2 on
        mem.write16(0x0600_0000 + 2 * (5 + 7 * 240), 0x7C1F); // magenta at (5,7)
        render_scanline(&mut mem, 7);
        assert_eq!(mem.framebuffer[7 * 240 + 5], 0x7C1F);
        assert_eq!(mem.framebuffer[7 * 240 + 6], 0x0000);
    }

    #[test]
    fn mode0_text_tile() {
        let mut mem = MemMap::new(vec![]);
        mem.write16(0x0400_0000, 0x0100); // mode 0, BG0 on
                                          // BG0: char base 0, screen base block 31 (0xF800), 4bpp, 32x32.
        mem.write16(0x0400_0008, 31 << 8);
        // Palette 1, color 3 = red-ish.
        mem.write16(0x0500_0000 + (16 + 3) * 2, 0x001F);
        // Tile 1: all pixels = color 3.
        for row in 0..8 {
            mem.write32(0x0600_0000 + 32 + row * 4, 0x3333_3333);
        }
        // Map entry (0,0): tile 1, palette 1.
        mem.write16(0x0600_F800, (1 << 12) | 1);
        render_scanline(&mut mem, 0);
        assert_eq!(mem.framebuffer[0], 0x001F);
        // (8,0) is tile (1,0): map entry 0 -> tile 0 -> transparent -> backdrop.
        assert_eq!(mem.framebuffer[8], pal16(&mem, 0));
    }

    #[test]
    fn sprite_over_backdrop() {
        let mut mem = MemMap::new(vec![]);
        mem.write16(0x0400_0000, 0x1000); // mode 0, OBJ on
                                          // OBJ palette color 1 = green.
        mem.write16(0x0500_0200 + 2, 0x03E0);
        // Tile 1 in OBJ charblock: all color 1 (4bpp).
        for row in 0..8 {
            mem.write32(0x0601_0000 + 32 + row * 4, 0x1111_1111);
        }
        // Sprite 0: 8x8 at (10, 20), tile 1.
        mem.write16(0x0700_0000, 20); // attr0: y=20
        mem.write16(0x0700_0002, 10); // attr1: x=10
        mem.write16(0x0700_0004, 1); // attr2: tile 1
        render_scanline(&mut mem, 22);
        assert_eq!(mem.framebuffer[22 * 240 + 10], 0x03E0);
        assert_eq!(mem.framebuffer[22 * 240 + 18], 0x0000); // outside sprite
    }

    #[test]
    fn sprite_flips_use_obj_attr1_bits() {
        // Tile with one green pixel at texel (0, 0); the rest transparent.
        // Flips relocate the lit screen pixel: hflip = attr1 bit 12,
        // vflip = bit 13 (NOT the BG-entry bits 10/11).
        let mut mem = MemMap::new(vec![]);
        mem.write16(0x0400_0000, 0x1000); // mode 0, OBJ on
        mem.write16(0x0500_0200 + 2, 0x03E0);
        mem.write32(0x0601_0000 + 32, 0x0000_0001); // tile 1 row 0: texel 0 = color 1
        mem.write16(0x0700_0000, 20); // attr0: y=20
        mem.write16(0x0700_0002, 10); // attr1: x=10
        mem.write16(0x0700_0004, 1); // attr2: tile 1
        let lit = |mem: &mut MemMap, line: usize, x: usize| {
            render_scanline(mem, line);
            mem.framebuffer[line * 240 + x] == 0x03E0
        };
        assert!(lit(&mut mem, 20, 10)); // unflipped: top-left
        mem.write16(0x0700_0002, 10 | 0x1000); // hflip
        assert!(lit(&mut mem, 20, 17));
        mem.write16(0x0700_0002, 10 | 0x2000); // vflip
        assert!(lit(&mut mem, 27, 10));
        mem.write16(0x0700_0002, 10 | 0x3000); // both
        assert!(lit(&mut mem, 27, 17));
        // BG-entry flip bits must do nothing on sprites.
        mem.write16(0x0700_0002, 10 | 0x0C00);
        assert!(lit(&mut mem, 20, 10));
    }

    #[test]
    fn brightness_math() {
        assert_eq!(brighten(0x0000, 16), 0x7FFF);
        assert_eq!(darken(0x7FFF, 16), 0x0000);
        assert_eq!(brighten(0x0000, 0), 0x0000);
    }

    // Hardware blend-unit identities:
    // round-half-up MAC, 6-bit green lane.

    #[test]
    fn blend_identity_and_extremes() {
        // eva=16/evb=0 passes every color through exactly, including
        // the green lane round-trip.
        for c in [0x0000u16, 0x7FFF, 0x7C1F, 0x03E0, 0x1234, 0x5A5A] {
            assert_eq!(alpha_blend(c, 0x7FFF ^ c, 16, 0), c);
            assert_eq!(alpha_blend(0x7FFF ^ c, c, 0, 16), c);
        }
        // Saturation: white + white at full coefficients clamps.
        assert_eq!(alpha_blend(0x7FFF, 0x7FFF, 16, 16), 0x7FFF);
        // evy=0 is a no-op for both brightness directions.
        for c in [0x7FFF, 0x1234, 0x5A5A] {
            assert_eq!(brighten(c, 0), c);
            assert_eq!(darken(c, 0), c);
        }
    }

    #[test]
    fn blend_rounds_half_up() {
        // 50/50 of red 15 and 16: true value 15.5, hardware rounds up.
        // (15*8 + 16*8 + 8) >> 4 = 16.
        assert_eq!(alpha_blend(15, 16, 8, 8) & 31, 16);
        // Red 0 with 1 at evb=8: 0.5 rounds up to 1 (truncation gave 0).
        assert_eq!(alpha_blend(0, 1, 8, 8) & 31, 1);
        // Green's 6-bit lane rounds up only at fraction >= 12/16:
        // greens 15/16 at 8/8 -> S=124 in g6 (30*8+32*8=496; +8 >>4 =
        // 31; >>1 = 15): the half-step stays DOWN on green.
        assert_eq!((alpha_blend(15 << 5, 16 << 5, 8, 8) >> 5) & 31, 15);
        // ...but green 15/eva=4, 16/evb=12 (true 15.75) rounds up.
        assert_eq!((alpha_blend(15 << 5, 16 << 5, 4, 12) >> 5) & 31, 16);
    }

    #[test]
    fn brightness_rounds() {
        // Brighten red 0 by 1/16: (31*1 + 8) >> 4 = 2 (truncation: 1).
        assert_eq!(brighten(0, 1) & 31, 2);
        // Darken red 31 by 1/16: 31 - ((31 + 7) >> 4) = 29.
        assert_eq!(darken(31, 1) & 31, 29);
        // Green lane: brighten green 0 by 1/16 -> g6 = (63+8)>>4 = 4,
        // back to 5-bit = 2.
        assert_eq!((brighten(0, 1) >> 5) & 31, 2);
    }
}

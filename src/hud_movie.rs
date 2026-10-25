//! In-memory patch of the HUD movie (`menu/01_000_fe.gfx`).
//!
//! Earlier packages shipped a whole replacement `01_000_fe.gfx`, which
//! conflicts with every other mod that ships its own copy of that file. Now
//! the game's file is left alone on disk: when Scaleform opens the HUD movie,
//! the bytes it is about to read are patched in memory with the few changes
//! this mod needs, on top of whichever `01_000_fe.gfx` is loaded:
//!
//! * the vanilla Spirit Ash panel (`Buddy`) is made transparent, because it
//!   can only show one Ash;
//! * the six remote-player tags (`RemotePCTag0..5`) become the Ash rows:
//!   0.82 of their loaded scale, stacked every 45 px (times the `Buddy`
//!   panel's scale) from the height of the `Buddy` panel, and the icons a summon does not use (rank, big rune,
//!   voice chat, multiplayer weakness) are made transparent;
//! * the role line of the row (`RoleName`) is moved onto the name line at
//!   half opacity: an Ash being dismissed shows its name there (dimmed).
//!
//! Elements are found by instance name, never by offset. Each of the two
//! groups is applied only when every element it needs is present and
//! decodes cleanly; otherwise that group is skipped and the movie is served
//! as loaded. The patch is idempotent.
//!
//! The hook sits on `CS::CSScaleformFileOpener`'s OpenFile override, found
//! through the game's RTTI (vtable slot 1), and the result is checked to be
//! a `Scaleform::MemoryFile` whose field layout is verified from its own
//! accessor code. Nothing is hooked when any of those checks fails.

use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

/// Patch values (this mod's own HUD layout).
const ROW_SCALE_FIXED: i32 = 53_739; // 0.82 in 16.16 fixed point
const ROW_SPACING_TWIPS: i32 = 900; // 45 px
/// Moves the rows left so their name and bar start at the HUD's left edge,
/// with the portrait frame and the quick slots (user's screenshot, 24-10:
/// text at x 147 of 2000 -> 52; -91 HUD px at scale 1).
const ROW_SHIFT_X_TWIPS: i32 = -1820;
/// Moves the block up from the Buddy panel into the free space under the
/// status icons, clear of the game's action prompt ("Kick") and the quick
/// slots (user's screenshot, 24-10: first row at y 565 of 1125 -> ~200;
/// -350 HUD px at scale 1).
const ROW_SHIFT_Y_TWIPS: i32 = -7000;
const ROW_COUNT: usize = 6;
const HIDDEN_ROW_ICONS: [&[u8]; 4] = [
    b"RankIcon",
    b"BigRune",
    b"VoiceChatIconRoot",
    b"MultiPlayWeakIcon",
];
const ROW_NAME: &[u8] = b"Name";
const DIMMED_ROW_NAME: &[u8] = b"RoleName";
const DIMMED_ALPHA: i32 = 128; // 0.5 in 8.8 fixed point
const BUDDY_PANEL: &[u8] = b"Buddy";
const ROW_TAG_PREFIX: &[u8] = b"RemotePCTag";
pub(crate) const HUD_MOVIE_FILE: &str = "01_000_fe.gfx";

const TAG_END: u16 = 0;
const TAG_PLACE_OBJECT2: u16 = 26;
const TAG_DEFINE_SPRITE: u16 = 39;

// ---------------------------------------------------------------------------
// GFX (uncompressed SWF container) model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Tag {
    code: u16,
    long_header: bool,
    body: Vec<u8>,
}

fn parse_tags(data: &[u8], mut pos: usize) -> Result<Vec<Tag>, &'static str> {
    let mut tags = Vec::new();
    while pos < data.len() {
        let header = read_u16(data, pos).ok_or("truncated tag header")?;
        let code = header >> 6;
        let mut len = usize::from(header & 0x3f);
        let long_header = len == 0x3f;
        pos += 2;
        if long_header {
            len = read_u32(data, pos).ok_or("truncated long tag header")? as usize;
            pos += 4;
        }
        let end = pos.checked_add(len).ok_or("tag length overflow")?;
        let body = data
            .get(pos..end)
            .ok_or("tag runs past the end of the movie")?;
        tags.push(Tag {
            code,
            long_header,
            body: body.to_vec(),
        });
        pos = end;
        if code == TAG_END {
            break;
        }
    }
    Ok(tags)
}

fn write_tags(out: &mut Vec<u8>, tags: &[Tag]) -> Result<(), &'static str> {
    for tag in tags {
        let len = tag.body.len();
        if tag.long_header || len >= 0x3f {
            let len = u32::try_from(len).map_err(|_| "tag too large")?;
            out.extend_from_slice(&((tag.code << 6) | 0x3f).to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
        } else {
            out.extend_from_slice(&((tag.code << 6) | len as u16).to_le_bytes());
        }
        out.extend_from_slice(&tag.body);
    }
    Ok(())
}

fn read_u16(data: &[u8], pos: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(pos..pos + 2)?.try_into().ok()?))
}

fn read_u32(data: &[u8], pos: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?))
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], byte: usize) -> Self {
        Self {
            data,
            bit: byte * 8,
        }
    }

    fn unsigned(&mut self, count: u32) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            let byte = *self.data.get(self.bit >> 3)?;
            value = (value << 1) | u32::from((byte >> (7 - (self.bit & 7))) & 1);
            self.bit += 1;
        }
        Some(value)
    }

    fn signed(&mut self, count: u32) -> Option<i32> {
        let value = self.unsigned(count)?;
        if count == 0 {
            return Some(0);
        }
        let shift = 32 - count;
        Some(((value << shift) as i32) >> shift)
    }

    fn byte_pos(&self) -> usize {
        self.bit.div_ceil(8)
    }
}

#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    bit: usize,
}

impl BitWriter {
    fn put(&mut self, value: u32, count: u32) {
        for index in (0..count).rev() {
            if self.bit % 8 == 0 {
                self.out.push(0);
            }
            if (value >> index) & 1 == 1 {
                let last = self.out.len() - 1;
                self.out[last] |= 0x80 >> (self.bit % 8);
            }
            self.bit += 1;
        }
    }

    fn put_signed(&mut self, value: i32, count: u32) {
        self.put(value as u32 & mask(count), count);
    }
}

fn mask(count: u32) -> u32 {
    if count >= 32 {
        u32::MAX
    } else {
        (1u32 << count) - 1
    }
}

/// Smallest signed bit width that holds every value.
fn signed_bits(values: &[i32]) -> u32 {
    values
        .iter()
        .map(|&value| {
            if value == 0 {
                0
            } else if value > 0 {
                33 - value.leading_zeros()
            } else {
                33 - (!value).leading_zeros()
            }
        })
        .max()
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Matrix {
    scale: Option<(i32, i32)>,
    rotate: Option<(i32, i32)>,
    translate: (i32, i32),
}

impl Matrix {
    fn read(data: &[u8], pos: usize) -> Option<(Self, usize)> {
        let mut bits = BitReader::new(data, pos);
        let scale = if bits.unsigned(1)? == 1 {
            let count = bits.unsigned(5)?;
            Some((bits.signed(count)?, bits.signed(count)?))
        } else {
            None
        };
        let rotate = if bits.unsigned(1)? == 1 {
            let count = bits.unsigned(5)?;
            Some((bits.signed(count)?, bits.signed(count)?))
        } else {
            None
        };
        let count = bits.unsigned(5)?;
        let translate = (bits.signed(count)?, bits.signed(count)?);
        Some((
            Self {
                scale,
                rotate,
                translate,
            },
            bits.byte_pos(),
        ))
    }

    fn write(&self, out: &mut Vec<u8>) -> Option<()> {
        let mut bits = BitWriter::default();
        for pair in [self.scale, self.rotate] {
            match pair {
                Some((a, b)) => {
                    let count = signed_bits(&[a, b]);
                    if count > 31 {
                        return None;
                    }
                    bits.put(1, 1);
                    bits.put(count, 5);
                    bits.put_signed(a, count);
                    bits.put_signed(b, count);
                }
                None => bits.put(0, 1),
            }
        }
        let (x, y) = self.translate;
        let count = signed_bits(&[x, y]);
        if count > 31 {
            return None;
        }
        bits.put(count, 5);
        bits.put_signed(x, count);
        bits.put_signed(y, count);
        out.extend_from_slice(&bits.out);
        Some(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ColorTransform {
    mult: Option<[i32; 4]>,
    add: Option<[i32; 4]>,
}

impl ColorTransform {
    fn read(data: &[u8], pos: usize) -> Option<(Self, usize)> {
        let mut bits = BitReader::new(data, pos);
        let has_add = bits.unsigned(1)? == 1;
        let has_mult = bits.unsigned(1)? == 1;
        let count = bits.unsigned(4)?;
        let read4 = |bits: &mut BitReader| -> Option<[i32; 4]> {
            Some([
                bits.signed(count)?,
                bits.signed(count)?,
                bits.signed(count)?,
                bits.signed(count)?,
            ])
        };
        let mult = if has_mult {
            Some(read4(&mut bits)?)
        } else {
            None
        };
        let add = if has_add {
            Some(read4(&mut bits)?)
        } else {
            None
        };
        Some((Self { mult, add }, bits.byte_pos()))
    }

    fn write(&self, out: &mut Vec<u8>) -> Option<()> {
        let mut values = Vec::new();
        values.extend(self.mult.iter().flatten());
        values.extend(self.add.iter().flatten());
        let count = signed_bits(&values).max(1);
        if count > 15 {
            return None;
        }
        let mut bits = BitWriter::default();
        bits.put(u32::from(self.add.is_some()), 1);
        bits.put(u32::from(self.mult.is_some()), 1);
        bits.put(count, 4);
        for terms in [self.mult, self.add].into_iter().flatten() {
            for value in terms {
                bits.put_signed(value, count);
            }
        }
        out.extend_from_slice(&bits.out);
        Some(())
    }

    /// The same transform with the alpha multiplier at zero.
    fn transparent(current: Option<Self>) -> Self {
        let mut result = current.unwrap_or(Self {
            mult: None,
            add: None,
        });
        let mut mult = result.mult.unwrap_or([256, 256, 256, 256]);
        mult[3] = 0;
        result.mult = Some(mult);
        result
    }

    /// The same transform at half opacity.
    fn dimmed(current: Option<Self>) -> Self {
        let mut result = Self::transparent(current);
        if let Some(mult) = result.mult.as_mut() {
            mult[3] = DIMMED_ALPHA;
        }
        result
    }
}

/// PlaceObject2 with every optional field decoded; clip actions and anything
/// after them are kept as raw bytes.
#[derive(Clone, Debug, PartialEq)]
struct PlaceObject2 {
    flags: u8,
    depth: u16,
    character: Option<u16>,
    matrix: Option<Matrix>,
    color: Option<ColorTransform>,
    ratio: Option<u16>,
    name: Option<Vec<u8>>,
    clip_depth: Option<u16>,
    rest: Vec<u8>,
}

impl PlaceObject2 {
    fn parse(body: &[u8]) -> Option<Self> {
        let flags = *body.first()?;
        let depth = read_u16(body, 1)?;
        let mut pos = 3;
        let character = if flags & 0x02 != 0 {
            let value = read_u16(body, pos)?;
            pos += 2;
            Some(value)
        } else {
            None
        };
        let matrix = if flags & 0x04 != 0 {
            let (value, next) = Matrix::read(body, pos)?;
            pos = next;
            Some(value)
        } else {
            None
        };
        let color = if flags & 0x08 != 0 {
            let (value, next) = ColorTransform::read(body, pos)?;
            pos = next;
            Some(value)
        } else {
            None
        };
        let ratio = if flags & 0x10 != 0 {
            let value = read_u16(body, pos)?;
            pos += 2;
            Some(value)
        } else {
            None
        };
        let name = if flags & 0x20 != 0 {
            let end = pos + body.get(pos..)?.iter().position(|&byte| byte == 0)?;
            let value = body[pos..end].to_vec();
            pos = end + 1;
            Some(value)
        } else {
            None
        };
        let clip_depth = if flags & 0x40 != 0 {
            let value = read_u16(body, pos)?;
            pos += 2;
            Some(value)
        } else {
            None
        };
        let rest = body.get(pos..)?.to_vec();
        if flags & 0x80 == 0 && !rest.is_empty() {
            return None;
        }
        Some(Self {
            flags,
            depth,
            character,
            matrix,
            color,
            ratio,
            name,
            clip_depth,
            rest,
        })
    }

    fn encode(&self) -> Option<Vec<u8>> {
        let mut flags = self.flags & 0x81;
        flags |= if self.character.is_some() { 0x02 } else { 0 };
        flags |= if self.matrix.is_some() { 0x04 } else { 0 };
        flags |= if self.color.is_some() { 0x08 } else { 0 };
        flags |= if self.ratio.is_some() { 0x10 } else { 0 };
        flags |= if self.name.is_some() { 0x20 } else { 0 };
        flags |= if self.clip_depth.is_some() { 0x40 } else { 0 };
        let mut out = vec![flags];
        out.extend_from_slice(&self.depth.to_le_bytes());
        if let Some(character) = self.character {
            out.extend_from_slice(&character.to_le_bytes());
        }
        if let Some(matrix) = &self.matrix {
            matrix.write(&mut out)?;
        }
        if let Some(color) = &self.color {
            color.write(&mut out)?;
        }
        if let Some(ratio) = self.ratio {
            out.extend_from_slice(&ratio.to_le_bytes());
        }
        if let Some(name) = &self.name {
            out.extend_from_slice(name);
            out.push(0);
        }
        if let Some(clip_depth) = self.clip_depth {
            out.extend_from_slice(&clip_depth.to_le_bytes());
        }
        out.extend_from_slice(&self.rest);
        Some(out)
    }

    fn named(&self, name: &[u8]) -> bool {
        self.name.as_deref() == Some(name)
    }
}

fn find_placement(tags: &[Tag], name: &[u8]) -> Option<(usize, PlaceObject2)> {
    let mut found = None;
    for (index, tag) in tags.iter().enumerate() {
        if tag.code != TAG_PLACE_OBJECT2 {
            continue;
        }
        let Some(place) = PlaceObject2::parse(&tag.body) else {
            continue;
        };
        if place.named(name) {
            if found.is_some() {
                return None; // ambiguous: leave it alone
            }
            found = Some((index, place));
        }
    }
    found
}

fn replace_placement(tags: &mut [Tag], index: usize, place: &PlaceObject2) -> Option<()> {
    tags[index].body = place.encode()?;
    Some(())
}

/// What `patch_hud_movie` changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PatchReport {
    pub buddy_panel_hidden: bool,
    pub ash_rows: bool,
    /// The row's dimmed name line (for an Ash being dismissed) is in place.
    pub dimmed_names: bool,
    pub changed: bool,
}

/// Patches a loaded `01_000_fe.gfx`. Returns the new movie when anything
/// changed, `Ok(None)` when it already has every change, and an error when
/// the movie cannot be read safely (then it must be served unchanged).
pub(crate) fn patch_hud_movie(input: &[u8]) -> Result<(Option<Vec<u8>>, PatchReport), String> {
    let signature = input.get(0..3).ok_or("movie too short")?;
    if signature != b"GFX" && signature != b"FWS" {
        return Err("not an uncompressed GFX/SWF movie".into());
    }
    let declared = read_u32(input, 4).ok_or("movie too short")? as usize;
    // The game's buffer may carry archive padding after the declared length.
    if declared < 16 || declared > input.len() {
        return Err(format!(
            "declared length {declared} does not fit the {} loaded bytes",
            input.len()
        ));
    }
    let data = &input[..declared];
    let rect_bits = u32::from(data[8] >> 3);
    let header_len = 8 + (5 + 4 * rect_bits as usize).div_ceil(8) + 4;
    if header_len >= data.len() {
        return Err("movie header is truncated".into());
    }
    let mut tags = parse_tags(data, header_len).map_err(str::to_owned)?;
    let original = tags.clone();
    let mut report = PatchReport::default();

    // Group 1: hide the single-Ash panel.
    let buddy = find_placement(&tags, BUDDY_PANEL);
    if let Some((index, mut place)) = buddy.clone() {
        place.color = Some(ColorTransform::transparent(place.color));
        if replace_placement(&mut tags, index, &place).is_some() {
            report.buddy_panel_hidden = true;
        }
    }

    // Group 2: turn the remote-player tags into the Ash rows.
    if let Some(rows) = plan_ash_rows(&tags, buddy.as_ref().map(|(_, place)| place)) {
        if rows.already_applied {
            report.ash_rows = true;
        } else {
            let mut candidate = tags.clone();
            if apply_ash_rows(&mut candidate, &rows).is_some() {
                tags = candidate;
                report.ash_rows = true;
            }
        }
        // Own step, so movies laid out by an earlier package also get it.
        if report.ash_rows {
            if rows.dimmed_names_applied {
                report.dimmed_names = true;
            } else {
                let mut candidate = tags.clone();
                if apply_dimmed_names(&mut candidate, rows.sprite_index).is_some() {
                    tags = candidate;
                    report.dimmed_names = true;
                }
            }
        }
    }

    if tags == original {
        return Ok((None, report));
    }
    report.changed = true;
    let mut out = Vec::with_capacity(declared + 256);
    out.extend_from_slice(&data[..header_len]);
    write_tags(&mut out, &tags).map_err(str::to_owned)?;
    let total = u32::try_from(out.len()).map_err(|_| "movie too large")?;
    out[4..8].copy_from_slice(&total.to_le_bytes());
    Ok((Some(out), report))
}

struct AshRowsPlan {
    rows: Vec<(usize, PlaceObject2)>,
    sprite_index: usize,
    base_y: i32,
    /// Vertical scale of the Buddy panel (16.16); another HUD layout may
    /// have scaled the HUD, and the row spacing follows it.
    base_scale: i32,
    /// Every hidden icon is already transparent: the rows were laid out by
    /// an earlier pass (or a movie that already carries this layout).
    already_applied: bool,
    /// The role line already sits on the name line at half opacity.
    dimmed_names_applied: bool,
}

fn plan_ash_rows(tags: &[Tag], buddy: Option<&PlaceObject2>) -> Option<AshRowsPlan> {
    let buddy_matrix = buddy?.matrix?;
    let base_y = buddy_matrix.translate.1;
    let base_scale = buddy_matrix.scale.map_or(FIXED_ONE, |(_, y)| y);
    if buddy_matrix.rotate.is_some() || base_scale <= 0 {
        return None;
    }
    let mut rows = Vec::with_capacity(ROW_COUNT);
    let mut character = None;
    for row in 0..ROW_COUNT {
        let mut name = ROW_TAG_PREFIX.to_vec();
        name.extend_from_slice(row.to_string().as_bytes());
        let (index, place) = find_placement(tags, &name)?;
        let matrix = place.matrix?;
        if matrix.rotate.is_some() || place.character.is_none() {
            return None;
        }
        if character.is_some() && character != place.character {
            return None;
        }
        character = place.character;
        rows.push((index, place));
    }
    let character = character?;
    let sprite_index = tags.iter().position(|tag| {
        tag.code == TAG_DEFINE_SPRITE && read_u16(&tag.body, 0) == Some(character)
    })?;
    let children = parse_tags(&tags[sprite_index].body, 4).ok()?;
    let mut already_applied = true;
    for icon in HIDDEN_ROW_ICONS {
        let (_, place) = find_placement(&children, icon)?;
        already_applied &= place.color.and_then(|color| color.mult).map(|mult| mult[3]) == Some(0);
    }
    let (_, name) = find_placement(&children, ROW_NAME)?;
    let (_, dimmed) = find_placement(&children, DIMMED_ROW_NAME)?;
    let dimmed_names_applied = dimmed.color.and_then(|color| color.mult).map(|mult| mult[3])
        == Some(DIMMED_ALPHA)
        && dimmed.matrix.map(|matrix| matrix.translate) == name.matrix.map(|matrix| matrix.translate);
    Some(AshRowsPlan {
        rows,
        sprite_index,
        base_y,
        base_scale,
        already_applied,
        dimmed_names_applied,
    })
}

const FIXED_ONE: i32 = 0x1_0000;

/// value * factor for a 16.16 fixed-point factor, rounded to nearest.
fn scale_fixed(value: i32, factor: i32) -> Option<i32> {
    let product = i64::from(value) * i64::from(factor);
    i32::try_from((product + product.signum() * 0x8000) / 0x1_0000).ok()
}

fn apply_ash_rows(tags: &mut [Tag], plan: &AshRowsPlan) -> Option<()> {
    // Hide the unused icons inside the row template sprite.
    let sprite = &tags[plan.sprite_index].body;
    let header = sprite.get(0..4)?.to_vec();
    let mut children = parse_tags(sprite, 4).ok()?;
    for icon in HIDDEN_ROW_ICONS {
        let (index, mut place) = find_placement(&children, icon)?;
        place.color = Some(ColorTransform::transparent(place.color));
        replace_placement(&mut children, index, &place)?;
    }
    let mut body = header;
    write_tags(&mut body, &children).ok()?;
    tags[plan.sprite_index].body = body;

    // Stack the rows from the Buddy panel, scaled down. Both are relative to
    // what the loaded movie has, so a HUD scale applied by another layout
    // is kept (vanilla: scale 1 -> 0.82, 45 px spacing).
    let spacing = scale_fixed(ROW_SPACING_TWIPS, plan.base_scale)?;
    let shift_x = scale_fixed(ROW_SHIFT_X_TWIPS, plan.base_scale)?;
    let base_y = plan
        .base_y
        .checked_add(scale_fixed(ROW_SHIFT_Y_TWIPS, plan.base_scale)?)?;
    for (row, (index, place)) in plan.rows.iter().enumerate() {
        let mut place = place.clone();
        let mut matrix = place.matrix?;
        let (scale_x, scale_y) = matrix.scale.unwrap_or((FIXED_ONE, FIXED_ONE));
        matrix.scale = Some((
            scale_fixed(scale_x, ROW_SCALE_FIXED)?,
            scale_fixed(scale_y, ROW_SCALE_FIXED)?,
        ));
        matrix.translate.1 = base_y.checked_add(spacing.checked_mul(row as i32)?)?;
        matrix.translate.0 = matrix.translate.0.checked_add(shift_x)?;
        place.matrix = Some(matrix);
        replace_placement(tags, *index, &place)?;
    }
    Some(())
}

/// Puts the row's role line (`RoleName`, the same text clip as `Name`) on the
/// name line at half opacity. The HUD writes a dismissed Ash's name there and
/// leaves `Name` empty, so the row reads dimmed; other rows leave it empty.
fn apply_dimmed_names(tags: &mut [Tag], sprite_index: usize) -> Option<()> {
    let sprite = &tags[sprite_index].body;
    let header = sprite.get(0..4)?.to_vec();
    let mut children = parse_tags(sprite, 4).ok()?;
    let (_, name) = find_placement(&children, ROW_NAME)?;
    let (index, mut dimmed) = find_placement(&children, DIMMED_ROW_NAME)?;
    let mut matrix = dimmed.matrix?;
    matrix.translate = name.matrix?.translate;
    dimmed.matrix = Some(matrix);
    dimmed.color = Some(ColorTransform::dimmed(dimmed.color));
    replace_placement(&mut children, index, &dimmed)?;
    let mut body = header;
    write_tags(&mut body, &children).ok()?;
    tags[sprite_index].body = body;
    Some(())
}

// ---------------------------------------------------------------------------
// Locating the game's Scaleform file types (works on a mapped image or on a
// file laid out as an image, so it can be checked offline)
// ---------------------------------------------------------------------------

pub(crate) struct Section<'a> {
    pub name: [u8; 8],
    pub rva: u32,
    pub data: &'a [u8],
    pub executable: bool,
}

pub(crate) struct ImageView<'a> {
    pub base: u64,
    pub sections: Vec<Section<'a>>,
}

impl ImageView<'_> {
    fn section_named(&self, name: &[u8]) -> Option<&Section<'_>> {
        self.sections.iter().find(|section| {
            section.name.starts_with(name) && section.name.get(name.len()) == Some(&0)
        })
    }

    fn read(&self, rva: u32, len: usize) -> Option<&[u8]> {
        self.sections.iter().find_map(|section| {
            let offset = rva.checked_sub(section.rva)? as usize;
            section.data.get(offset..offset.checked_add(len)?)
        })
    }

    fn read_u64(&self, rva: u32) -> Option<u64> {
        Some(u64::from_le_bytes(self.read(rva, 8)?.try_into().ok()?))
    }

    fn is_code(&self, rva: u32) -> bool {
        self.sections.iter().any(|section| {
            section.executable
                && rva >= section.rva
                && ((rva - section.rva) as usize) < section.data.len()
        })
    }

    fn rva_of_va(&self, va: u64) -> Option<u32> {
        u32::try_from(va.checked_sub(self.base)?).ok()
    }

    /// Primary vtable of a class by its MSVC RTTI decorated name.
    pub(crate) fn rtti_vtable(&self, decorated: &[u8]) -> Option<u32> {
        let mut descriptor = None;
        'sections: for section in self.sections.iter().filter(|section| !section.executable) {
            let data = section.data;
            let mut offset = 16;
            while offset + decorated.len() < data.len() {
                if data[offset] == decorated[0]
                    && &data[offset..offset + decorated.len()] == decorated
                    && data[offset + decorated.len()] == 0
                {
                    descriptor = Some(section.rva + offset as u32 - 16);
                    break 'sections;
                }
                offset += 1;
            }
        }
        let descriptor = descriptor?;
        let rdata = self.section_named(b".rdata")?;
        let read32 = |offset: usize| -> Option<u32> {
            Some(u32::from_le_bytes(
                rdata.data.get(offset..offset + 4)?.try_into().ok()?,
            ))
        };
        let mut locator = None;
        let mut offset = 0;
        while offset + 24 <= rdata.data.len() {
            let rva = rdata.rva + offset as u32;
            if read32(offset) == Some(1)
                && read32(offset + 4) == Some(0)
                && read32(offset + 12) == Some(descriptor)
                && read32(offset + 20) == Some(rva)
            {
                locator = Some(rva);
                break;
            }
            offset += 4;
        }
        let locator_va = self.base + u64::from(locator?);
        let mut offset = 0;
        while offset + 16 <= rdata.data.len() {
            if rdata.data[offset..offset + 8] == locator_va.to_le_bytes() {
                return Some(rdata.rva + offset as u32 + 8);
            }
            offset += 8;
        }
        None
    }
}

/// `CSScaleformFileOpener::OpenFile` entry: four whole push instructions
/// (push rbp; push rbx; push rsi; push rdi) that the hook relocates, followed
/// by the rest of the verified prologue.
pub(crate) const OPEN_FILE_PROLOGUE: [u8; 16] = [
    0x40, 0x55, 0x53, 0x56, 0x57, 0x41, 0x54, 0x41, 0x56, 0x41, 0x57, 0x48, 0x8d, 0x6c, 0x24, 0xd9,
];
pub(crate) const OPEN_FILE_RELOCATED: usize = 5;
// Scaleform::File slots and the MemoryFile fields their code reads.
const SLOT_IS_VALID: usize = 2;
const SLOT_TELL: usize = 4;
const SLOT_GET_LENGTH: usize = 6;
const IS_VALID_CODE: [u8; 5] = [0x0f, 0xb6, 0x41, 0x28, 0xc3]; // movzx eax, byte [rcx+0x28]
const TELL_CODE: [u8; 4] = [0x8b, 0x41, 0x24, 0xc3]; // mov eax, [rcx+0x24]
const GET_LENGTH_CODE: [u8; 4] = [0x8b, 0x41, 0x20, 0xc3]; // mov eax, [rcx+0x20]
const MEMORY_FILE_BUFFER: usize = 0x18;
const MEMORY_FILE_SIZE: usize = 0x20;
const MEMORY_FILE_INDEX: usize = 0x24;
const MEMORY_FILE_VALID: usize = 0x28;
const MAX_MOVIE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScaleformTargets {
    pub open_file: u32,
    pub memory_file_vtable: u32,
    /// The entry already starts with a rel32 jump (another hook).
    pub open_file_hooked: bool,
}

pub(crate) fn locate_scaleform_targets(
    image: &ImageView<'_>,
) -> Result<ScaleformTargets, &'static str> {
    let opener = image
        .rtti_vtable(b".?AVCSScaleformFileOpener@CS@@")
        .ok_or("CSScaleformFileOpener RTTI not found")?;
    let memory_file = image
        .rtti_vtable(b".?AVMemoryFile@Scaleform@@")
        .ok_or("Scaleform::MemoryFile RTTI not found")?;
    let slot = |vtable: u32, index: usize| -> Option<u32> {
        image.rva_of_va(image.read_u64(vtable + 8 * index as u32)?)
    };
    for (index, code) in [
        (SLOT_IS_VALID, &IS_VALID_CODE[..]),
        (SLOT_TELL, &TELL_CODE[..]),
        (SLOT_GET_LENGTH, &GET_LENGTH_CODE[..]),
    ] {
        let target = slot(memory_file, index).ok_or("MemoryFile slot outside the executable")?;
        if !image.is_code(target) || image.read(target, code.len()) != Some(code) {
            return Err("MemoryFile field layout differs from the verified one");
        }
    }
    let open_file = slot(opener, 1).ok_or("OpenFile slot outside the executable")?;
    if !image.is_code(open_file) {
        return Err("OpenFile is not code");
    }
    let entry = image
        .read(open_file, OPEN_FILE_PROLOGUE.len())
        .ok_or("OpenFile unreadable")?;
    let open_file_hooked = entry[0] == 0xe9;
    if !open_file_hooked && entry != OPEN_FILE_PROLOGUE {
        return Err("OpenFile prologue differs from the verified one");
    }
    if open_file % 8 != 0 {
        return Err("OpenFile entry is not 8-byte aligned");
    }
    Ok(ScaleformTargets {
        open_file,
        memory_file_vtable: memory_file,
        open_file_hooked,
    })
}

// ---------------------------------------------------------------------------
// Runtime hook
// ---------------------------------------------------------------------------

type OpenFileFn = unsafe extern "system" fn(*mut c_void, *const u8, i32, i32) -> *mut c_void;

static INSTALLED: AtomicBool = AtomicBool::new(false);
static ORIGINAL_OPEN_FILE: AtomicUsize = AtomicUsize::new(0);
static MEMORY_FILE_VTABLE: AtomicUsize = AtomicUsize::new(0);
/// The loaded HUD movie has the dimmed name line (see `apply_dimmed_names`).
pub(crate) static DIMMED_NAMES_READY: AtomicBool = AtomicBool::new(false);
/// Patched movies by input hash; served buffers stay alive for the process.
static PATCHED: LazyLock<Mutex<HashMap<(u64, usize), Option<&'static [u8]>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn url_is_hud_movie(url: *const u8) -> bool {
    if url.is_null() {
        return false;
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(url.cast()) }.to_bytes();
    let name = HUD_MOVIE_FILE.as_bytes();
    bytes.len() >= name.len()
        && bytes[bytes.len() - name.len()..].eq_ignore_ascii_case(name)
        && (bytes.len() == name.len()
            || matches!(bytes[bytes.len() - name.len() - 1], b'/' | b'\\' | b':'))
}

unsafe extern "system" fn open_file_detour(
    opener: *mut c_void,
    url: *const u8,
    flags: i32,
    mode: i32,
) -> *mut c_void {
    let original: OpenFileFn =
        unsafe { std::mem::transmute(ORIGINAL_OPEN_FILE.load(Ordering::Acquire)) };
    let file = unsafe { original(opener, url, flags, mode) };
    if file.is_null() || !url_is_hud_movie(url) {
        return file;
    }
    let _ = std::panic::catch_unwind(|| unsafe { patch_open_movie(file) });
    file
}

unsafe fn read_field<T: Copy>(object: *mut c_void, offset: usize) -> T {
    unsafe { object.cast::<u8>().add(offset).cast::<T>().read_unaligned() }
}

unsafe fn write_field<T: Copy>(object: *mut c_void, offset: usize, value: T) {
    unsafe {
        object
            .cast::<u8>()
            .add(offset)
            .cast::<T>()
            .write_unaligned(value)
    }
}

/// Swaps the movie bytes of a MemoryFile (or of a wrapper that reuses the
/// MemoryFile accessors, i.e. the same field layout). Any other file type is
/// left alone.
unsafe fn patch_open_movie(file: *mut c_void) {
    let memory_vtable = MEMORY_FILE_VTABLE.load(Ordering::Acquire) as *const usize;
    let vtable = unsafe { read_field::<*const usize>(file, 0) };
    let same_layout = vtable == memory_vtable
        || [SLOT_IS_VALID, SLOT_TELL, SLOT_GET_LENGTH]
            .iter()
            .all(|&slot| unsafe { *vtable.add(slot) == *memory_vtable.add(slot) });
    if !same_layout {
        super::log_line("HUD movie: opened through an unknown file type; served unchanged");
        return;
    }
    if unsafe { read_field::<u8>(file, MEMORY_FILE_VALID) } == 0 {
        return;
    }
    let buffer = unsafe { read_field::<*const u8>(file, MEMORY_FILE_BUFFER) };
    let size = unsafe { read_field::<i32>(file, MEMORY_FILE_SIZE) };
    if buffer.is_null() || size <= 0 || size as usize > MAX_MOVIE_BYTES {
        return;
    }
    let input = unsafe { std::slice::from_raw_parts(buffer, size as usize) };
    let key = (fnv1a(input), input.len());
    let Ok(mut cache) = PATCHED.lock() else {
        return;
    };
    let output = *cache.entry(key).or_insert_with(|| match patch_hud_movie(input) {
        Ok((Some(bytes), report)) => {
            super::log_line(format_args!(
                "HUD movie patched in memory ({} -> {} bytes): buddy_panel_hidden={} ash_rows={} dimmed_names={}",
                input.len(),
                bytes.len(),
                report.buddy_panel_hidden,
                report.ash_rows,
                report.dimmed_names
            ));
            DIMMED_NAMES_READY.store(report.dimmed_names, Ordering::Release);
            Some(Box::leak(bytes.into_boxed_slice()))
        }
        Ok((None, report)) => {
            super::log_line(format_args!(
                "HUD movie already has the changes it can take: buddy_panel_hidden={} ash_rows={} dimmed_names={}",
                report.buddy_panel_hidden, report.ash_rows, report.dimmed_names
            ));
            DIMMED_NAMES_READY.store(report.dimmed_names, Ordering::Release);
            None
        }
        Err(reason) => {
            super::log_line(format_args!("HUD movie served unchanged: {reason}"));
            None
        }
    });
    if let Some(bytes) = output {
        unsafe {
            write_field(file, MEMORY_FILE_BUFFER, bytes.as_ptr());
            write_field(file, MEMORY_FILE_SIZE, bytes.len() as i32);
            write_field(file, MEMORY_FILE_INDEX, 0i32);
        }
    }
}

/// Builds an [`ImageView`] over the running executable's readable sections.
fn running_image() -> Option<ImageView<'static>> {
    use windows::Win32::System::{
        LibraryLoader::GetModuleHandleW,
        Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery},
    };
    let base = unsafe { GetModuleHandleW(None) }.ok()?.0 as usize;
    let readable = |address: usize, len: usize| -> bool {
        let mut cursor = address;
        while cursor < address + len {
            let mut info = MEMORY_BASIC_INFORMATION::default();
            let written = unsafe {
                VirtualQuery(
                    Some(cursor as *const c_void),
                    &mut info,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if written == 0
                || info.State != MEM_COMMIT
                || info.Protect.0 & (PAGE_NOACCESS.0 | PAGE_GUARD.0) != 0
            {
                return false;
            }
            cursor = info.BaseAddress as usize + info.RegionSize;
        }
        true
    };
    if !readable(base, 0x1000) {
        return None;
    }
    let headers = unsafe { std::slice::from_raw_parts(base as *const u8, 0x1000) };
    let pe = read_u32(headers, 0x3c)? as usize;
    let count = usize::from(read_u16(headers, pe + 6)?);
    let optional = usize::from(read_u16(headers, pe + 20)?);
    let mut sections = Vec::new();
    for index in 0..count {
        let entry = pe + 24 + optional + index * 40;
        let name: [u8; 8] = headers.get(entry..entry + 8)?.try_into().ok()?;
        let virtual_size = read_u32(headers, entry + 8)? as usize;
        let rva = read_u32(headers, entry + 12)?;
        let characteristics = read_u32(headers, entry + 36)?;
        let start = base + rva as usize;
        if virtual_size == 0 || !readable(start, virtual_size) {
            continue;
        }
        sections.push(Section {
            name,
            rva,
            data: unsafe { std::slice::from_raw_parts(start as *const u8, virtual_size) },
            executable: characteristics & 0x2000_0000 != 0,
        });
    }
    Some(ImageView {
        base: base as u64,
        sections,
    })
}

/// Installs the OpenFile hook. Call as early as possible: a HUD movie opened
/// before this is served unchanged. Returns whether the hook is active.
pub(crate) fn install() -> bool {
    use windows::Win32::System::{
        Diagnostics::Debug::FlushInstructionCache,
        Memory::{PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualProtect},
        Threading::GetCurrentProcess,
    };
    if INSTALLED.load(Ordering::Acquire) {
        return true;
    }
    let Some(image) = running_image() else {
        super::log_line(
            "HUD movie hook: executable image unreadable; menu/01_000_fe.gfx served unchanged",
        );
        return false;
    };
    let targets = match locate_scaleform_targets(&image) {
        Ok(targets) => targets,
        Err(reason) => {
            super::log_line(format_args!(
                "HUD movie hook not installed: {reason}; menu/01_000_fe.gfx served unchanged"
            ));
            return false;
        }
    };
    let base = image.base as usize;
    let entry = base + targets.open_file as usize;
    let Some(page) = (unsafe { super::allocate_near(entry) }) else {
        super::log_line("HUD movie hook: no executable page within rel32 range");
        return false;
    };
    let page = page.cast::<u8>();
    let Ok(relative) = i32::try_from(page as isize - (entry as isize + 5)) else {
        super::log_line("HUD movie hook: relay page outside rel32 range");
        return false;
    };
    // page+0x00: jmp [rip+0] -> detour
    // page+0x20: relocated entry bytes, then jmp [rip+0] -> entry+5
    let detour = open_file_detour as *const () as u64;
    let mut relay = vec![0xff, 0x25, 0, 0, 0, 0];
    relay.extend_from_slice(&detour.to_le_bytes());
    unsafe {
        std::ptr::copy_nonoverlapping(relay.as_ptr(), page, relay.len());
    }
    MEMORY_FILE_VTABLE.store(
        base + targets.memory_file_vtable as usize,
        Ordering::Release,
    );

    let mut old = PAGE_PROTECTION_FLAGS(0);
    if unsafe { VirtualProtect(entry as *const c_void, 8, PAGE_EXECUTE_READWRITE, &mut old) }
        .is_err()
    {
        super::log_line("HUD movie hook: OpenFile entry could not be made writable");
        return false;
    }
    // Another mod may hook the same entry at the same moment: write with a
    // compare-and-swap and rebuild the chain from the bytes actually there.
    let slot = unsafe { &*(entry as *const std::sync::atomic::AtomicU64) };
    let mut chained = false;
    let mut installed = false;
    for _ in 0..16 {
        let current = slot.load(Ordering::SeqCst).to_le_bytes();
        let continuation = if current[0] == 0xe9 {
            // Chain behind the existing hook: call wherever its jump goes.
            chained = true;
            let next = i32::from_le_bytes(current[1..5].try_into().unwrap_or_default());
            (entry as isize + 5 + next as isize) as usize
        } else if current[..OPEN_FILE_RELOCATED] == OPEN_FILE_PROLOGUE[..OPEN_FILE_RELOCATED] {
            chained = false;
            let mut trampoline = current[..OPEN_FILE_RELOCATED].to_vec();
            trampoline.extend_from_slice(&[0xff, 0x25, 0, 0, 0, 0]);
            trampoline.extend_from_slice(&((entry + OPEN_FILE_RELOCATED) as u64).to_le_bytes());
            unsafe {
                std::ptr::copy_nonoverlapping(
                    trampoline.as_ptr(),
                    page.add(0x20),
                    trampoline.len(),
                );
            }
            page as usize + 0x20
        } else {
            break;
        };
        unsafe {
            let _ = FlushInstructionCache(GetCurrentProcess(), Some(page.cast()), 0x40);
        }
        // Set before the jump goes live: the detour can run right after the swap.
        ORIGINAL_OPEN_FILE.store(continuation, Ordering::Release);
        let mut patched = current;
        patched[0] = 0xe9;
        patched[1..5].copy_from_slice(&relative.to_le_bytes());
        if slot
            .compare_exchange(
                u64::from_le_bytes(current),
                u64::from_le_bytes(patched),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            installed = true;
            break;
        }
    }
    unsafe {
        let _ = FlushInstructionCache(GetCurrentProcess(), Some(entry as *const c_void), 8);
        let mut ignored = PAGE_PROTECTION_FLAGS(0);
        let _ = VirtualProtect(entry as *const c_void, 8, old, &mut ignored);
    }
    if !installed {
        super::log_line("HUD movie hook: OpenFile entry changed to unknown bytes; not hooked");
        return false;
    }
    INSTALLED.store(true, Ordering::Release);
    super::log_line(format_args!(
        "HUD movie hook installed: OpenFile rva={:#x}{} MemoryFile vtable rva={:#x}",
        targets.open_file,
        if chained {
            " (chained behind an existing hook)"
        } else {
            ""
        },
        targets.memory_file_vtable
    ));
    true
}

// ---------------------------------------------------------------------------
// Offline checks (run with `cargo test`; files are read from the paths in the
// MSA_TEST_* environment variables or the default local paths, and a missing
// file skips its check)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{env, fs, path::PathBuf};

    pub(crate) fn file_image(bytes: &[u8]) -> ImageView<'_> {
        let pe = read_u32(bytes, 0x3c).unwrap() as usize;
        let count = usize::from(read_u16(bytes, pe + 6).unwrap());
        let optional = usize::from(read_u16(bytes, pe + 20).unwrap());
        let base = u64::from_le_bytes(bytes[pe + 24 + 24..pe + 24 + 32].try_into().unwrap());
        let mut sections = Vec::new();
        for index in 0..count {
            let entry = pe + 24 + optional + index * 40;
            let name: [u8; 8] = bytes[entry..entry + 8].try_into().unwrap();
            let virtual_size = read_u32(bytes, entry + 8).unwrap() as usize;
            let rva = read_u32(bytes, entry + 12).unwrap();
            let raw_size = read_u32(bytes, entry + 16).unwrap() as usize;
            let raw = read_u32(bytes, entry + 20).unwrap() as usize;
            let characteristics = read_u32(bytes, entry + 36).unwrap();
            let len = virtual_size.min(raw_size);
            sections.push(Section {
                name,
                rva,
                data: &bytes[raw..raw + len],
                executable: characteristics & 0x2000_0000 != 0,
            });
        }
        ImageView { base, sections }
    }

    fn path(var: &str, default: &str) -> PathBuf {
        env::var_os(var).map_or_else(|| PathBuf::from(default), PathBuf::from)
    }

    fn load(var: &str, default: &str) -> Option<Vec<u8>> {
        let path = path(var, default);
        match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(_) => {
                eprintln!("skipped: {} not found", path.display());
                None
            }
        }
    }

    /// (label, folder under MSA_TEST_EXE_DIR or test-gfx\exe holding that eldenring.exe,
    /// expected OpenFile RVA, expected MemoryFile vtable RVA)
    const EXES: [(&str, &str, u32, u32); 5] = [
        (
            "1.17 WW 2.7.0.0",
            "2.7.0.0",
            0xd6d210,
            0x2ba7d70,
        ),
        (
            "1.17.1 WW 2.7.1.0",
            "2.7.1.0",
            0xd6d280,
            0x2ba7d70,
        ),
        (
            "1.17.2 WW 2.7.2.0",
            "2.7.2.0",
            0xd6e970,
            0x2baa020,
        ),
        (
            "JP 2.7.1.1",
            "2.7.1.1-jp",
            0xd6d280,
            0x2ba7d70,
        ),
        (
            "Steam (current)",
            "current",
            0xd6e970,
            0x2baa020,
        ),
    ];

    #[test]
    fn scaleform_targets_resolve_on_every_executable() {
        let root = std::env::var("MSA_TEST_EXE_DIR")
            .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), r"\test-gfx\exe").to_owned());
        for (label, folder, open_file, memory_file) in EXES {
            let exe = format!(r"{root}\{folder}\eldenring.exe");
            let Some(bytes) = load("MSA_TEST_EXE_NONE", &exe) else {
                continue;
            };
            let targets = locate_scaleform_targets(&file_image(&bytes))
                .unwrap_or_else(|error| panic!("{label}: {error}"));
            eprintln!("{label}: {targets:x?}");
            assert_eq!(targets.open_file, open_file, "{label}");
            assert_eq!(targets.memory_file_vtable, memory_file, "{label}");
            assert!(!targets.open_file_hooked, "{label}");
        }
    }

    fn placement(movie: &[u8], name: &[u8]) -> PlaceObject2 {
        let header = 8 + (5 + 4 * usize::from(movie[8] >> 3)).div_ceil(8) + 4;
        let tags = parse_tags(&movie[..read_u32(movie, 4).unwrap() as usize], header).unwrap();
        find_placement(&tags, name).unwrap().1
    }

    fn sprite_placement(movie: &[u8], sprite: u16, name: &[u8]) -> PlaceObject2 {
        let header = 8 + (5 + 4 * usize::from(movie[8] >> 3)).div_ceil(8) + 4;
        let tags = parse_tags(&movie[..read_u32(movie, 4).unwrap() as usize], header).unwrap();
        let tag = tags
            .iter()
            .find(|tag| tag.code == TAG_DEFINE_SPRITE && read_u16(&tag.body, 0) == Some(sprite))
            .unwrap();
        let children = parse_tags(&tag.body, 4).unwrap();
        find_placement(&children, name).unwrap().1
    }

    /// `shift_y`: the new block offset; 0 for a movie whose rows were placed
    /// by the old Alpha 1 package (left as it is, not patched twice).
    fn assert_msa_layout(movie: &[u8], shift_y: i32) {
        let alpha0 =
            |place: &PlaceObject2| place.color.and_then(|c| c.mult).map(|m| m[3]) == Some(0);
        let buddy = placement(movie, BUDDY_PANEL);
        assert!(alpha0(&buddy));
        let base = buddy.matrix.unwrap().translate.1;
        let mut sprite = None;
        for row in 0..ROW_COUNT {
            let place = placement(movie, format!("RemotePCTag{row}").as_bytes());
            let matrix = place.matrix.unwrap();
            assert_eq!(matrix.scale, Some((ROW_SCALE_FIXED, ROW_SCALE_FIXED)));
            assert_eq!(
                matrix.translate.1,
                base + shift_y + ROW_SPACING_TWIPS * row as i32
            );
            sprite = place.character;
        }
        for icon in HIDDEN_ROW_ICONS {
            assert!(alpha0(&sprite_placement(movie, sprite.unwrap(), icon)));
        }
        let name = sprite_placement(movie, sprite.unwrap(), ROW_NAME);
        assert!(!alpha0(&name));
        assert!(!alpha0(&sprite_placement(movie, sprite.unwrap(), b"HP")));
        let dimmed = sprite_placement(movie, sprite.unwrap(), DIMMED_ROW_NAME);
        assert_eq!(dimmed.color.and_then(|c| c.mult).map(|m| m[3]), Some(DIMMED_ALPHA));
        assert_eq!(
            dimmed.matrix.unwrap().translate,
            name.matrix.unwrap().translate
        );
    }

    // Game and third-party movies are not part of the repository: copy them
    // into test-gfx\ (ignored by git) or point the MSA_TEST_GFX_* variables at
    // them. Tests whose movie is missing are skipped.
    pub(crate) const VANILLA: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), r"\test-gfx\vanilla\01_000_fe.gfx");
    pub(crate) const OTHER_HUD: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), r"\test-gfx\other-hud\01_000_fe.gfx");
    const OLD_PACKAGE: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), r"\test-gfx\msa-0.1.56\01_000_fe.gfx");

    #[test]
    fn vanilla_movie_gets_the_layout_and_is_idempotent() {
        let Some(vanilla) = load("MSA_TEST_GFX_VANILLA", VANILLA) else {
            return;
        };
        let (patched, report) = patch_hud_movie(&vanilla).unwrap();
        let patched = patched.unwrap();
        assert!(report.buddy_panel_hidden && report.ash_rows && report.dimmed_names && report.changed);
        assert_msa_layout(&patched, ROW_SHIFT_Y_TWIPS);
        // Only the patched tags changed size.
        assert!(patched.len() > vanilla.len() && patched.len() < vanilla.len() + 200);
        let (again, _) = patch_hud_movie(&patched).unwrap();
        assert!(again.is_none(), "second pass must not change anything");
        // Archive padding after the declared length is accepted and dropped.
        let mut padded = vanilla.clone();
        padded.extend_from_slice(&[0; 12]);
        assert_eq!(patch_hud_movie(&padded).unwrap().0.unwrap(), patched);
    }

    #[test]
    fn another_hud_mods_movie_keeps_its_own_changes() {
        let (Some(vanilla), Some(other)) = (
            load("MSA_TEST_GFX_VANILLA", VANILLA),
            load("MSA_TEST_GFX_OTHER", OTHER_HUD),
        ) else {
            return;
        };
        let patched = patch_hud_movie(&other).unwrap().0.unwrap();
        assert_msa_layout(&patched, ROW_SHIFT_Y_TWIPS);
        // Every tag the other mod changed relative to vanilla survives.
        let header = |m: &[u8]| 8 + (5 + 4 * usize::from(m[8] >> 3)).div_ceil(8) + 4;
        let tags =
            |m: &[u8]| parse_tags(&m[..read_u32(m, 4).unwrap() as usize], header(m)).unwrap();
        let (v, o, p) = (tags(&vanilla), tags(&other), tags(&patched));
        assert_eq!(v.len(), o.len());
        assert_eq!(o.len(), p.len());
        let ours = tags(&patch_hud_movie(&vanilla).unwrap().0.unwrap());
        for index in 0..o.len() {
            if o[index] != v[index] {
                assert_eq!(
                    p[index], o[index],
                    "tag {index} of the other mod was altered"
                );
            } else {
                assert_eq!(p[index], ours[index], "tag {index}");
            }
        }
    }

    #[test]
    fn a_scaled_hud_layout_is_kept() {
        let Some(vanilla) = load("MSA_TEST_GFX_VANILLA", VANILLA) else {
            return;
        };
        // Simulate another layout that scaled the Buddy panel and the rows by 0.8.
        let header = 8 + (5 + 4 * usize::from(vanilla[8] >> 3)).div_ceil(8) + 4;
        let mut tags =
            parse_tags(&vanilla[..read_u32(&vanilla, 4).unwrap() as usize], header).unwrap();
        let factor = 52_429; // 0.8
        let mut names = vec![BUDDY_PANEL.to_vec()];
        names.extend((0..ROW_COUNT).map(|row| format!("RemotePCTag{row}").into_bytes()));
        for name in &names {
            let (index, mut place) = find_placement(&tags, name).unwrap();
            let mut matrix = place.matrix.unwrap();
            matrix.scale = Some((factor, factor));
            place.matrix = Some(matrix);
            replace_placement(&mut tags, index, &place).unwrap();
        }
        let mut scaled = vanilla[..header].to_vec();
        write_tags(&mut scaled, &tags).unwrap();
        let total = scaled.len() as u32;
        scaled[4..8].copy_from_slice(&total.to_le_bytes());

        let patched = patch_hud_movie(&scaled).unwrap().0.unwrap();
        let base = placement(&patched, BUDDY_PANEL).matrix.unwrap().translate.1;
        let spacing = scale_fixed(ROW_SPACING_TWIPS, factor).unwrap();
        for row in 0..ROW_COUNT {
            let matrix = placement(&patched, format!("RemotePCTag{row}").as_bytes())
                .matrix
                .unwrap();
            let expected = scale_fixed(factor, ROW_SCALE_FIXED).unwrap();
            assert_eq!(matrix.scale, Some((expected, expected)));
            let shift = scale_fixed(ROW_SHIFT_Y_TWIPS, factor).unwrap();
            assert_eq!(matrix.translate.1, base + shift + spacing * row as i32);
        }
        assert!(patch_hud_movie(&patched).unwrap().0.is_none());
    }

    #[test]
    fn previous_package_movie_is_accepted() {
        let Some(old) = load("MSA_TEST_GFX_OLD", OLD_PACKAGE) else {
            return;
        };
        let (patched, _) = patch_hud_movie(&old).unwrap();
        assert_msa_layout(patched.as_deref().unwrap_or(&old), 0);
    }

    #[test]
    fn unreadable_movies_are_refused() {
        assert!(patch_hud_movie(b"CFX\x0b\x10\x00\x00\x00").is_err());
        let Some(vanilla) = load("MSA_TEST_GFX_VANILLA", VANILLA) else {
            return;
        };
        let mut truncated = vanilla[..vanilla.len() / 2].to_vec();
        truncated[4..8].copy_from_slice(&(vanilla.len() as u32).to_le_bytes());
        assert!(patch_hud_movie(&truncated).is_err());
        // A movie without the named elements is left unchanged.
        let mut renamed = vanilla.clone();
        for window in 0..renamed.len() - 6 {
            if &renamed[window..window + 6] == b"Buddy\0" {
                renamed[window] = b'X';
            }
        }
        let (output, report) = patch_hud_movie(&renamed).unwrap();
        assert!(!report.buddy_panel_hidden && !report.ash_rows && !report.dimmed_names);
        assert!(output.is_none());
    }

    #[test]
    fn bit_codecs_round_trip() {
        for matrix in [
            Matrix {
                scale: None,
                rotate: None,
                translate: (0, 0),
            },
            Matrix {
                scale: Some((ROW_SCALE_FIXED, ROW_SCALE_FIXED)),
                rotate: None,
                translate: (3939, 11220),
            },
            Matrix {
                scale: Some((-52_428, 52_428)),
                rotate: Some((1, -1)),
                translate: (-1700, -520),
            },
        ] {
            let mut out = Vec::new();
            matrix.write(&mut out).unwrap();
            assert_eq!(Matrix::read(&out, 0).unwrap().0, matrix);
        }
        for color in [
            ColorTransform::transparent(None),
            ColorTransform::dimmed(None),
        ] {
            let mut out = Vec::new();
            color.write(&mut out).unwrap();
            assert_eq!(ColorTransform::read(&out, 0).unwrap().0, color);
        }
    }
}

#[cfg(test)]
mod dump {
    /// `MSA_DUMP_DIR=<dir> cargo test dump_patched_movies` writes the patched
    /// movies for inspection with research/gfx_place.py.
    #[test]
    fn dump_patched_movies() {
        let Some(dir) = std::env::var_os("MSA_DUMP_DIR") else {
            return;
        };
        for (name, source) in [
            ("vanilla", super::tests::VANILLA),
            ("other_hud", super::tests::OTHER_HUD),
        ] {
            let Ok(input) = std::fs::read(source) else {
                continue;
            };
            let output = super::patch_hud_movie(&input).unwrap().0.unwrap();
            std::fs::write(
                std::path::Path::new(&dir).join(format!("{name}-patched.gfx")),
                output,
            )
            .unwrap();
        }
    }
}
